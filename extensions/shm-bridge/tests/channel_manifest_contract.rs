use aether_domain::{ChannelId, PointId, PointKind};
use aether_shm_bridge::{ChannelPointManifest, PhysicalPointAddress};

#[test]
fn layout_hash_remains_wire_compatible() {
    let manifest = ChannelPointManifest::from_entries([(1, [3, 1, 1, 1]), (7, [0, 2, 0, 1])]);

    assert_eq!(manifest.layout_hash(), 10_398_498_299_693_571_253);
}

#[test]
fn physical_points_iterate_in_slot_order_and_skip_padding() {
    let manifest = ChannelPointManifest::from_entries([(7, [1, 1, 0, 1]), (1, [1, 0, 1, 0])]);

    let actual: Vec<_> = manifest.iter_physical_points().collect();
    let expected = vec![
        (
            0,
            PhysicalPointAddress::new(ChannelId::new(1), PointKind::Telemetry, PointId::new(0)),
        ),
        (
            2,
            PhysicalPointAddress::new(ChannelId::new(1), PointKind::Command, PointId::new(0)),
        ),
        (
            4,
            PhysicalPointAddress::new(ChannelId::new(7), PointKind::Telemetry, PointId::new(0)),
        ),
        (
            5,
            PhysicalPointAddress::new(ChannelId::new(7), PointKind::Status, PointId::new(0)),
        ),
        (
            6,
            PhysicalPointAddress::new(ChannelId::new(7), PointKind::Action, PointId::new(0)),
        ),
    ];

    assert_eq!(actual, expected);
    assert_eq!(manifest.slot_count(), 7);
    assert_eq!(manifest.point_count(), 5);
}

#[test]
fn physical_point_reverse_lookup_round_trips_typed_addresses() {
    let manifest = ChannelPointManifest::from_entries([(7, [1, 1, 0, 1]), (1, [1, 0, 1, 0])]);

    for (slot, address) in manifest.iter_physical_points() {
        assert_eq!(manifest.slot_for(address), Some(slot));
        assert_eq!(manifest.physical_point_at(slot), Some(address));
        assert!(
            address.channel_id() == ChannelId::new(1) || address.channel_id() == ChannelId::new(7)
        );
        assert!(matches!(
            address.kind(),
            PointKind::Telemetry | PointKind::Status | PointKind::Command | PointKind::Action
        ));
        assert_eq!(address.point_id(), PointId::new(0));
    }

    assert_eq!(manifest.physical_point_at(1), None);
    assert_eq!(manifest.physical_point_at(3), None);
    assert_eq!(manifest.physical_point_at(7), None);
}

#[test]
fn typed_lookup_rejects_missing_channel_kind_and_point() {
    let manifest = ChannelPointManifest::from_entries([(4, [2, 1, 0, 0])]);

    assert_eq!(
        manifest.slot_for(PhysicalPointAddress::new(
            ChannelId::new(4),
            PointKind::Telemetry,
            PointId::new(1),
        )),
        Some(1)
    );
    assert_eq!(
        manifest.slot_for(PhysicalPointAddress::new(
            ChannelId::new(4),
            PointKind::Command,
            PointId::new(0),
        )),
        None
    );
    assert_eq!(
        manifest.slot_for(PhysicalPointAddress::new(
            ChannelId::new(4),
            PointKind::Telemetry,
            PointId::new(2),
        )),
        None
    );
    assert_eq!(
        manifest.slot_for(PhysicalPointAddress::new(
            ChannelId::new(99),
            PointKind::Telemetry,
            PointId::new(0),
        )),
        None
    );
}

#[test]
fn legacy_raw_lookup_matches_the_typed_lookup() {
    let manifest = ChannelPointManifest::from_entries([(4, [2, 1, 0, 0])]);
    let address =
        PhysicalPointAddress::new(ChannelId::new(4), PointKind::Telemetry, PointId::new(1));

    assert_eq!(manifest.slot(4, PointKind::Telemetry, 1), Some(1));
    assert_eq!(
        manifest.slot(4, PointKind::Telemetry, 1),
        manifest.slot_for(address)
    );
    assert_eq!(manifest.slot(4, PointKind::Command, 0), None);
}
