//! Industry-neutral domain types for the Aether edge kernel.

#![no_std]

extern crate alloc;

mod alarm;
mod command;
mod error;
mod identity;
mod point;

pub use alarm::{AlarmComparator, AlarmRuleDefinition, AlarmRuleTarget, AlarmSeverity};
pub use command::{
    CommandConstraints, ControlCommand, DEFAULT_COMMAND_TTL_MS, PhysicalDeviceCommand,
};
pub use error::DomainError;
pub use identity::{
    AlarmRuleId, AlertId, ChannelId, CommandId, InstanceId, InstanceName, InstanceNameError,
    PointId, RuleId, TimestampMs,
};
pub use point::{
    AcquiredPointSample, ChannelCommandAddress, ChannelPointAddress, PointAddress, PointKind,
    PointQuality, PointSample,
};
