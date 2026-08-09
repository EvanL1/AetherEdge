//! Tear-resistant snapshot serialization independent from the live mmap ABI.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::core::header::{calculate_file_size, slot_offset};
use crate::core::slot::{PointSlot, SLOT_UNWRITTEN_BITS};
use crate::core::snapshot_format::{
    SLOT_ABSENT, SLOT_PAYLOAD_SIZE, SLOT_PRESENT, SNAPSHOT_HEADER_SIZE, SnapshotHeader,
};
use crate::{DataplaneError, DataplaneResult};

/// Writes one atomic snapshot without flushing the live mmap.
///
/// Each slot is captured through its seqlock. A concurrently-mutating slot is
/// recorded as absent rather than persisting torn data. The staging inode is
/// flushed before rename and the containing directory is flushed afterwards,
/// making the replacement durable across a crash once this function returns.
pub(crate) fn save_snapshot_impl(
    mmap_data: &[u8],
    slot_count: usize,
    layout_hash: u64,
    path: &Path,
    label: &str,
) -> DataplaneResult<()> {
    let physical_slot_count = u32::try_from(slot_count).map_err(|_| {
        DataplaneError::InvalidLayout(format!("snapshot slot_count {slot_count} exceeds u32::MAX"))
    })?;
    let required_len = calculate_file_size(physical_slot_count);
    if mmap_data.len() != required_len {
        return Err(DataplaneError::InvalidLayout(format!(
            "snapshot source mmap length mismatch: have {} bytes, need {required_len} for slot_count={slot_count}",
            mmap_data.len()
        )));
    }

    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|source| {
        DataplaneError::io(format!("create snapshot directory {parent:?}"), source)
    })?;

    let staging_path = staging_path(path);
    let file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&staging_path)
        .map_err(|source| {
            DataplaneError::io(
                format!("create temporary snapshot {staging_path:?}"),
                source,
            )
        })?;
    let mut cleanup = StagingCleanup::new(staging_path.clone());
    let mut output = BufWriter::new(file);
    let header = SnapshotHeader::new(physical_slot_count, layout_hash);
    output
        .write_all(&header.encode())
        .map_err(|source| DataplaneError::io("write snapshot header", source))?;

    // SAFETY: exact source length was checked above; the live slot array begins
    // at a 32-byte-aligned offset and contains exactly `slot_count` PointSlots.
    let slots_ptr = unsafe { mmap_data.as_ptr().add(slot_offset()) as *const PointSlot };
    let mut torn = 0_usize;
    let mut absent = 0_usize;
    let mut data_size = SNAPSHOT_HEADER_SIZE;
    for slot_index in 0..slot_count {
        // SAFETY: slot_index is bounded by the validated physical slot count.
        let slot = unsafe { &*slots_ptr.add(slot_index) };
        match slot.try_load_consistent() {
            None => {
                torn += 1;
                absent += 1;
                output
                    .write_all(&[SLOT_ABSENT])
                    .map_err(|source| DataplaneError::io("write absent snapshot slot", source))?;
                data_size += 1;
            },
            Some((value, raw, _timestamp_ms, _quality_code))
                if value.to_bits() == SLOT_UNWRITTEN_BITS
                    && raw.to_bits() == SLOT_UNWRITTEN_BITS =>
            {
                absent += 1;
                output
                    .write_all(&[SLOT_ABSENT])
                    .map_err(|source| DataplaneError::io("write absent snapshot slot", source))?;
                data_size += 1;
            },
            Some((value, raw, timestamp_ms, quality_code)) => {
                if !value.is_finite() || !raw.is_finite() {
                    return Err(DataplaneError::InvalidLayout(format!(
                        "cannot snapshot slot {slot_index} with non-finite present data"
                    )));
                }
                let record = present_slot_bytes(value, raw, timestamp_ms, quality_code);
                output
                    .write_all(&record)
                    .map_err(|source| DataplaneError::io("write present snapshot slot", source))?;
                data_size += record.len();
            },
        }
    }

    output
        .flush()
        .map_err(|source| DataplaneError::io("flush buffered snapshot", source))?;
    output
        .get_ref()
        .sync_all()
        .map_err(|source| DataplaneError::io("sync temporary snapshot file", source))?;
    drop(output);

    std::fs::rename(&staging_path, path).map_err(|source| {
        DataplaneError::io(
            format!("rename temporary snapshot {staging_path:?} to {path:?}"),
            source,
        )
    })?;
    cleanup.commit();
    sync_parent_directory(parent)?;

    if torn > 0 {
        tracing::warn!(
            "{} snapshot saved with {} contended slot(s) recorded absent: {:?}, size={} bytes, slots={}, absent={}",
            label,
            torn,
            path,
            data_size,
            slot_count,
            absent
        );
    } else {
        tracing::info!(
            "{} snapshot saved: {:?}, size={} bytes, slots={}, absent={}",
            label,
            path,
            data_size,
            slot_count,
            absent
        );
    }
    Ok(())
}

fn present_slot_bytes(
    value: f64,
    raw: f64,
    timestamp_ms: u64,
    quality_code: u32,
) -> [u8; 1 + SLOT_PAYLOAD_SIZE] {
    let mut bytes = [0_u8; 1 + SLOT_PAYLOAD_SIZE];
    bytes[0] = SLOT_PRESENT;
    bytes[1..9].copy_from_slice(&value.to_bits().to_le_bytes());
    bytes[9..17].copy_from_slice(&raw.to_bits().to_le_bytes());
    bytes[17..25].copy_from_slice(&timestamp_ms.to_le_bytes());
    bytes[25..29].copy_from_slice(&quality_code.to_le_bytes());
    bytes
}

fn staging_path(path: &Path) -> PathBuf {
    let mut path_with_suffix: OsString = path.as_os_str().to_owned();
    path_with_suffix.push(".tmp");
    PathBuf::from(path_with_suffix)
}

fn sync_parent_directory(parent: &Path) -> DataplaneResult<()> {
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| DataplaneError::io(format!("sync snapshot directory {parent:?}"), source))
}

struct StagingCleanup {
    path: PathBuf,
    committed: bool,
}

impl StagingCleanup {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            committed: false,
        }
    }

    fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for StagingCleanup {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
