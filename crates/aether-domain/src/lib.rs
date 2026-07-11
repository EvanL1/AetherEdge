//! Industry-neutral domain types for the Aether edge kernel.

#![no_std]

mod command;
mod error;
mod identity;
mod point;

pub use command::{CommandConstraints, ControlCommand, DEFAULT_COMMAND_TTL_MS};
pub use error::DomainError;
pub use identity::{CommandId, InstanceId, PointId, TimestampMs};
pub use point::{PointAddress, PointKind, PointQuality, PointSample};
