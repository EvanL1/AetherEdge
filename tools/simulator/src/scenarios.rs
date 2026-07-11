//! Scenario configuration loading and parsing.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Root scenario configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scenario {
    /// Scenario name for logging
    pub name: String,

    /// List of simulated devices
    pub devices: Vec<DeviceConfig>,

    /// Fault injection settings
    #[serde(default)]
    pub faults: FaultConfig,
}

/// Device configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceConfig {
    /// Device type (pcs, bms, pv, etc.)
    #[serde(rename = "type")]
    pub device_type: String,

    /// Modbus unit ID (slave address)
    pub unit_id: u8,

    /// Register configurations
    pub registers: Vec<RegisterConfig>,

    /// Coil configurations (FC01/FC05/FC0F)
    #[serde(default)]
    pub coils: Vec<CoilConfig>,

    /// Discrete input configurations (FC02, read-only)
    #[serde(default)]
    pub discrete_inputs: Vec<DiscreteInputConfig>,

    /// Optional state machine configuration
    #[serde(default)]
    pub state_machine: Option<StateMachineConfig>,

    /// CAN LYNK sender configuration (Linux only)
    #[serde(default)]
    pub can_lynk: Option<CanLynkConfig>,

    /// J1939 sender configuration (Linux only)
    #[serde(default)]
    pub j1939: Option<J1939SenderConfig>,
}

/// Coil configuration for initial state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoilConfig {
    /// Coil address (0-65535)
    pub address: u16,
    /// Initial value
    pub value: bool,
}

/// Discrete input configuration for initial state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscreteInputConfig {
    /// Discrete input address (0-65535)
    pub address: u16,
    /// Initial value
    pub value: bool,
}

/// Register configuration with waveform generator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterConfig {
    /// Register address (0-65535)
    pub address: u16,

    /// Optional register name for logging
    pub name: Option<String>,

    /// Waveform generator configuration
    pub generator: GeneratorConfig,
}

/// Waveform generator configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GeneratorConfig {
    /// Constant value
    Constant { value: f64 },

    /// Sine wave
    Sine {
        frequency: f64,
        amplitude: f64,
        offset: f64,
        #[serde(default)]
        phase: f64,
    },

    /// Square wave
    Square {
        frequency: f64,
        high: f64,
        low: f64,
        #[serde(default = "default_duty_cycle")]
        duty_cycle: f64,
    },

    /// Triangle wave
    Triangle { frequency: f64, min: f64, max: f64 },

    /// Random drift
    RandomDrift {
        center: f64,
        max_delta: f64,
        #[serde(default = "default_smoothness")]
        smoothness: f64,
    },

    /// Daily pattern (24-hour cycle)
    DailyPattern {
        peak_hour: u8,
        peak_value: f64,
        base_value: f64,
        #[serde(default = "default_spread_hours")]
        spread_hours: f64,
    },

    /// Noise generator
    Noise { mean: f64, std_dev: f64 },

    /// Linear ramp (for charging/discharging simulation)
    LinearRamp {
        /// Starting value
        start: f64,
        /// Ending value
        end: f64,
        /// Duration in seconds
        duration_sec: u64,
        /// Whether to loop the ramp (default: false)
        #[serde(default)]
        loop_mode: bool,
    },
}

fn default_duty_cycle() -> f64 {
    0.5
}

fn default_smoothness() -> f64 {
    0.9
}

fn default_spread_hours() -> f64 {
    4.0
}

/// Fault injection configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FaultConfig {
    /// Whether fault injection is enabled
    #[serde(default)]
    pub enabled: bool,

    /// List of fault scenarios
    #[serde(default)]
    pub scenarios: Vec<FaultScenario>,
}

/// Individual fault scenario.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FaultScenario {
    /// Drop connection
    ConnectionDrop {
        /// Probability of triggering (0.0 - 1.0)
        probability: f64,
        /// Duration in seconds
        duration_sec: u64,
    },

    /// Slow response
    SlowResponse {
        /// Probability of triggering (0.0 - 1.0)
        probability: f64,
        /// Delay in milliseconds
        delay_ms: u64,
    },

    /// Invalid response (protocol error)
    InvalidResponse {
        /// Probability of triggering (0.0 - 1.0)
        probability: f64,
    },

    /// No response (timeout)
    NoResponse {
        /// Probability of triggering (0.0 - 1.0)
        probability: f64,
    },
}

/// State machine configuration for a device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateMachineConfig {
    /// Initial state name (standby, running, fault, maintenance)
    #[serde(default = "default_initial_state")]
    pub initial_state: String,

    /// Transition rules
    #[serde(default)]
    pub transitions: Vec<TransitionConfig>,
}

/// A transition rule in YAML.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionConfig {
    /// Source state
    pub from: String,
    /// Target state
    pub to: String,
    /// Trigger type and parameters
    pub trigger: TriggerConfig,
}

/// Trigger configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TriggerConfig {
    Coil { address: u16, value: bool },
    Register { address: u16, value: u16 },
}

fn default_initial_state() -> String {
    "standby".to_string()
}

/// CAN LYNK sender configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanLynkConfig {
    /// vcan interface name (e.g., "vcan0")
    pub interface: String,
    /// Send interval in milliseconds
    #[serde(default = "default_can_interval")]
    pub interval_ms: u64,
}

/// J1939 sender configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct J1939SenderConfig {
    /// vcan interface name
    pub interface: String,
    /// ECU source address (default 0x00)
    #[serde(default)]
    pub source_address: u8,
    /// Send interval in milliseconds
    #[serde(default = "default_can_interval")]
    pub interval_ms: u64,
}

fn default_can_interval() -> u64 {
    1000
}

/// Load scenario from YAML file.
pub fn load_scenario(path: &Path) -> Result<Scenario> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read scenario file: {:?}", path))?;

    let scenario: Scenario = serde_yml::from_str(&content)
        .with_context(|| format!("Failed to parse scenario file: {:?}", path))?;

    Ok(scenario)
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_scenario() {
        let yaml = r#"
name: "Test Scenario"
devices:
  - type: pcs
    unit_id: 1
    registers:
      - address: 0
        name: "Total Power"
        generator:
          type: daily_pattern
          peak_hour: 14
          peak_value: 450.0
          base_value: 50.0
      - address: 10
        generator:
          type: sine
          frequency: 0.01
          amplitude: 20.0
          offset: 700.0
faults:
  enabled: false
"#;

        let scenario: Scenario = serde_yml::from_str(yaml).unwrap();
        assert_eq!(scenario.name, "Test Scenario");
        assert_eq!(scenario.devices.len(), 1);
        assert_eq!(scenario.devices[0].registers.len(), 2);
    }

    #[test]
    fn test_parse_faults() {
        let yaml = r#"
name: "Fault Test"
devices: []
faults:
  enabled: true
  scenarios:
    - type: connection_drop
      probability: 0.1
      duration_sec: 5
    - type: slow_response
      probability: 0.05
      delay_ms: 2000
"#;

        let scenario: Scenario = serde_yml::from_str(yaml).unwrap();
        assert!(scenario.faults.enabled);
        assert_eq!(scenario.faults.scenarios.len(), 2);
    }
}
