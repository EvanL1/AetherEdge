//! Validated reader for the persistent snapshot format.

use std::path::Path;

use crate::core::slot_io::SlotRead;
use crate::core::snapshot_format::{
    SLOT_ABSENT, SLOT_PAYLOAD_SIZE, SLOT_PRESENT, SNAPSHOT_HEADER_SIZE, SnapshotHeader,
};
use crate::{DataplaneError, DataplaneResult};

/// Fully validated snapshot image.
#[derive(Debug)]
pub struct SnapshotImage {
    header: SnapshotHeader,
    slots: Vec<Option<SlotRead>>,
}

impl SnapshotImage {
    /// Loads and validates the current snapshot format.
    ///
    /// Live mmap images and every earlier snapshot format are rejected; there
    /// is no compatibility decoder.
    pub fn load(path: impl AsRef<Path>) -> DataplaneResult<Self> {
        let path = path.as_ref();
        let bytes = std::fs::read(path)
            .map_err(|source| DataplaneError::io(format!("read SHM snapshot {path:?}"), source))?;
        let header = SnapshotHeader::decode(&bytes)?;
        let slot_count = header.slot_count as usize;
        let minimum_len = SNAPSHOT_HEADER_SIZE
            .checked_add(slot_count)
            .ok_or_else(|| DataplaneError::InvalidLayout("snapshot length overflow".to_string()))?;
        if bytes.len() < minimum_len {
            return Err(DataplaneError::InvalidLayout(format!(
                "snapshot is too short for {slot_count} presence records: have {} bytes, need at least {minimum_len}",
                bytes.len()
            )));
        }

        let mut cursor = SNAPSHOT_HEADER_SIZE;
        let mut slots = Vec::with_capacity(slot_count);
        for slot in 0..slot_count {
            let presence = *bytes.get(cursor).ok_or_else(|| {
                DataplaneError::InvalidLayout(format!(
                    "snapshot is missing presence tag for slot {slot}"
                ))
            })?;
            cursor += 1;
            match presence {
                SLOT_ABSENT => slots.push(None),
                SLOT_PRESENT => {
                    let payload_end = cursor.checked_add(SLOT_PAYLOAD_SIZE).ok_or_else(|| {
                        DataplaneError::InvalidLayout("snapshot length overflow".to_string())
                    })?;
                    let payload = bytes.get(cursor..payload_end).ok_or_else(|| {
                        DataplaneError::InvalidLayout(format!(
                            "snapshot is missing value payload for slot {slot}"
                        ))
                    })?;
                    slots.push(Some(decode_present_slot(payload, slot)?));
                    cursor = payload_end;
                },
                tag => {
                    return Err(DataplaneError::InvalidLayout(format!(
                        "snapshot slot {slot} has unknown presence tag {tag}"
                    )));
                },
            }
        }
        if cursor != bytes.len() {
            return Err(DataplaneError::InvalidLayout(format!(
                "snapshot has {} trailing byte(s)",
                bytes.len() - cursor
            )));
        }

        Ok(Self { header, slots })
    }

    /// Returns the snapshot's persistent layout metadata.
    #[must_use]
    pub const fn header(&self) -> SnapshotHeader {
        self.header
    }

    /// Returns slot values; `None` is an explicit absent record.
    #[must_use]
    pub fn slots(&self) -> &[Option<SlotRead>] {
        &self.slots
    }
}

fn decode_present_slot(payload: &[u8], slot: usize) -> DataplaneResult<SlotRead> {
    let value = f64::from_bits(u64::from_le_bytes(read_array(payload, 0, "value")?));
    let raw = f64::from_bits(u64::from_le_bytes(read_array(payload, 8, "raw value")?));
    let timestamp_ms = u64::from_le_bytes(read_array(payload, 16, "timestamp")?);
    let quality_code = u32::from_le_bytes(read_array(payload, 24, "quality")?);
    if !value.is_finite() || !raw.is_finite() {
        return Err(DataplaneError::InvalidLayout(format!(
            "snapshot slot {slot} contains non-finite present data"
        )));
    }
    Ok(SlotRead {
        value,
        raw,
        timestamp_ms,
        quality_code,
    })
}

fn read_array<const N: usize>(
    bytes: &[u8],
    offset: usize,
    label: &str,
) -> DataplaneResult<[u8; N]> {
    bytes
        .get(offset..offset + N)
        .ok_or_else(|| DataplaneError::InvalidLayout(format!("snapshot is missing slot {label}")))?
        .try_into()
        .map_err(|_| DataplaneError::InvalidLayout(format!("snapshot has an invalid slot {label}")))
}
