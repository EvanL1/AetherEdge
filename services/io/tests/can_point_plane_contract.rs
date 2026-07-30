//! CAN read-plane capability contract.

#![allow(clippy::disallowed_methods)]

use aether_core::PointType;
use aether_io::protocols::adapters::can::config::CanFrameCache;
use aether_io::protocols::adapters::can::decoder::PointManager;
use aether_io::protocols::adapters::can::{CanDataType, CanPoint};
use aether_io::protocols::core::data::Value;

fn point(point_id: u32, point_type: PointType, can_id: u32) -> CanPoint {
    CanPoint {
        point_id,
        point_type,
        can_id,
        byte_offset: 0,
        bit_position: 0,
        bit_length: 16,
        data_type: CanDataType::UInt16,
        scale: 1.0,
        offset: 0.0,
    }
}

#[test]
fn can_preserves_telemetry_and_signal_planes_and_rejects_writes() {
    let mut rejected_generation = PointManager::new();
    assert!(
        rejected_generation
            .add_points(vec![
                point(9, PointType::Telemetry, 0x351),
                point(10, PointType::Control, 0x351),
            ])
            .is_err()
    );
    assert!(rejected_generation.is_empty());

    let mut manager = PointManager::new();
    manager
        .add_point(point(1, PointType::Telemetry, 0x351))
        .unwrap();
    manager
        .add_point(point(1, PointType::Signal, 0x356))
        .unwrap();
    assert!(
        manager
            .add_point(point(2, PointType::Control, 0x351))
            .is_err()
    );
    assert!(
        manager
            .add_point(point(3, PointType::Adjustment, 0x351))
            .is_err()
    );

    let mut cache = CanFrameCache::new();
    cache.update(0x351, &[100, 0, 0, 0, 0, 0, 0, 0]);
    cache.update(0x356, &[1, 0, 0, 0, 0, 0, 0, 0]);

    let batch = manager.apply_mappings(&cache).unwrap();
    assert_eq!(batch.len(), 2);
    assert!(batch.iter().any(|sample| {
        sample.id == 1
            && sample.point_type == PointType::Telemetry
            && sample.value == Value::Integer(100)
    }));
    assert!(batch.iter().any(|sample| {
        sample.id == 1
            && sample.point_type == PointType::Signal
            && sample.value == Value::Integer(1)
    }));
}
