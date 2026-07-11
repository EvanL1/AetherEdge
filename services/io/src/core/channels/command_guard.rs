//! Final device-command validation at the IO trust boundary.

use std::collections::HashMap;
use std::fmt;

use aether_domain::{
    CommandConstraints, CommandId, ControlCommand, DomainError, InstanceId, PointAddress, PointId,
    PointKind, TimestampMs,
};
use aether_model::PointType;

use crate::core::config::RuntimeChannelConfig;

/// A command rejected before any protocol adapter can touch hardware.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandGuardError {
    /// The command targets no configured writable point of the requested type.
    UnknownWritablePoint {
        point_type: PointType,
        point_id: u32,
    },
    /// A domain-level command invariant was violated.
    InvalidCommand(DomainError),
}

impl fmt::Display for CommandGuardError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownWritablePoint {
                point_type,
                point_id,
            } => write!(
                formatter,
                "unknown writable point {}:{}",
                point_type.as_str(),
                point_id
            ),
            Self::InvalidCommand(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for CommandGuardError {}

impl From<DomainError> for CommandGuardError {
    fn from(error: DomainError) -> Self {
        Self::InvalidCommand(error)
    }
}

/// Immutable per-channel safety policy used by the unified channel task.
pub(crate) struct CommandGuard {
    channel_id: u32,
    controls: HashMap<u32, CommandConstraints>,
    adjustments: HashMap<u32, CommandConstraints>,
}

impl CommandGuard {
    /// Builds the final-dispatch policy from the same runtime configuration as
    /// the protocol adapter. Invalid point constraints fail channel creation.
    pub fn from_runtime(config: &RuntimeChannelConfig) -> Result<Self, CommandGuardError> {
        let controls = config
            .control_points
            .iter()
            .map(|point| {
                let minimum = f64::from(point.on_value.min(point.off_value));
                let maximum = f64::from(point.on_value.max(point.off_value));
                let step = (maximum > minimum).then_some(maximum - minimum);
                CommandConstraints::new(Some(minimum), Some(maximum), step)
                    .map(|constraints| (point.base.point_id, constraints))
                    .map_err(CommandGuardError::from)
            })
            .collect::<Result<HashMap<_, _>, _>>()?;
        let adjustments = config
            .adjustment_points
            .iter()
            .map(|point| {
                CommandConstraints::new(point.min_value, point.max_value, Some(point.step))
                    .map(|constraints| (point.base.point_id, constraints))
                    .map_err(CommandGuardError::from)
            })
            .collect::<Result<HashMap<_, _>, _>>()?;

        Ok(Self {
            channel_id: config.id(),
            controls,
            adjustments,
        })
    }

    /// Validates one command at the last boundary before protocol dispatch.
    pub fn validate(
        &self,
        point_type: PointType,
        point_id: u32,
        value: f64,
        issued_at_ms: i64,
        expires_at_ms: i64,
        now_ms: i64,
    ) -> Result<(), CommandGuardError> {
        let constraints = match point_type {
            PointType::Control => self.controls.get(&point_id),
            PointType::Adjustment => self.adjustments.get(&point_id),
            _ => None,
        }
        .copied()
        .ok_or(CommandGuardError::UnknownWritablePoint {
            point_type,
            point_id,
        })?;

        let issued_at = u64::try_from(issued_at_ms)
            .map(TimestampMs::new)
            .map_err(|_| DomainError::InvalidCommandWindow)?;
        let expires_at = u64::try_from(expires_at_ms)
            .map(TimestampMs::new)
            .map_err(|_| DomainError::InvalidCommandWindow)?;
        let now = u64::try_from(now_ms)
            .map(TimestampMs::new)
            .map_err(|_| DomainError::InvalidCommandWindow)?;
        let target = PointAddress::new(
            InstanceId::new(self.channel_id),
            PointKind::Action,
            PointId::new(point_id),
        );
        let command = ControlCommand::new(CommandId::new(0), target, value, issued_at, expires_at)?;
        command.validate_at(now, constraints)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guard() -> CommandGuard {
        CommandGuard {
            channel_id: 7,
            controls: HashMap::from([(
                1,
                CommandConstraints::new(Some(0.0), Some(1.0), Some(1.0)).unwrap(),
            )]),
            adjustments: HashMap::from([(
                2,
                CommandConstraints::new(Some(-10.0), Some(10.0), Some(0.5)).unwrap(),
            )]),
        }
    }

    #[test]
    fn rejects_unknown_expired_out_of_range_and_off_step_commands() {
        let guard = guard();
        assert!(matches!(
            guard.validate(PointType::Control, 99, 1.0, 100, 200, 150),
            Err(CommandGuardError::UnknownWritablePoint { .. })
        ));
        assert_eq!(
            guard.validate(PointType::Control, 1, 1.0, 100, 200, 200),
            Err(CommandGuardError::InvalidCommand(
                DomainError::CommandExpired
            ))
        );
        assert_eq!(
            guard.validate(PointType::Adjustment, 2, 10.5, 100, 200, 150),
            Err(CommandGuardError::InvalidCommand(
                DomainError::CommandValueOutOfRange
            ))
        );
        assert_eq!(
            guard.validate(PointType::Adjustment, 2, 1.25, 100, 200, 150),
            Err(CommandGuardError::InvalidCommand(
                DomainError::CommandValueOffStep
            ))
        );
    }

    #[test]
    fn accepts_control_and_adjustment_values_inside_policy() {
        let guard = guard();
        assert_eq!(
            guard.validate(PointType::Control, 1, 1.0, 100, 200, 150),
            Ok(())
        );
        assert_eq!(
            guard.validate(PointType::Adjustment, 2, -9.5, 100, 200, 150),
            Ok(())
        );
    }
}
