//! Industry-neutral domain types for the Aether edge kernel.

#![no_std]

extern crate alloc;

mod alarm;
mod command;
mod data_processing;
mod error;
mod identity;
mod point;

pub use alarm::{AlarmComparator, AlarmRuleDefinition, AlarmRuleTarget, AlarmSeverity};
pub use command::{
    CommandConstraints, ControlCommand, DEFAULT_COMMAND_TTL_MS, PhysicalDeviceCommand,
};
pub use data_processing::{
    ArtifactProvenance, ArtifactSelector, BindingIdentity, DataProcessingRequest,
    DataProcessingTask, DerivedData, FallbackInfo, FallbackPolicy, FeatureDefinition, FeatureRole,
    FeatureValue, FeatureValueType, ForecastOptions, ForecastOutput, ForecastPoint,
    ForecastQuantile, ForecastTarget, ForecastTaskSpec, FrameQuality, HistoryAggregation,
    HistoryDuplicatePolicy, HistoryFeaturePolicy, NumericFeatureConstraints, ProcessTaskRequest,
    ProcessingFrame, ProcessingOptions, ProcessingOutput, ProcessingResult, ProcessingStatus,
    ProcessorProvenance, SampleQuality, Segment, SegmentKind, Series, SourceKind, SourceProvenance,
    StaticFeature, TaskIdentity, TaskKind, UnavailableInfo, is_semantic_source_ref,
    maximum_observation_gap,
};
pub use error::DomainError;
pub use identity::{
    AlarmRuleId, AlertId, ChannelId, CommandId, InstanceId, PointId, RuleId, TimestampMs,
};
pub use point::{
    AcquiredPointSample, ChannelCommandAddress, ChannelPointAddress, PointAddress, PointKind,
    PointQuality, PointSample,
};
