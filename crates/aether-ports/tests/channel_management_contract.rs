use std::{collections::BTreeMap, sync::Arc};

use aether_domain::ChannelId;
use aether_ports::{
    ChannelDefinition, ChannelDesiredStateObservation, ChannelLoggingPolicy, ChannelMutation,
    ChannelMutationKind, ChannelMutationReceipt, ChannelMutator, ChannelParameterValue,
    ChannelParameters, ChannelPatch, ChannelReconciler, ChannelReconciliationItem,
    ChannelReconciliationReceipt, ChannelReconciliationScope, ChannelRevision,
    ChannelRuntimeProjection,
};

fn logging() -> ChannelLoggingPolicy {
    ChannelLoggingPolicy::default()
        .with_enabled(true)
        .with_level("debug")
        .with_file("/var/log/aether/channel-7.log")
}

fn parameters() -> ChannelParameters {
    BTreeMap::from([
        (
            "host".to_string(),
            ChannelParameterValue::String("192.0.2.10".to_string()),
        ),
        ("port".to_string(), ChannelParameterValue::Integer(502)),
        (
            "retry_backoff".to_string(),
            ChannelParameterValue::Array(vec![
                ChannelParameterValue::Float(0.5),
                ChannelParameterValue::Null,
            ]),
        ),
        (
            "tls".to_string(),
            ChannelParameterValue::Object(BTreeMap::from([(
                "enabled".to_string(),
                ChannelParameterValue::Bool(true),
            )])),
        ),
    ])
}

fn definition() -> ChannelDefinition {
    ChannelDefinition::new(
        Some(ChannelId::new(7)),
        "packaging-plc",
        "modbus_tcp",
        parameters(),
    )
    .with_description("primary packaging controller")
    .with_logging(logging())
    .with_enabled(true)
}

#[test]
fn channel_definition_preserves_transport_neutral_commissioning_data() {
    let definition = definition();

    assert_eq!(definition.requested_channel_id(), Some(ChannelId::new(7)));
    assert_eq!(definition.name(), "packaging-plc");
    assert_eq!(
        definition.description(),
        Some("primary packaging controller")
    );
    assert_eq!(definition.protocol(), "modbus_tcp");
    assert_eq!(definition.parameters(), &parameters());
    assert_eq!(definition.logging(), &logging());
    assert!(definition.enabled());
}

#[test]
fn logging_policy_has_transport_neutral_defaults_and_accessors() {
    let defaults = ChannelLoggingPolicy::default();
    assert!(!defaults.enabled());
    assert_eq!(defaults.level(), None);
    assert_eq!(defaults.file(), None);

    let configured = logging();
    assert!(configured.enabled());
    assert_eq!(configured.level(), Some("debug"));
    assert_eq!(configured.file(), Some("/var/log/aether/channel-7.log"));
}

#[test]
fn one_mutation_type_covers_create_update_delete_enable_and_disable() {
    let create = ChannelMutation::create(definition());
    assert_eq!(create.kind(), ChannelMutationKind::Create);
    assert_eq!(create.channel_id(), Some(ChannelId::new(7)));
    assert_eq!(create.definition(), Some(&definition()));

    let patch = ChannelPatch::new()
        .with_name("packaging-plc-2")
        .with_description("replacement controller")
        .with_protocol("modbus_tcp")
        .with_parameters(parameters())
        .with_logging(logging());
    let revision = ChannelRevision::new(3);
    let update = ChannelMutation::update_with_revision(ChannelId::new(7), revision, patch.clone());
    assert_eq!(update.kind(), ChannelMutationKind::Update);
    assert_eq!(update.channel_id(), Some(ChannelId::new(7)));
    assert_eq!(update.expected_revision(), Some(revision));
    assert_eq!(update.patch(), Some(&patch));
    assert_eq!(patch.logging(), Some(&logging()));

    let delete = ChannelMutation::delete_with_revision(ChannelId::new(7), revision);
    assert_eq!(delete.kind(), ChannelMutationKind::Delete);
    assert_eq!(delete.expected_revision(), Some(revision));

    let enable = ChannelMutation::enable_with_revision(ChannelId::new(7), revision);
    assert_eq!(enable.kind(), ChannelMutationKind::Enable);

    let disable = ChannelMutation::disable_with_revision(ChannelId::new(7), revision);
    assert_eq!(disable.kind(), ChannelMutationKind::Disable);

    assert_eq!(create.expected_revision(), None);

    assert_eq!(
        ChannelMutation::update(ChannelId::new(7), patch).expected_revision(),
        None
    );
}

#[test]
fn debug_output_redacts_parameter_values() {
    let definition_debug = format!("{:?}", definition());
    let value_debug = format!(
        "{:?}",
        ChannelParameterValue::String("never-log-this".to_string())
    );
    let object_debug = format!(
        "{:?}",
        ChannelParameterValue::Object(BTreeMap::from([(
            "never-log-this-key".to_string(),
            ChannelParameterValue::String("never-log-this-value".to_string()),
        )]))
    );

    assert!(!definition_debug.contains("192.0.2.10"));
    assert!(!definition_debug.contains("/var/log/aether/channel-7.log"));
    assert!(!value_debug.contains("never-log-this"));
    assert!(!object_debug.contains("never-log-this-key"));
    assert!(!object_debug.contains("never-log-this-value"));
}

#[test]
fn mutation_receipt_exposes_authoritative_revision_and_runtime_projection() {
    let receipt = ChannelMutationReceipt::new(
        ChannelId::new(7),
        ChannelMutationKind::Update,
        ChannelRevision::new(4),
        true,
        ChannelRuntimeProjection::Degraded,
    );

    assert_eq!(receipt.channel_id(), ChannelId::new(7));
    assert_eq!(receipt.kind(), ChannelMutationKind::Update);
    assert_eq!(receipt.resulting_revision(), ChannelRevision::new(4));
    assert!(receipt.desired_enabled());
    assert_eq!(
        receipt.runtime_projection(),
        ChannelRuntimeProjection::Degraded
    );
    assert!(receipt.reconciliation_required());
}

#[test]
fn runtime_projection_status_determines_reconciliation_requirement() {
    for (status, reconciliation_required) in [
        (ChannelRuntimeProjection::Stopped, false),
        (ChannelRuntimeProjection::ActivationPending, true),
        (ChannelRuntimeProjection::Active, false),
        (ChannelRuntimeProjection::Degraded, true),
        (ChannelRuntimeProjection::Removed, false),
    ] {
        let receipt = ChannelMutationReceipt::new(
            ChannelId::new(7),
            ChannelMutationKind::Update,
            ChannelRevision::new(4),
            true,
            status,
        );

        assert_eq!(receipt.reconciliation_required(), reconciliation_required);
    }
}

#[test]
fn channel_mutator_is_object_safe() {
    fn accepts_port(_: Option<Arc<dyn ChannelMutator>>) {}

    accepts_port(None);
}

#[test]
fn reconciliation_receipt_preserves_only_authoritative_desired_facts() {
    let present = ChannelReconciliationItem::new(
        ChannelId::new(7),
        ChannelDesiredStateObservation::present(ChannelRevision::new(4), true),
        ChannelRuntimeProjection::Active,
    );
    let absent = ChannelReconciliationItem::new(
        ChannelId::new(8),
        ChannelDesiredStateObservation::absent(Some(ChannelRevision::new(6))),
        ChannelRuntimeProjection::Removed,
    );
    let degraded = ChannelReconciliationItem::new(
        ChannelId::new(9),
        ChannelDesiredStateObservation::present(ChannelRevision::new(2), false),
        ChannelRuntimeProjection::Degraded,
    );
    let receipt = ChannelReconciliationReceipt::new(
        ChannelReconciliationScope::All,
        vec![degraded, absent, present],
    );

    assert_eq!(receipt.scope(), ChannelReconciliationScope::All);
    assert_eq!(
        receipt
            .items()
            .iter()
            .map(|item| item.channel_id().get())
            .collect::<Vec<_>>(),
        vec![7, 8, 9],
        "bulk receipts must be deterministic by channel identity"
    );
    assert_eq!(
        receipt.items()[0].desired_revision(),
        Some(ChannelRevision::new(4))
    );
    assert_eq!(receipt.items()[0].desired_enabled(), Some(true));
    assert_eq!(
        receipt.items()[1].desired_revision(),
        Some(ChannelRevision::new(6))
    );
    assert_eq!(receipt.items()[1].desired_enabled(), None);
    assert_eq!(receipt.items()[2].desired_enabled(), Some(false));
    assert_eq!(receipt.degraded_count(), 1);
    assert!(receipt.reconciliation_required());
}

#[test]
fn one_channel_reconciliation_scope_is_typed_and_port_is_object_safe() {
    let scope = ChannelReconciliationScope::One(ChannelId::new(42));
    assert_eq!(scope.channel_id(), Some(ChannelId::new(42)));
    assert!(!scope.is_all());
    assert!(ChannelReconciliationScope::All.is_all());

    fn accepts_port(_: Option<Arc<dyn ChannelReconciler>>) {}
    accepts_port(None);
}
