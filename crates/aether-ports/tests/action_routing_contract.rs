use std::sync::Arc;

use aether_domain::{ChannelCommandAddress, ChannelId, InstanceId, PointId, PointKind};
use aether_ports::{
    ActionRoute, ActionRouteKey, ActionRoutingMutation, ActionRoutingMutationKind,
    ActionRoutingMutationReceipt, ActionRoutingRuntimeStatus, ActionRoutingTarget,
    AutomationActionRoutingMutator, LogicalRoutingRevision, PortError, PortErrorKind,
    RevisionedActionRoutingMutation,
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
    let expected = LogicalRoutingRevision::new(7);
    let upsert = RevisionedActionRoutingMutation::upsert(route(), expected);
    assert_eq!(upsert.kind(), ActionRoutingMutationKind::Upsert);
    assert_eq!(upsert.expected_revision(), expected);
    assert_eq!(upsert.route_key(), Some(route_key()));
    assert_eq!(upsert.target(), ActionRoutingTarget::Route(route_key()));
    assert_eq!(upsert.route(), Some(&route()));

    let delete = RevisionedActionRoutingMutation::delete(route_key(), expected);
    assert_eq!(delete.kind(), ActionRoutingMutationKind::Delete);
    assert_eq!(delete.route_key(), Some(route_key()));

    let enable = RevisionedActionRoutingMutation::set_enabled(route_key(), true, expected);
    assert_eq!(enable.kind(), ActionRoutingMutationKind::Enable);
    assert_eq!(enable.route_key(), Some(route_key()));

    let disable = RevisionedActionRoutingMutation::set_enabled(route_key(), false, expected);
    assert_eq!(disable.kind(), ActionRoutingMutationKind::Disable);
    assert_eq!(disable.route_key(), Some(route_key()));

    let delete_instance =
        RevisionedActionRoutingMutation::delete_actions_for_instance(InstanceId::new(7), expected);
    assert_eq!(
        delete_instance.kind(),
        ActionRoutingMutationKind::DeleteActionsForInstance
    );
    assert_eq!(
        delete_instance.target(),
        ActionRoutingTarget::Instance(InstanceId::new(7))
    );

    let delete_channel =
        RevisionedActionRoutingMutation::delete_actions_for_channel(ChannelId::new(3), expected);
    assert_eq!(
        delete_channel.kind(),
        ActionRoutingMutationKind::DeleteActionsForChannel
    );
    assert_eq!(
        delete_channel.target(),
        ActionRoutingTarget::Channel(ChannelId::new(3))
    );

    let delete_all = RevisionedActionRoutingMutation::delete_all(expected);
    assert_eq!(
        delete_all.kind(),
        ActionRoutingMutationKind::DeleteAllActions
    );
    assert_eq!(delete_all.route_key(), None);
    assert_eq!(delete_all.target(), ActionRoutingTarget::AllActions);
}

#[test]
fn legacy_action_routing_constructors_remain_revisionless() {
    let mutations = [
        ActionRoutingMutation::upsert(route()),
        ActionRoutingMutation::delete(route_key()),
        ActionRoutingMutation::set_enabled(route_key(), true),
        ActionRoutingMutation::delete_actions_for_instance(InstanceId::new(7)),
        ActionRoutingMutation::delete_actions_for_channel(ChannelId::new(3)),
        ActionRoutingMutation::delete_all(),
    ];

    assert_eq!(mutations[0].kind(), ActionRoutingMutationKind::Upsert);
    assert_eq!(
        mutations[5].kind(),
        ActionRoutingMutationKind::DeleteAllActions
    );
}

#[test]
fn mutation_receipt_preserves_operation_target_and_affected_count() {
    let receipt = ActionRoutingMutationReceipt::new_at_revision(
        ActionRoutingMutationKind::Delete,
        ActionRoutingTarget::Route(route_key()),
        1,
        LogicalRoutingRevision::new(8),
    );

    assert_eq!(receipt.kind(), ActionRoutingMutationKind::Delete);
    assert_eq!(receipt.route_key(), Some(route_key()));
    assert_eq!(receipt.target(), ActionRoutingTarget::Route(route_key()));
    assert_eq!(receipt.affected_routes(), 1);
    assert_eq!(receipt.resulting_revision(), LogicalRoutingRevision::new(8));
    assert!(receipt.runtime_status().is_published());
    assert!(!receipt.runtime_status().reconciliation_required());
}

#[test]
fn committed_receipt_can_report_fail_closed_runtime_degradation_without_becoming_an_error() {
    let receipt = ActionRoutingMutationReceipt::commands_revoked_at_revision(
        ActionRoutingMutationKind::Upsert,
        ActionRoutingTarget::Route(route_key()),
        1,
        LogicalRoutingRevision::new(8),
        PortError::new(
            PortErrorKind::Unavailable,
            "physical topology is incomplete",
        ),
    );

    assert_eq!(receipt.runtime_status().as_str(), "commands_revoked");
    assert_eq!(receipt.resulting_revision(), LogicalRoutingRevision::new(8));
    assert!(receipt.runtime_status().reconciliation_required());
    assert_eq!(
        receipt
            .runtime_status()
            .failure()
            .expect("retained publication failure")
            .kind(),
        PortErrorKind::Unavailable
    );
    assert!(matches!(
        receipt.runtime_status(),
        ActionRoutingRuntimeStatus::CommandsRevoked { .. }
    ));
}

#[test]
fn action_routing_mutator_is_object_safe() {
    fn accepts_port(_: Option<Arc<dyn AutomationActionRoutingMutator>>) {}

    accepts_port(None);
}
