//! Transport-neutral Aether use cases and safety policy.

mod acceptance;
mod action_routing;
mod alarm_rule;
mod alert_resolution;
mod capability;
mod channel_management;
mod channel_reconciliation;
mod context;
mod control;
mod edge;
mod error;
mod measurement_routing;
mod outbox_forwarder;
mod policy;
mod rule_execution;
mod rule_mutation;

pub use acceptance::{
    AcceptedOutcome, ActionRoutingMutationAcceptance, AlarmRuleMutationAcceptance,
    AlertResolutionAcceptance, ChannelMutationAcceptance, ChannelReconciliationAcceptance,
    CommandAcceptance, CompletionAuditStatus, MeasurementRoutingMutationAcceptance,
    RuleExecutionAcceptance, RuleMutationAcceptance,
};
pub use action_routing::ActionRoutingApplication;
pub use aether_domain::DEFAULT_COMMAND_TTL_MS;
pub use alarm_rule::AlarmRuleApplication;
pub use alert_resolution::AlertResolutionApplication;
pub use capability::{
    AuditPolicy, CapabilityDescriptor, ConfirmationPolicy, EXECUTE_RULE_CAPABILITY,
    MANAGE_ALARM_RULE_CAPABILITY, MANAGE_CHANNEL_CAPABILITY, MANAGE_INSTANCE_CAPABILITY,
    MANAGE_ROUTING_CAPABILITY, MANAGE_RULE_CAPABILITY, OperationKind, READ_POINT_CAPABILITY,
    RECONCILE_CHANNELS_CAPABILITY, RESOLVE_ALERT_CAPABILITY, RiskLevel, WRITE_POINT_CAPABILITY,
    capability_catalog,
};
pub use channel_management::ChannelManagementApplication;
pub use channel_reconciliation::ChannelReconciliationApplication;
pub use context::{Actor, RequestContext};
pub use control::ControlApplication;
pub use edge::EdgeApplication;
pub use error::ApplicationError;
pub use measurement_routing::MeasurementRoutingApplication;
pub use outbox_forwarder::{DrainReport, OutboxForwarder};
pub use policy::SafetyPolicy;
pub use rule_execution::RuleExecutionApplication;
pub use rule_mutation::RuleMutationApplication;
