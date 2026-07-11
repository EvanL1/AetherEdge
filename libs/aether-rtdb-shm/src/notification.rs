//! UDS Event Notification Protocol
//!
//! Used for M2C command notifications from automation → io.
//! Notifications carry the full command event so io does not need to
//! re-read SHM and collapse multiple writes to the same point.

use aether_model::PointType;
use bytemuck::{Pod, Zeroable};

/// M2C command notification (56 bytes, fixed size)
///
/// Sent via Unix Domain Socket to notify io of new Control/Adjustment commands.
/// Uses `#[repr(C)]` to ensure C-compatible memory layout for cross-process transport.
///
/// # Protocol Semantics
///
/// Notifications are **command events**. Each message carries:
/// - routing target (`channel_id`, `point_type`, `point_id`)
/// - command payload (`value_bits`, `timestamp_ms`, `expires_at_ms`)
/// - producer ordering (`producer_id`, `seq`)
///
/// `producer_id + seq` lets io reject duplicate or stale events after
/// reconnects without depending on SHM state.
///
/// # Safety
/// Deriving `Pod` + `Zeroable` guarantees at compile time:
/// - All fields are POD types (no pointers, no Drop)
/// - `#[repr(C)]` layout with explicit padding has no implicit padding bytes
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Pod, Zeroable)]
pub struct ShmNotification {
    /// Channel ID
    pub channel_id: u32,
    /// Point ID
    pub point_id: u32,
    /// Point type (Control=2, Adjustment=3)
    pub point_type: u8,
    /// Alignment padding
    pub _padding: [u8; 7],
    /// Command value encoded via `f64::to_bits()`
    pub value_bits: u64,
    /// Command timestamp (milliseconds since UNIX epoch)
    pub timestamp_ms: u64,
    /// Exclusive command deadline (milliseconds since UNIX epoch)
    pub expires_at_ms: u64,
    /// Producer incarnation ID
    pub producer_id: u64,
    /// Monotonic sequence number within the producer incarnation
    pub seq: u64,
}

impl ShmNotification {
    /// Fixed message size
    pub const SIZE: usize = 56;

    /// Create a new notification
    pub fn new(
        channel_id: u32,
        point_type: PointType,
        point_id: u32,
        value: f64,
        timestamp_ms: u64,
        expires_at_ms: u64,
        producer_id: u64,
        seq: u64,
    ) -> Self {
        Self {
            channel_id,
            point_id,
            point_type: point_type as u8,
            _padding: [0; 7],
            value_bits: value.to_bits(),
            timestamp_ms,
            expires_at_ms,
            producer_id,
            seq,
        }
    }

    /// Convert to byte array (zero-copy, compile-time safety guarantee)
    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes.copy_from_slice(bytemuck::bytes_of(self));
        bytes
    }

    /// Create from byte array (zero-copy, compile-time safety guarantee)
    pub fn from_bytes(bytes: &[u8; Self::SIZE]) -> Self {
        *bytemuck::from_bytes(bytes)
    }

    /// Decode command value
    #[inline]
    pub fn value(&self) -> f64 {
        f64::from_bits(self.value_bits)
    }

    /// Get point type
    pub fn get_point_type(&self) -> Option<PointType> {
        match self.point_type {
            2 => Some(PointType::Control),
            3 => Some(PointType::Adjustment),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_notification_size() {
        assert_eq!(
            std::mem::size_of::<ShmNotification>(),
            ShmNotification::SIZE
        );
    }

    #[test]
    fn test_notification_roundtrip() {
        let notif = ShmNotification::new(1001, PointType::Control, 42, 123.45, 99, 199, 7, 11);
        let bytes = notif.to_bytes();
        let decoded = ShmNotification::from_bytes(&bytes);

        assert_eq!(notif, decoded);
        assert_eq!(decoded.channel_id, 1001);
        assert_eq!(decoded.get_point_type(), Some(PointType::Control));
        assert_eq!(decoded.point_id, 42);
        assert_eq!(decoded.value(), 123.45);
        assert_eq!(decoded.timestamp_ms, 99);
        assert_eq!(decoded.expires_at_ms, 199);
        assert_eq!(decoded.producer_id, 7);
        assert_eq!(decoded.seq, 11);
    }
}
