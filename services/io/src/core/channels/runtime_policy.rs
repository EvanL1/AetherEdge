use std::collections::HashMap;
use std::num::NonZeroU64;
use std::time::Duration;

use aether_config::io::MAX_CHANNEL_TIMING_MS;
use serde_json::Value;

use crate::error::{IoError, Result};
use crate::protocols::core::metadata::{ParameterMetadata, ParameterType};
use crate::runtime::reconnect::{AutoRecoveryPolicy, ReconnectPolicy};

pub(crate) const RUNTIME_PARAMETER_KEYS: &[&str] = &[
    "poll_interval_ms",
    "zero_data_threshold",
    "reconnect_max_attempts",
    "reconnect_initial_delay_ms",
    "reconnect_max_delay_ms",
    "reconnect_backoff_multiplier",
    "watchdog_recovery_cooldown_secs",
    "watchdog_max_recovery_rounds",
];

pub(crate) struct ChannelRuntimePolicy {
    pub poll_interval_ms: NonZeroU64,
    pub zero_data_threshold: u32,
    pub reconnect: ReconnectPolicy,
    pub auto_recovery: Option<AutoRecoveryPolicy>,
}

impl ChannelRuntimePolicy {
    pub(crate) fn parameter_metadata(default_poll_interval_ms: u64) -> Vec<ParameterMetadata> {
        vec![
            ParameterMetadata::optional(
                "poll_interval_ms",
                "Poll Interval (ms)",
                "Runtime polling interval",
                ParameterType::Integer,
                Value::from(default_poll_interval_ms),
            )
            .with_integer_range(1, MAX_CHANNEL_TIMING_MS),
            ParameterMetadata::optional(
                "zero_data_threshold",
                "Zero-data Threshold",
                "Consecutive empty polls before reconnect; 0 disables",
                ParameterType::Integer,
                Value::from(5),
            )
            .with_integer_range(0, u64::from(u32::MAX)),
            ParameterMetadata::optional(
                "reconnect_max_attempts",
                "Reconnect Attempts",
                "Maximum reconnect attempts; 0 means unlimited",
                ParameterType::Integer,
                Value::from(0),
            )
            .with_integer_range(0, u64::from(u32::MAX)),
            ParameterMetadata::optional(
                "reconnect_initial_delay_ms",
                "Initial Reconnect Delay (ms)",
                "Initial delay before reconnect",
                ParameterType::Integer,
                Value::from(1_000),
            )
            .with_integer_range(1, MAX_CHANNEL_TIMING_MS),
            ParameterMetadata::optional(
                "reconnect_max_delay_ms",
                "Maximum Reconnect Delay (ms)",
                "Maximum reconnect backoff delay",
                ParameterType::Integer,
                Value::from(60_000),
            )
            .with_integer_range(1, MAX_CHANNEL_TIMING_MS),
            ParameterMetadata::optional(
                "reconnect_backoff_multiplier",
                "Reconnect Backoff",
                "Finite reconnect delay multiplier of at least 1",
                ParameterType::Float,
                Value::from(2.0),
            ),
            ParameterMetadata::optional(
                "watchdog_recovery_cooldown_secs",
                "Recovery Cooldown (s)",
                "Cooldown between watchdog recovery rounds",
                ParameterType::Integer,
                Value::from(300),
            )
            .with_integer_range(1, MAX_CHANNEL_TIMING_MS / 1_000),
            ParameterMetadata::optional(
                "watchdog_max_recovery_rounds",
                "Recovery Rounds",
                "Maximum watchdog recovery rounds; 0 disables",
                ParameterType::Integer,
                Value::from(3),
            )
            .with_integer_range(0, u64::from(u32::MAX)),
        ]
    }

    pub fn compile(
        parameters: &HashMap<String, Value>,
        default_poll_interval_ms: u64,
    ) -> Result<Self> {
        let poll_interval_ms = bounded_u64(
            parameters,
            "poll_interval_ms",
            default_poll_interval_ms,
            1,
            MAX_CHANNEL_TIMING_MS,
        )?;
        let zero_data_threshold = bounded_u32(parameters, "zero_data_threshold", 5, 0, u32::MAX)?;
        let reconnect_max_attempts =
            bounded_u32(parameters, "reconnect_max_attempts", 0, 0, u32::MAX)?;
        let reconnect_initial_delay_ms = bounded_u64(
            parameters,
            "reconnect_initial_delay_ms",
            1_000,
            1,
            MAX_CHANNEL_TIMING_MS,
        )?;
        let reconnect_max_delay_ms = bounded_u64(
            parameters,
            "reconnect_max_delay_ms",
            60_000,
            1,
            MAX_CHANNEL_TIMING_MS,
        )?;
        if reconnect_initial_delay_ms > reconnect_max_delay_ms {
            return Err(IoError::config(
                "'reconnect_initial_delay_ms' must not exceed 'reconnect_max_delay_ms'",
            ));
        }
        let reconnect_backoff_multiplier =
            finite_number(parameters, "reconnect_backoff_multiplier", 2.0)?;
        if reconnect_backoff_multiplier < 1.0 {
            return Err(IoError::config(
                "'reconnect_backoff_multiplier' must be at least 1",
            ));
        }

        let recovery_cooldown_secs = bounded_u64(
            parameters,
            "watchdog_recovery_cooldown_secs",
            300,
            1,
            MAX_CHANNEL_TIMING_MS / 1_000,
        )?;
        let recovery_rounds =
            bounded_u32(parameters, "watchdog_max_recovery_rounds", 3, 0, u32::MAX)?;

        Ok(Self {
            poll_interval_ms: NonZeroU64::new(poll_interval_ms)
                .ok_or_else(|| IoError::config("'poll_interval_ms' must be greater than zero"))?,
            zero_data_threshold,
            reconnect: ReconnectPolicy::from_config(
                reconnect_max_attempts,
                reconnect_initial_delay_ms,
                reconnect_max_delay_ms,
                reconnect_backoff_multiplier,
            ),
            auto_recovery: (recovery_rounds > 0).then_some(AutoRecoveryPolicy {
                cooldown: Duration::from_secs(recovery_cooldown_secs),
                max_recovery_rounds: recovery_rounds,
            }),
        })
    }
}

fn bounded_u64(
    parameters: &HashMap<String, Value>,
    name: &str,
    default: u64,
    minimum: u64,
    maximum: u64,
) -> Result<u64> {
    let value = match parameters.get(name) {
        Some(value) => value
            .as_u64()
            .ok_or_else(|| IoError::config(format!("'{name}' must be an unsigned integer")))?,
        None => default,
    };
    if !(minimum..=maximum).contains(&value) {
        return Err(IoError::config(format!(
            "'{name}' must be between {minimum} and {maximum}"
        )));
    }
    Ok(value)
}

fn bounded_u32(
    parameters: &HashMap<String, Value>,
    name: &str,
    default: u32,
    minimum: u32,
    maximum: u32,
) -> Result<u32> {
    let value = bounded_u64(
        parameters,
        name,
        u64::from(default),
        u64::from(minimum),
        u64::from(maximum),
    )?;
    u32::try_from(value).map_err(|_| IoError::config(format!("'{name}' exceeds the u32 range")))
}

fn finite_number(parameters: &HashMap<String, Value>, name: &str, default: f64) -> Result<f64> {
    let value = match parameters.get(name) {
        Some(value) => value
            .as_f64()
            .ok_or_else(|| IoError::config(format!("'{name}' must be a number")))?,
        None => default,
    };
    if !value.is_finite() {
        return Err(IoError::config(format!("'{name}' must be finite")));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::ChannelRuntimePolicy;

    #[test]
    fn defaults_preserve_the_existing_runtime_behavior() {
        let policy = ChannelRuntimePolicy::compile(&HashMap::new(), 1_000).expect("default policy");

        assert_eq!(policy.poll_interval_ms.get(), 1_000);
        assert_eq!(policy.zero_data_threshold, 5);
        assert_eq!(policy.reconnect.max_attempts, 0);
        assert_eq!(policy.reconnect.initial_delay.as_millis(), 1_000);
        assert_eq!(policy.reconnect.max_delay.as_millis(), 60_000);
        assert_eq!(policy.reconnect.backoff_multiplier, 2.0);
        let recovery = policy.auto_recovery.expect("default auto recovery");
        assert_eq!(recovery.cooldown.as_secs(), 300);
        assert_eq!(recovery.max_recovery_rounds, 3);
    }

    #[test]
    fn runtime_parameters_fail_closed_instead_of_truncating_or_defaulting() {
        for parameters in [
            HashMap::from([("zero_data_threshold".to_owned(), json!("5"))]),
            HashMap::from([(
                "reconnect_max_attempts".to_owned(),
                json!(u64::from(u32::MAX) + 1),
            )]),
            HashMap::from([("reconnect_initial_delay_ms".to_owned(), json!(0))]),
            HashMap::from([("reconnect_backoff_multiplier".to_owned(), json!(0.5))]),
            HashMap::from([(
                "watchdog_max_recovery_rounds".to_owned(),
                json!(u64::from(u32::MAX) + 1),
            )]),
        ] {
            assert!(ChannelRuntimePolicy::compile(&parameters, 1_000).is_err());
        }
    }

    #[test]
    fn runtime_policy_rejects_inverted_backoff_and_can_disable_recovery() {
        let inverted = HashMap::from([
            ("reconnect_initial_delay_ms".to_owned(), json!(2_000)),
            ("reconnect_max_delay_ms".to_owned(), json!(1_000)),
        ]);
        assert!(ChannelRuntimePolicy::compile(&inverted, 1_000).is_err());

        let disabled = HashMap::from([("watchdog_max_recovery_rounds".to_owned(), json!(0))]);
        assert!(
            ChannelRuntimePolicy::compile(&disabled, 1_000)
                .expect("disabled recovery is valid")
                .auto_recovery
                .is_none()
        );
    }
}
