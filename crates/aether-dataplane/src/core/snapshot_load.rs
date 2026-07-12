//! Validated reader for the compact physical snapshot format.

use std::path::Path;

use crate::core::header::{HeaderSnapshot, UNIFIED_MAGIC, UNIFIED_VERSION, slot_offset};
use crate::core::slot::SLOT_UNWRITTEN_BITS;
use crate::core::slot_io::SlotRead;
use crate::{DataplaneError, DataplaneResult};

const SLOT_BYTES: usize = 32;

/// Fully validated compact snapshot image.
#[derive(Debug)]
pub struct SnapshotImage {
    header: HeaderSnapshot,
    slots: Vec<Option<SlotRead>>,
}

impl SnapshotImage {
    /// Loads and validates a compact snapshot without interpreting its opaque
    /// manifest fingerprint.
    pub fn load(path: impl AsRef<Path>) -> DataplaneResult<Self> {
        let path = path.as_ref();
        let bytes = std::fs::read(path)
            .map_err(|source| DataplaneError::io(format!("read SHM snapshot {path:?}"), source))?;
        if bytes.len() < slot_offset() {
            return Err(DataplaneError::InvalidLayout(format!(
                "snapshot {path:?} is shorter than its header"
            )));
        }

        let header = decode_header(&bytes)?;
        if header.magic != UNIFIED_MAGIC {
            return Err(DataplaneError::InvalidLayout(format!(
                "snapshot magic mismatch: expected 0x{UNIFIED_MAGIC:x}, got 0x{:x}",
                header.magic
            )));
        }
        if header.version != UNIFIED_VERSION {
            return Err(DataplaneError::InvalidLayout(format!(
                "snapshot version mismatch: expected {UNIFIED_VERSION}, got {}",
                header.version
            )));
        }
        if header.slot_count > header.max_slots {
            return Err(DataplaneError::InvalidLayout(format!(
                "snapshot slot_count {} exceeds max_slots {}",
                header.slot_count, header.max_slots
            )));
        }
        if header.writer_generation == 0 || header.writer_generation & 1 != 0 {
            return Err(DataplaneError::InvalidLayout(format!(
                "snapshot writer generation {} is not stable",
                header.writer_generation
            )));
        }

        let required = usize::try_from(header.slot_count)
            .ok()
            .and_then(|count| count.checked_mul(SLOT_BYTES))
            .and_then(|slots| slot_offset().checked_add(slots))
            .ok_or_else(|| DataplaneError::InvalidLayout("snapshot length overflow".to_string()))?;
        if bytes.len() != required {
            return Err(DataplaneError::InvalidLayout(format!(
                "snapshot length mismatch: expected {required} bytes, got {}",
                bytes.len()
            )));
        }

        let mut slots = Vec::with_capacity(header.slot_count as usize);
        for slot in 0..header.slot_count as usize {
            slots.push(decode_slot(&bytes, slot)?);
        }
        Ok(Self { header, slots })
    }

    /// Returns physical header metadata captured by the snapshot.
    #[must_use]
    pub const fn header(&self) -> HeaderSnapshot {
        self.header
    }

    /// Returns compact slot values; `None` is the unwritten sentinel.
    #[must_use]
    pub fn slots(&self) -> &[Option<SlotRead>] {
        &self.slots
    }
}

fn decode_header(bytes: &[u8]) -> DataplaneResult<HeaderSnapshot> {
    Ok(HeaderSnapshot {
        magic: read_u64(bytes, 0, "magic")?,
        version: read_u32(bytes, 8, "version")?,
        max_slots: read_u32(bytes, 12, "max_slots")?,
        slot_count: read_u32(bytes, 16, "slot_count")?,
        last_update_ts: read_u64(bytes, 24, "last_update_ts")?,
        writer_heartbeat: read_u64(bytes, 32, "writer_heartbeat")?,
        routing_hash: read_u64(bytes, 40, "routing_hash")?,
        writer_generation: read_u64(bytes, 48, "writer_generation")?,
    })
}

fn decode_slot(bytes: &[u8], slot: usize) -> DataplaneResult<Option<SlotRead>> {
    let base = slot_offset() + slot * SLOT_BYTES;
    let value_bits = read_u64(bytes, base, "slot value")?;
    let timestamp_ms = read_u64(bytes, base + 8, "slot timestamp")?;
    let raw_bits = read_u64(bytes, base + 16, "slot raw value")?;
    let sequence = read_u32(bytes, base + 24, "slot sequence")?;
    if sequence & 1 != 0 {
        return Err(DataplaneError::InvalidLayout(format!(
            "snapshot slot {slot} has an in-progress sequence {sequence}"
        )));
    }
    if value_bits == SLOT_UNWRITTEN_BITS && raw_bits == SLOT_UNWRITTEN_BITS {
        return Ok(None);
    }
    let value = f64::from_bits(value_bits);
    let raw = f64::from_bits(raw_bits);
    if !value.is_finite() || !raw.is_finite() {
        return Err(DataplaneError::InvalidLayout(format!(
            "snapshot slot {slot} contains non-finite live data"
        )));
    }
    Ok(Some(SlotRead {
        value,
        raw,
        timestamp_ms,
    }))
}

fn read_u64(bytes: &[u8], offset: usize, label: &str) -> DataplaneResult<u64> {
    let value = bytes
        .get(offset..offset + 8)
        .ok_or_else(|| DataplaneError::InvalidLayout(format!("snapshot is missing {label}")))?;
    let value: [u8; 8] = value
        .try_into()
        .map_err(|_| DataplaneError::InvalidLayout(format!("snapshot has an invalid {label}")))?;
    Ok(u64::from_ne_bytes(value))
}

fn read_u32(bytes: &[u8], offset: usize, label: &str) -> DataplaneResult<u32> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| DataplaneError::InvalidLayout(format!("snapshot is missing {label}")))?;
    let value: [u8; 4] = value
        .try_into()
        .map_err(|_| DataplaneError::InvalidLayout(format!("snapshot has an invalid {label}")))?;
    Ok(u32::from_ne_bytes(value))
}
