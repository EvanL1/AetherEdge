use std::sync::Arc;

use aether_domain::{ChannelId, ChannelPointAddress, InstanceId, PointId, PointKind};
use aether_ports::{
    AutomationMeasurementRoutingMutator, LogicalRoutingRevision, MeasurementRoute,
    MeasurementRouteKey, MeasurementRoutingMutation,
};

fn route() -> MeasurementRoute {
    MeasurementRoute::new(
        MeasurementRouteKey::new(InstanceId::new(7), PointId::new(11)),
        ChannelPointAddress::new(ChannelId::new(3), PointKind::Telemetry, PointId::new(19))
            .expect("acquisition-owned destination"),
        true,
    )
}

#[test]
fn measurement_mutations_always_carry_a_compare_and_set_revision() {
    let expected = LogicalRoutingRevision::new(4);
    let upsert = MeasurementRoutingMutation::upsert(route(), expected);
    let delete = MeasurementRoutingMutation::delete(route().key(), expected);
    let disable = MeasurementRoutingMutation::set_enabled(route().key(), false, expected);

    assert_eq!(upsert.expected_revision(), expected);
    assert_eq!(delete.expected_revision(), expected);
    assert_eq!(disable.expected_revision(), expected);
    assert_eq!(
        expected.checked_next(),
        Some(LogicalRoutingRevision::new(5))
    );
    assert_eq!(
        MeasurementRoutingMutation::delete_all(expected).target(),
        aether_ports::MeasurementRoutingTarget::AllMeasurements
    );
}

#[test]
fn measurement_destination_is_acquisition_owned_by_construction() {
    let route = route();
    assert_eq!(route.destination().kind(), PointKind::Telemetry);
    assert_eq!(route.destination().channel_id(), ChannelId::new(3));

    fn accepts_port(_: Option<Arc<dyn AutomationMeasurementRoutingMutator>>) {}
    accepts_port(None);
}
