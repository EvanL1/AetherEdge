//! Capability-oriented ports implemented by Aether extensions.

mod acquisition;
mod alarm;
mod audit;
mod automation;
mod channel;
mod channel_health;
mod clock;
mod cloudlink;
mod control;
mod data_processing;
mod error;
mod history;
mod integration;
mod live_state;
mod mirror;
mod outbox;
mod secret;
mod uplink;

pub use acquisition::AcquisitionStateWriter;
pub use alarm::{
    AlarmRuleMutation, AlarmRuleMutationKind, AlarmRuleMutationReceipt, AlarmRuleMutator,
    AlarmRulePatch, AlertResolutionReceipt, AlertResolver,
};
pub use audit::{AuditOutcome, AuditRecord, AuditSink};
pub use automation::{
    ActionRoute, ActionRouteKey, ActionRoutingMutation, ActionRoutingMutationKind,
    ActionRoutingMutationReceipt, ActionRoutingRuntimeStatus, ActionRoutingTarget,
    AutomationActionRoutingMutator, AutomationMeasurementRoutingMutator, AutomationRuleExecutor,
    AutomationRuleMutator, AutomationRulesRevision, LogicalRoutingRevision, MeasurementRoute,
    MeasurementRouteKey, MeasurementRoutingMutation, MeasurementRoutingMutationKind,
    MeasurementRoutingMutationReceipt, MeasurementRoutingRuntimeStatus, MeasurementRoutingTarget,
    RevisionedActionRoutingMutation, RevisionedRuleMutation, RuleExecutionReceipt, RuleMutation,
    RuleMutationKind, RuleMutationReceipt, RuleRuntimeStatus, RuleSchedulerRefreshStatus,
};
pub use channel::{
    ChannelDefinition, ChannelDesiredStateObservation, ChannelLoggingPolicy, ChannelMutation,
    ChannelMutationKind, ChannelMutationReceipt, ChannelMutator, ChannelParameterValue,
    ChannelParameters, ChannelPatch, ChannelReconciler, ChannelReconciliationItem,
    ChannelReconciliationReceipt, ChannelReconciliationScope, ChannelRevision,
    ChannelRuntimeProjection,
};
pub use channel_health::{ChannelHealthObservation, ChannelHealthSource};
pub use clock::Clock;
pub use cloudlink::{
    CloudLinkDataLossEvidence, CloudLinkDeliveryState, CloudLinkDurableAck, CloudLinkEnqueue,
    CloudLinkMessageKind, CloudLinkRecord, CloudLinkRecordIdentity, CloudLinkReplayWindow,
    CloudLinkSessionBinding, CloudLinkSpool, CloudLinkSpoolError, CloudLinkSpoolErrorReason,
    CloudLinkSpoolStatus, CloudLinkTransport, CloudLinkTransportEvent, CloudLinkTransportMessage,
    CloudLinkTransportRoute, DurableAckOutcome,
};
pub use control::{CommandDispatcher, CommandReceipt, CommandTopologyFence, DeviceCommandSink};
pub use data_processing::{
    CovariateSource, CovariateWindow, DataBoundary, DataProcessor, DataProcessorDescriptor,
    HistoryQuery, HistoryWindow, ProcessorHealth, SourcedSegment,
};
pub use error::{PortError, PortErrorKind, PortResult};
pub use history::HistorySink;
pub use integration::{
    DelegatedDeviceProvider, IntegrationProjectionChange, IntegrationProjectionQuery,
    IntegrationProjectionReceipt, IntegrationProjectionSink, IntegrationTopologyGenerationStore,
};
pub use live_state::{LiveState, LiveStateWriter};
pub use mirror::StateMirror;
pub use outbox::{DurableOutbox, OutboxEntry, OutboxId, OutboxMessage};
pub use secret::{SecretMaterial, SecretRef, SecretResolver};
pub use uplink::UplinkPublisher;
