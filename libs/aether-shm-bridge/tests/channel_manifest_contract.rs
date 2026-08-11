use aether_domain::{ChannelCommandAddress, ChannelId, ChannelPointAddress, PointId, PointKind};
use aether_shm_bridge::{ChannelPointManifest, PhysicalPointAddress};

#[test]
fn layout_hash_is_canonical_and_covers_exact_addresses() {
    let addresses = [
        PhysicalPointAddress::from_raw_ids(7, PointKind::Status, 9),
        PhysicalPointAddress::from_raw_ids(1, PointKind::Telemetry, 3),
    ];
    let first = ChannelPointManifest::compile(addresses, 2).expect("exact manifest");
    let second = ChannelPointManifest::compile(addresses.into_iter().rev(), 2)
        .expect("order-independent manifest");
    let changed = ChannelPointManifest::compile(
        [
            PhysicalPointAddress::from_raw_ids(7, PointKind::Status, 10),
            PhysicalPointAddress::from_raw_ids(1, PointKind::Telemetry, 3),
        ],
        2,
    )
    .expect("changed manifest");

    assert_eq!(first.layout_hash(), second.layout_hash());
    assert_ne!(first.layout_hash(), changed.layout_hash());
}

#[test]
fn physical_points_iterate_in_canonical_order_with_only_ownership_padding() {
    let manifest = ChannelPointManifest::dense_test_fixture([(7, [1, 1, 0, 1]), (1, [1, 0, 1, 0])]);

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
            3,
            PhysicalPointAddress::new(ChannelId::new(7), PointKind::Telemetry, PointId::new(0)),
        ),
        (
            4,
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
    let manifest = ChannelPointManifest::dense_test_fixture([(7, [1, 1, 0, 1]), (1, [1, 0, 1, 0])]);

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
    assert_eq!(manifest.physical_point_at(5), None);
    assert_eq!(manifest.physical_point_at(7), None);
}

#[test]
fn acquisition_and_command_authorities_never_share_a_cache_line_within_a_channel() {
    let manifest = ChannelPointManifest::dense_test_fixture([(4, [3, 0, 1, 1])]);
    let telemetry_last = manifest
        .slot_for(PhysicalPointAddress::from_raw_ids(
            4,
            PointKind::Telemetry,
            2,
        ))
        .expect("last acquisition slot");
    let command_first = manifest
        .slot_for(PhysicalPointAddress::from_raw_ids(4, PointKind::Command, 0))
        .expect("first command slot");

    assert_ne!(telemetry_last / 2, command_first / 2);
    assert_eq!(command_first % 2, 0);
    assert_eq!(manifest.physical_point_at(3), None);
}

#[test]
fn sparse_point_identifiers_do_not_create_implicit_points() {
    let configured = [
        PhysicalPointAddress::from_raw_ids(4, PointKind::Telemetry, 2),
        PhysicalPointAddress::from_raw_ids(4, PointKind::Telemetry, 100),
    ];
    let manifest = ChannelPointManifest::compile(configured, 2).expect("sparse exact manifest");

    assert_eq!(manifest.slot_count(), 2);
    assert_eq!(manifest.slot_for(configured[0]), Some(0));
    assert_eq!(manifest.slot_for(configured[1]), Some(1));
    assert_eq!(
        manifest.slot_for(PhysicalPointAddress::from_raw_ids(
            4,
            PointKind::Telemetry,
            3,
        )),
        None
    );
    assert_eq!(
        manifest
            .point_ids(4, PointKind::Telemetry)
            .collect::<Vec<_>>(),
        vec![2, 100]
    );
}

#[test]
fn exact_compilation_rejects_duplicates_and_capacity_overflow() {
    let address = PhysicalPointAddress::from_raw_ids(4, PointKind::Telemetry, 2);
    let command = PhysicalPointAddress::from_raw_ids(4, PointKind::Command, 0);

    let duplicate = ChannelPointManifest::compile([address, address], 2)
        .expect_err("duplicate address must fail");
    let overflow = ChannelPointManifest::compile([address], 0)
        .expect_err("capacity must be checked before retaining a point");
    let padding_overflow = ChannelPointManifest::compile([address, command], 2)
        .expect_err("ownership padding participates in physical capacity");

    assert_eq!(duplicate.kind(), aether_ports::PortErrorKind::InvalidData);
    assert_eq!(overflow.kind(), aether_ports::PortErrorKind::InvalidData);
    assert_eq!(
        padding_overflow.kind(),
        aether_ports::PortErrorKind::InvalidData
    );
}

#[test]
fn domain_channel_addresses_convert_to_the_same_physical_manifest_address() {
    let acquisition =
        ChannelPointAddress::new(ChannelId::new(7), PointKind::Telemetry, PointId::new(3))
            .expect("acquisition-owned address");
    let command = ChannelCommandAddress::new(ChannelId::new(7), PointKind::Action, PointId::new(4))
        .expect("command-owned address");

    assert_eq!(
        PhysicalPointAddress::from(acquisition),
        PhysicalPointAddress::new(ChannelId::new(7), PointKind::Telemetry, PointId::new(3))
    );
    assert_eq!(
        PhysicalPointAddress::from(command),
        PhysicalPointAddress::new(ChannelId::new(7), PointKind::Action, PointId::new(4))
    );
    assert_eq!(
        PhysicalPointAddress::from_raw_ids(7, PointKind::Action, 4),
        PhysicalPointAddress::from(command)
    );
}

#[test]
fn typed_lookup_rejects_missing_channel_kind_and_point() {
    let manifest = ChannelPointManifest::dense_test_fixture([(4, [2, 1, 0, 0])]);

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
