//! Stable snapshot format independent from the live mmap ABI.

use crate::{DataplaneError, DataplaneResult};

/// Snapshot file magic (`AETHSNAP` as an ASCII big-endian integer).
pub const SNAPSHOT_MAGIC: u64 = u64::from_be_bytes(*b"AETHSNAP");

/// Serialized snapshot header size.
pub(crate) const SNAPSHOT_HEADER_SIZE: usize = 24;

/// Tag for a slot that had no committed value when captured.
pub(crate) const SLOT_ABSENT: u8 = 0;

/// Tag for a slot followed by value/raw/timestamp/quality payload bytes.
pub(crate) const SLOT_PRESENT: u8 = 1;

/// Serialized payload size following [`SLOT_PRESENT`].
pub(crate) const SLOT_PAYLOAD_SIZE: usize = 28;

/// Identity and layout metadata for one snapshot image.
///
/// The snapshot deliberately contains no live-process state such as heartbeat,
/// writer generation, publication epoch, or seqlock sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotHeader {
    /// Snapshot format magic.
    pub magic: u64,
    /// Number of physical slots represented by the image.
    pub slot_count: u32,
    /// Composition-provided slot-layout fingerprint.
    pub layout_hash: u64,
}

impl SnapshotHeader {
    pub(crate) fn new(slot_count: u32, layout_hash: u64) -> Self {
        Self {
            magic: SNAPSHOT_MAGIC,
            slot_count,
            layout_hash,
        }
    }

    pub(crate) fn encode(self) -> [u8; SNAPSHOT_HEADER_SIZE] {
        let mut bytes = [0_u8; SNAPSHOT_HEADER_SIZE];
        bytes[0..8].copy_from_slice(&self.magic.to_be_bytes());
        bytes[12..16].copy_from_slice(&self.slot_count.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.layout_hash.to_le_bytes());
        bytes
    }

    pub(crate) fn decode(bytes: &[u8]) -> DataplaneResult<Self> {
        if bytes.len() < SNAPSHOT_HEADER_SIZE {
            return Err(DataplaneError::InvalidLayout(format!(
                "snapshot is shorter than its {SNAPSHOT_HEADER_SIZE}-byte header"
            )));
        }
        let header = Self {
            magic: u64::from_be_bytes(read_array(bytes, 0, "magic")?),
            slot_count: u32::from_le_bytes(read_array(bytes, 12, "slot_count")?),
            layout_hash: u64::from_le_bytes(read_array(bytes, 16, "layout_hash")?),
        };
        if header.magic != SNAPSHOT_MAGIC {
            return Err(DataplaneError::InvalidLayout(format!(
                "snapshot magic mismatch: expected 0x{SNAPSHOT_MAGIC:016x}, got 0x{:016x}",
                header.magic
            )));
        }
        if bytes[8..12].iter().any(|byte| *byte != 0) {
            return Err(DataplaneError::InvalidLayout(
                "snapshot reserved header bytes are non-zero".to_string(),
            ));
        }
        Ok(header)
    }
}

fn read_array<const N: usize>(
    bytes: &[u8],
    offset: usize,
    label: &str,
) -> DataplaneResult<[u8; N]> {
    bytes
        .get(offset..offset + N)
        .ok_or_else(|| DataplaneError::InvalidLayout(format!("snapshot is missing {label}")))?
        .try_into()
        .map_err(|_| DataplaneError::InvalidLayout(format!("snapshot has an invalid {label}")))
}
