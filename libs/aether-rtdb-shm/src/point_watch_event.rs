//! PointWatch Event — 56-byte wire format
//!
//! Carries a telemetry/status change notification from io → automation
//! over a Unix Domain Socket. Fixed 56-byte frame with no length prefix,
//! consistent with the existing fixed-size `ShmNotification` style.
//!
//! # Layout
//!
//! ```text
//! 0-3:    channel_id   (u32 LE)
//! 4-7:    point_id     (u32 LE)
//! 8:      point_type   (u8)      0=Telemetry, 1=Signal
//! 9-15:   _padding     ([u8;7])  zeros
//! 16-23:  value_bits   (u64 LE)  f64::to_bits(engineering value)
//! 24-31:  raw_bits     (u64 LE)  f64::to_bits(raw value)
//! 32-39:  slot_index   (u64 LE)  SHM slot index
//! 40-47:  timestamp_ms (u64 LE)  milliseconds since UNIX epoch
//! 48-55:  producer_id  (u64 LE)  io incarnation ID
//! ```

use bytemuck::{Pod, Zeroable};

/// PointWatch event wire frame (56 bytes, fixed size).
///
/// Generated in the io hot path when a subscribed T/S slot is written.
/// Delivered to automation via UDS for sub-5 ms rule-engine wake-up.
///
/// # Why no `seq` field?
///
/// PointWatch events are idempotent: automation reads the value from the event
/// (or re-reads SHM via `slot_index`), then evaluates deadband. Duplicate
/// events at most cause an extra deadband check — safe and cheap.
/// If strict deduplication is ever required, add a `seq` at offset 56 by
/// extending the struct to 64 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct PointWatchEvent {
    /// Channel ID (4 B)
    pub channel_id: u32,
    /// Point ID (4 B)
    pub point_id: u32,
    /// Point type byte: 0 = Telemetry, 1 = Signal (1 B)
    pub point_type: u8,
    /// Alignment padding — always zero (7 B)
    pub _padding: [u8; 7],
    /// Engineering value encoded as `f64::to_bits()` (8 B)
    pub value_bits: u64,
    /// Raw value encoded as `f64::to_bits()` (8 B)
    pub raw_bits: u64,
    /// SHM slot index — automation can re-read the slot directly (8 B)
    pub slot_index: u64,
    /// Event timestamp in milliseconds since UNIX epoch (8 B)
    pub timestamp_ms: u64,
    /// Io incarnation ID — changes on each io restart (8 B)
    pub producer_id: u64,
}

const _: () = assert!(
    std::mem::size_of::<PointWatchEvent>() == 56,
    "PointWatchEvent must be exactly 56 bytes"
);
const _: () = assert!(
    std::mem::align_of::<PointWatchEvent>() == 8,
    "PointWatchEvent must be 8-byte aligned"
);

impl PointWatchEvent {
    /// Fixed wire-frame size (bytes).
    pub const SIZE: usize = 56;

    /// Decode engineering value.
    #[inline]
    pub fn value(&self) -> f64 {
        f64::from_bits(self.value_bits)
    }

    /// Decode raw value.
    #[inline]
    pub fn raw(&self) -> f64 {
        f64::from_bits(self.raw_bits)
    }

    /// Serialize to a fixed byte array (zero-copy, safe via bytemuck).
    #[inline]
    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut buf = [0u8; Self::SIZE];
        buf.copy_from_slice(bytemuck::bytes_of(self));
        buf
    }

    /// Deserialize from a fixed byte array (zero-copy, safe via bytemuck).
    #[inline]
    pub fn from_bytes(bytes: &[u8; Self::SIZE]) -> Self {
        *bytemuck::from_bytes(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_size_and_align() {
        assert_eq!(std::mem::size_of::<PointWatchEvent>(), 56);
        assert_eq!(std::mem::align_of::<PointWatchEvent>(), 8);
    }

    #[test]
    fn event_roundtrip() {
        let ev = PointWatchEvent {
            channel_id: 1001,
            point_id: 42,
            point_type: 0, // Telemetry
            _padding: [0; 7],
            value_bits: 220.5f64.to_bits(),
            raw_bits: 2205.0f64.to_bits(),
            slot_index: 500,
            timestamp_ms: 1_748_430_000_000,
            producer_id: 0xDEAD_BEEF,
        };

        let bytes = ev.to_bytes();
        let decoded = PointWatchEvent::from_bytes(&bytes);

        assert_eq!(ev, decoded);
        assert_eq!(decoded.channel_id, 1001);
        assert_eq!(decoded.point_id, 42);
        assert_eq!(decoded.point_type, 0);
        assert!((decoded.value() - 220.5).abs() < f64::EPSILON);
        assert!((decoded.raw() - 2205.0).abs() < f64::EPSILON);
        assert_eq!(decoded.slot_index, 500);
        assert_eq!(decoded.producer_id, 0xDEAD_BEEF);
    }

    #[test]
    fn bytemuck_pod_from_bytes() {
        let ev = PointWatchEvent {
            channel_id: 99,
            point_id: 1,
            point_type: 1, // Signal
            _padding: [0; 7],
            value_bits: 0.0f64.to_bits(),
            raw_bits: 0.0f64.to_bits(),
            slot_index: 0,
            timestamp_ms: 0,
            producer_id: 1,
        };
        let bytes: &[u8] = bytemuck::bytes_of(&ev);
        assert_eq!(bytes.len(), 56);
        let decoded: PointWatchEvent = *bytemuck::from_bytes(bytes);
        assert_eq!(ev, decoded);
    }
}
