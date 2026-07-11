//! Tear-resistant SHM snapshot serialization (writer side).
//!
//! Pure-infra: takes raw mmap bytes + slot count, produces a snapshot
//! file at the given path. Knows nothing about channels, point types,
//! instances, or routing — the restore-side adapter (in `unified_shm`)
//! is the one that re-derives business semantics from the byte stream.
//!
//! # Why per-slot seqlock-aware serialization
//!
//! Earlier versions did a raw `memcpy` of the mmap region. If the
//! writer was mid-update through a seqlock at snapshot time, the
//! snapshot captured torn bytes (new value + old raw, or seq=odd
//! mid-write) and preserved them across restart — a stale or
//! impossible reading would then be restored to SHM and propagate
//! through Redis until overwritten by the next live write.
//!
//! Now each slot is read via `try_load_consistent()`. Torn reads
//! (writer concurrently mid-update) become unwritten-NaN sentinels in
//! the snapshot file. The header bytes are still copied verbatim;
//! header atomics are byte-stable so a `memcpy` of a 64-byte aligned
//! region is safe.

use crate::core::header::slot_offset;
use crate::core::slot::{PointSlot, SLOT_UNWRITTEN_BITS};
use crate::{DataplaneError, DataplaneResult};

/// Write a SHM mmap region as a snapshot file, using tear-resistant
/// per-slot serialization.
///
/// The file is written atomically: data is first written to
/// `<path>.tmp` and then renamed.
pub(crate) fn save_snapshot_impl(
    mmap_data: &[u8],
    slot_count: usize,
    path: &std::path::Path,
    label: &str,
) -> DataplaneResult<()> {
    use std::io::Write;

    let required_len = slot_count
        .checked_mul(std::mem::size_of::<PointSlot>())
        .and_then(|slots_len| slot_offset().checked_add(slots_len))
        .ok_or_else(|| {
            DataplaneError::InvalidLayout(format!(
                "snapshot length overflow for slot_count={slot_count}"
            ))
        })?;
    if mmap_data.len() < required_len {
        return Err(DataplaneError::InvalidLayout(format!(
            "snapshot source mmap too small: len={} slot_count={slot_count}",
            mmap_data.len()
        )));
    }

    let temp_path = path.with_extension("tmp");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| {
            DataplaneError::io(format!("create snapshot directory {parent:?}"), source)
        })?;
    }

    let mut file = std::fs::File::create(&temp_path).map_err(|source| {
        DataplaneError::io(format!("create temporary snapshot {temp_path:?}"), source)
    })?;

    // Header: verbatim copy.
    file.write_all(&mmap_data[..slot_offset()])
        .map_err(|source| DataplaneError::io("write snapshot header", source))?;

    // Slots: seqlock-aware per-slot read; torn reads become unwritten
    // sentinels in the snapshot.
    // SAFETY: bounds checked above; PointSlot is repr(C, align(32)).
    let slots_ptr = unsafe { mmap_data.as_ptr().add(slot_offset()) as *const PointSlot };
    let mut torn = 0usize;
    for i in 0..slot_count {
        // SAFETY: i < slot_count, mmap covers slot_count slots.
        let slot = unsafe { &*slots_ptr.add(i) };
        let bytes = match slot.try_load_consistent() {
            Some((value, raw, ts)) => slot_snapshot_bytes(value, raw, ts),
            None => {
                torn += 1;
                slot_unwritten_bytes()
            },
        };
        file.write_all(&bytes)
            .map_err(|source| DataplaneError::io("write snapshot slot", source))?;
    }

    file.flush()
        .map_err(|source| DataplaneError::io("flush snapshot file", source))?;
    file.sync_all()
        .map_err(|source| DataplaneError::io("sync snapshot file", source))?;

    std::fs::rename(&temp_path, path).map_err(|source| {
        DataplaneError::io(
            format!("rename temporary snapshot {temp_path:?} to {path:?}"),
            source,
        )
    })?;

    let data_size = slot_offset() + slot_count * std::mem::size_of::<PointSlot>();
    if torn > 0 {
        tracing::warn!(
            "{} snapshot saved with {} torn slot(s) elided as unwritten: {:?}, size={} bytes, slots={}",
            label,
            torn,
            path,
            data_size,
            slot_count
        );
    } else {
        tracing::info!(
            "{} snapshot saved: {:?}, size={} bytes, slots={}",
            label,
            path,
            data_size,
            slot_count
        );
    }
    Ok(())
}

/// Encode a known-consistent slot as 32 bytes matching `PointSlot`'s
/// `#[repr(C)]` layout: value_bits(u64) | timestamp(u64) | raw_bits(u64) |
/// seq(u32) | dirty(u32). `seq` is written as 2 (even, "value committed"),
/// `dirty` as 0. Native-endian bytes match the in-memory representation
/// that restore loads directly.
fn slot_snapshot_bytes(value: f64, raw: f64, ts: u64) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[0..8].copy_from_slice(&value.to_bits().to_ne_bytes());
    out[8..16].copy_from_slice(&ts.to_ne_bytes());
    out[16..24].copy_from_slice(&raw.to_bits().to_ne_bytes());
    out[24..28].copy_from_slice(&2u32.to_ne_bytes());
    out[28..32].copy_from_slice(&0u32.to_ne_bytes());
    out
}

/// Encode an unwritten-sentinel slot for the snapshot file: NaN
/// value_bits + NaN raw_bits + seq=0, dirty=0. Matches the layout
/// produced by `PointSlot::new()` and is recognized by restore via
/// the NaN sentinel filter.
fn slot_unwritten_bytes() -> [u8; 32] {
    let mut out = [0u8; 32];
    out[0..8].copy_from_slice(&SLOT_UNWRITTEN_BITS.to_ne_bytes());
    // timestamp = 0 (already)
    out[16..24].copy_from_slice(&SLOT_UNWRITTEN_BITS.to_ne_bytes());
    // seq = 0, dirty = 0 (already)
    out
}
