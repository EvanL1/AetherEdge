//! Validated device-control commands.

use crate::{ChannelCommandAddress, CommandId, DomainError, PointAddress, TimestampMs};

/// Default lifetime for a device-control command after interface acceptance.
pub const DEFAULT_COMMAND_TTL_MS: u64 = 5_000;

/// Device-side limits applied immediately before a command is dispatched.
///
/// `step` is measured from `minimum` when present, otherwise from zero.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CommandConstraints {
    minimum: Option<f64>,
    maximum: Option<f64>,
    step: Option<f64>,
}

impl CommandConstraints {
    /// Creates a validated constraint set.
    pub fn new(
        minimum: Option<f64>,
        maximum: Option<f64>,
        step: Option<f64>,
    ) -> Result<Self, DomainError> {
        if minimum.is_some_and(|value| !value.is_finite())
            || maximum.is_some_and(|value| !value.is_finite())
            || step.is_some_and(|value| !value.is_finite() || value <= 0.0)
            || matches!((minimum, maximum), (Some(min), Some(max)) if min > max)
        {
            return Err(DomainError::InvalidCommandConstraints);
        }

        Ok(Self {
            minimum,
            maximum,
            step,
        })
    }

    /// Creates an unconstrained value policy. Structural command checks still apply.
    #[must_use]
    pub const fn unbounded() -> Self {
        Self {
            minimum: None,
            maximum: None,
            step: None,
        }
    }

    /// Returns the inclusive lower bound.
    #[must_use]
    pub const fn minimum(self) -> Option<f64> {
        self.minimum
    }

    /// Returns the inclusive upper bound.
    #[must_use]
    pub const fn maximum(self) -> Option<f64> {
        self.maximum
    }

    /// Returns the allowed increment.
    #[must_use]
    pub const fn step(self) -> Option<f64> {
        self.step
    }

    /// Validates a finite command value against this range and step policy.
    pub fn validate_value(self, value: f64) -> Result<(), DomainError> {
        if !value.is_finite() {
            return Err(DomainError::NonFiniteCommandValue);
        }
        if self.minimum.is_some_and(|minimum| value < minimum)
            || self.maximum.is_some_and(|maximum| value > maximum)
        {
            return Err(DomainError::CommandValueOutOfRange);
        }

        if let Some(step) = self.step {
            let origin = self.minimum.unwrap_or(0.0);
            let offset = value - origin;
            let remainder = (offset % step).abs();
            let distance = if remainder < (step - remainder).abs() {
                remainder
            } else {
                (step - remainder).abs()
            };
            let magnitude = offset.abs();
            let representation_tolerance = f64::EPSILON * magnitude * 8.0;
            let base_tolerance = step * 1.0e-9;
            let candidate = if representation_tolerance > base_tolerance {
                representation_tolerance
            } else {
                base_tolerance
            };
            let maximum_tolerance = step * 1.0e-6;
            let tolerance = if candidate < maximum_tolerance {
                candidate
            } else {
                maximum_tolerance
            };
            if distance > tolerance {
                return Err(DomainError::CommandValueOffStep);
            }
        }

        Ok(())
    }
}

/// A validated request to change a writable device point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ControlCommand {
    id: CommandId,
    target: PointAddress,
    value: f64,
    issued_at: TimestampMs,
    expires_at: TimestampMs,
}

impl ControlCommand {
    /// Creates a command after validating that the target is writable.
    pub fn new(
        id: CommandId,
        target: PointAddress,
        value: f64,
        issued_at: TimestampMs,
        expires_at: TimestampMs,
    ) -> Result<Self, DomainError> {
        if !target.kind().is_writable() {
            return Err(DomainError::PointNotWritable(target.kind()));
        }
        if !value.is_finite() {
            return Err(DomainError::NonFiniteCommandValue);
        }
        if expires_at <= issued_at {
            return Err(DomainError::InvalidCommandWindow);
        }

        Ok(Self {
            id,
            target,
            value,
            issued_at,
            expires_at,
        })
    }

    /// Revalidates a command at a trust boundary immediately before dispatch.
    pub fn validate_at(
        self,
        now: TimestampMs,
        constraints: CommandConstraints,
    ) -> Result<(), DomainError> {
        if now >= self.expires_at {
            return Err(DomainError::CommandExpired);
        }
        constraints.validate_value(self.value)
    }

    /// Returns the caller-provided command correlation identifier.
    #[must_use]
    pub const fn id(self) -> CommandId {
        self.id
    }

    /// Returns the target point.
    #[must_use]
    pub const fn target(self) -> PointAddress {
        self.target
    }

    /// Returns the requested value.
    #[must_use]
    pub const fn value(self) -> f64 {
        self.value
    }

    /// Returns when the command was issued.
    #[must_use]
    pub const fn issued_at(self) -> TimestampMs {
        self.issued_at
    }

    /// Returns the exclusive command deadline.
    #[must_use]
    pub const fn expires_at(self) -> TimestampMs {
        self.expires_at
    }
}

/// A routed command addressed to one physical channel point.
///
/// Logical instance routing and configured limits are resolved before this
/// value is constructed. The physical sink still rechecks finiteness and the
/// exclusive deadline at its own trust boundary.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhysicalDeviceCommand {
    id: CommandId,
    target: ChannelCommandAddress,
    value: f64,
    issued_at: TimestampMs,
    expires_at: TimestampMs,
}

impl PhysicalDeviceCommand {
    /// Creates a finite physical command with a non-empty local transport window.
    pub fn new(
        id: CommandId,
        target: ChannelCommandAddress,
        value: f64,
        issued_at: TimestampMs,
        expires_at: TimestampMs,
    ) -> Result<Self, DomainError> {
        if !value.is_finite() {
            return Err(DomainError::NonFiniteCommandValue);
        }
        if expires_at <= issued_at {
            return Err(DomainError::InvalidCommandWindow);
        }
        Ok(Self {
            id,
            target,
            value,
            issued_at,
            expires_at,
        })
    }

    /// Rejects a command at or after its exclusive deadline.
    pub fn validate_at(self, now: TimestampMs) -> Result<(), DomainError> {
        if now >= self.expires_at {
            return Err(DomainError::CommandExpired);
        }
        Ok(())
    }

    /// Returns the end-to-end command correlation identifier.
    #[must_use]
    pub const fn id(self) -> CommandId {
        self.id
    }

    /// Returns the routed physical target.
    #[must_use]
    pub const fn target(self) -> ChannelCommandAddress {
        self.target
    }

    /// Returns the requested engineering value.
    #[must_use]
    pub const fn value(self) -> f64 {
        self.value
    }

    /// Returns the interface acceptance time.
    #[must_use]
    pub const fn issued_at(self) -> TimestampMs {
        self.issued_at
    }

    /// Returns the exclusive local command-transport deadline.
    #[must_use]
    pub const fn expires_at(self) -> TimestampMs {
        self.expires_at
    }
}
