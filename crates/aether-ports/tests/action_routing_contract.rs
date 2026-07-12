use std::sync::Arc;

use aether_domain::{ChannelCommandAddress, ChannelId, InstanceId, PointId, PointKind};
use aether_ports::{
    ActionRoute, ActionRouteKey, ActionRoutingMutation, ActionRoutingMutationKind,
    ActionRoutingMutationReceipt, ActionRoutingTarget, AutomationActionRoutingMutator,
};

fn route_key() -> ActionRouteKey {
    ActionRouteKey::new(InstanceId::new(7), PointId::new(11))
}

fn route() -> ActionRoute {
    let destination =
        ChannelCommandAddress::new(ChannelId::new(3), PointKind::Command, PointId::new(19))
            .expect("command-owned channel address");
    ActionRoute::new(route_key(), destination, true)
}

#[test]
fn action_route_preserves_typed_source_destination_and_enabled_state() {
    let route = route();

    assert_eq!(route.key(), route_key());
    assert_eq!(route.key().instance_id(), InstanceId::new(7));
    assert_eq!(route.key().action_id(), PointId::new(11));
    assert_eq!(route.destination().channel_id(), ChannelId::new(3));
    assert_eq!(route.destination().kind(), PointKind::Command);
    assert_eq!(route.destination().point_id(), PointId::new(19));
    assert!(route.enabled());
}

#[test]
fn one_mutation_type_covers_upsert_delete_toggle_and_delete_all() {
    let upsert = ActionRoutingMutation::upsert(route());
    assert_eq!(upsert.kind(), ActionRoutingMutationKind::Upsert);
    assert_eq!(upsert.route_key(), Some(route_key()));
    assert_eq!(upsert.target(), ActionRoutingTarget::Route(route_key()));
    assert_eq!(upsert.route(), Some(&route()));

    let delete = ActionRoutingMutation::delete(route_key());
    assert_eq!(delete.kind(), ActionRoutingMutationKind::Delete);
    assert_eq!(delete.route_key(), Some(route_key()));

    let enable = ActionRoutingMutation::set_enabled(route_key(), true);
    assert_eq!(enable.kind(), ActionRoutingMutationKind::Enable);
    assert_eq!(enable.route_key(), Some(route_key()));

    let disable = ActionRoutingMutation::set_enabled(route_key(), false);
    assert_eq!(disable.kind(), ActionRoutingMutationKind::Disable);
    assert_eq!(disable.route_key(), Some(route_key()));

    let delete_instance = ActionRoutingMutation::delete_actions_for_instance(InstanceId::new(7));
    assert_eq!(
        delete_instance.kind(),
        ActionRoutingMutationKind::DeleteActionsForInstance
    );
    assert_eq!(
        delete_instance.target(),
        ActionRoutingTarget::Instance(InstanceId::new(7))
    );

    let delete_channel = ActionRoutingMutation::delete_actions_for_channel(ChannelId::new(3));
    assert_eq!(
        delete_channel.kind(),
        ActionRoutingMutationKind::DeleteActionsForChannel
    );
    assert_eq!(
        delete_channel.target(),
        ActionRoutingTarget::Channel(ChannelId::new(3))
    );

    let delete_all = ActionRoutingMutation::delete_all();
    assert_eq!(
        delete_all.kind(),
        ActionRoutingMutationKind::DeleteAllActions
    );
    assert_eq!(delete_all.route_key(), None);
    assert_eq!(delete_all.target(), ActionRoutingTarget::AllActions);
}

#[test]
fn mutation_receipt_preserves_operation_target_and_affected_count() {
    let receipt = ActionRoutingMutationReceipt::new(
        ActionRoutingMutationKind::Delete,
        ActionRoutingTarget::Route(route_key()),
        1,
    );

    assert_eq!(receipt.kind(), ActionRoutingMutationKind::Delete);
    assert_eq!(receipt.route_key(), Some(route_key()));
    assert_eq!(receipt.target(), ActionRoutingTarget::Route(route_key()));
    assert_eq!(receipt.affected_routes(), 1);
}

#[test]
fn action_routing_mutator_is_object_safe() {
    fn accepts_port(_: Option<Arc<dyn AutomationActionRoutingMutator>>) {}

    accepts_port(None);
}
