//! Capability-oriented ports implemented by Aether extensions.

mod acquisition;
mod alarm;
mod audit;
mod automation;
mod channel;
mod clock;
mod control;
mod data_processing;
mod error;
mod history;
mod live_state;
mod mirror;
mod outbox;
mod uplink;

pub use acquisition::AcquisitionStateWriter;
pub use alarm::{
    AlarmRuleMutation, AlarmRuleMutationKind, AlarmRuleMutationReceipt, AlarmRuleMutator,
    AlarmRulePatch, AlertResolutionReceipt, AlertResolver,
};
pub use audit::{AuditOutcome, AuditRecord, AuditSink};
pub use automation::{
    ActionRoute, ActionRouteKey, ActionRoutingMutation, ActionRoutingMutationKind,
    ActionRoutingMutationReceipt, ActionRoutingTarget, AutomationActionRoutingMutator,
    AutomationRuleExecutor, AutomationRuleMutator, RuleExecutionReceipt, RuleMutation,
    RuleMutationKind, RuleMutationReceipt, RuleSchedulerRefreshStatus,
};
pub use channel::{
    ChannelDefinition, ChannelDesiredStateObservation, ChannelLoggingPolicy, ChannelMutation,
    ChannelMutationKind, ChannelMutationReceipt, ChannelMutator, ChannelParameterValue,
    ChannelParameters, ChannelPatch, ChannelReconciler, ChannelReconciliationItem,
    ChannelReconciliationReceipt, ChannelReconciliationScope, ChannelRevision,
    ChannelRuntimeProjection,
};
pub use clock::Clock;
pub use control::{CommandDispatcher, CommandReceipt, DeviceCommandSink};
pub use data_processing::{
    CovariateSource, CovariateWindow, DataBoundary, DataProcessor, DataProcessorDescriptor,
    HistoryQuery, HistoryWindow, ProcessorHealth, SourcedSegment,
};
pub use error::{PortError, PortErrorKind, PortResult};
pub use history::HistorySink;
pub use live_state::{LiveState, LiveStateWriter};
pub use mirror::StateMirror;
pub use outbox::{DurableOutbox, OutboxEntry, OutboxId, OutboxMessage};
pub use uplink::UplinkPublisher;
