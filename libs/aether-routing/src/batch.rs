//! Data-plane batch types shared by the routing cache and SHM writer.
//!
//! This module deliberately contains no storage implementation. The
//! acquisition owner writes batches through `aether-rtdb-shm`; optional
//! mirrors consume SHM after the authoritative write.

use aether_model::PointType;

/// One engineering-value update produced by a device protocol.
#[derive(Debug, Clone)]
pub struct ChannelPointUpdate {
    pub channel_id: u32,
    pub point_type: PointType,
    pub point_id: u32,
    pub value: f64,
    pub raw_value: Option<f64>,
    pub cascade_depth: u8,
}

impl ChannelPointUpdate {
    #[must_use]
    pub fn new(channel_id: u32, point_type: PointType, point_id: u32, value: f64) -> Self {
        Self {
            channel_id,
            point_type,
            point_id,
            value,
            raw_value: None,
            cascade_depth: 0,
        }
    }

    #[must_use]
    pub fn with_raw(mut self, raw_value: f64) -> Self {
        self.raw_value = Some(raw_value);
        self
    }
}

/// Counters returned by an authoritative SHM batch write.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BatchRoutingResult {
    pub channel_writes: usize,
    /// C2M is an SHM alias lookup, not a copied write; retained as a metric
    /// field and therefore normally zero on the SHM-only path.
    pub c2m_writes: usize,
    pub c2c_forwards: usize,
    pub cycles_detected: usize,
    pub slot_misses: usize,
}

impl BatchRoutingResult {
    pub fn merge(&mut self, other: Self) {
        self.channel_writes += other.channel_writes;
        self.c2m_writes += other.c2m_writes;
        self.c2c_forwards += other.c2c_forwards;
        self.cycles_detected += other.cycles_detected;
        self.slot_misses += other.slot_misses;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_accumulates_every_counter() {
        let mut result = BatchRoutingResult {
            channel_writes: 2,
            c2m_writes: 0,
            c2c_forwards: 1,
            cycles_detected: 0,
            slot_misses: 3,
        };
        result.merge(BatchRoutingResult {
            channel_writes: 4,
            c2m_writes: 0,
            c2c_forwards: 2,
            cycles_detected: 1,
            slot_misses: 5,
        });
        assert_eq!(result.channel_writes, 6);
        assert_eq!(result.c2c_forwards, 3);
        assert_eq!(result.cycles_detected, 1);
        assert_eq!(result.slot_misses, 8);
    }
}
