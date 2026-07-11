//! Device state machine for realistic simulation.
//!
//! Reacts to Modbus writes (coil/register) and transitions between
//! operational states (Standby, Running, Fault, Maintenance).

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use serde::Serialize;

/// Device operational state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceState {
    #[default]
    Standby,
    Running,
    Fault,
    Maintenance,
}

impl std::str::FromStr for DeviceState {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "standby" => Ok(Self::Standby),
            "running" => Ok(Self::Running),
            "fault" => Ok(Self::Fault),
            "maintenance" => Ok(Self::Maintenance),
            _ => Err(format!("unknown state: {s}")),
        }
    }
}

impl DeviceState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Standby => "standby",
            Self::Running => "running",
            Self::Fault => "fault",
            Self::Maintenance => "maintenance",
        }
    }
}

/// Trigger condition for a state transition.
#[derive(Debug, Clone)]
pub enum Trigger {
    /// Coil write: (address, expected_value)
    Coil { address: u16, value: bool },
    /// Register write: (address, expected_value)
    Register { address: u16, value: u16 },
}

/// A state transition rule.
#[derive(Debug, Clone)]
pub struct Transition {
    pub from: DeviceState,
    pub trigger: Trigger,
    pub to: DeviceState,
}

/// Per-device state machine.
pub struct StateMachine {
    state: RwLock<DeviceState>,
    transitions: Vec<Transition>,
}

impl StateMachine {
    pub fn new(initial: DeviceState, transitions: Vec<Transition>) -> Self {
        Self {
            state: RwLock::new(initial),
            transitions,
        }
    }

    pub fn current_state(&self) -> DeviceState {
        *self.state.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Called when a coil is written. Returns new state if transitioned.
    ///
    /// Uses a single write lock for the entire check-and-set to avoid TOCTOU races.
    pub fn on_coil_write(&self, address: u16, value: bool) -> Option<DeviceState> {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        for t in &self.transitions {
            if t.from == *state
                && let Trigger::Coil {
                    address: a,
                    value: v,
                } = &t.trigger
                && *a == address
                && *v == value
            {
                *state = t.to;
                return Some(t.to);
            }
        }
        None
    }

    /// Called when a register is written. Returns new state if transitioned.
    ///
    /// Uses a single write lock for the entire check-and-set to avoid TOCTOU races.
    pub fn on_register_write(&self, address: u16, value: u16) -> Option<DeviceState> {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        for t in &self.transitions {
            if t.from == *state
                && let Trigger::Register {
                    address: a,
                    value: v,
                } = &t.trigger
                && *a == address
                && *v == value
            {
                *state = t.to;
                return Some(t.to);
            }
        }
        None
    }
}

/// Global state machine store: unit_id -> StateMachine
pub type StateMachineStore = HashMap<u8, Arc<StateMachine>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_state() {
        let sm = StateMachine::new(DeviceState::Standby, vec![]);
        assert_eq!(sm.current_state(), DeviceState::Standby);
    }

    #[test]
    fn test_coil_transition() {
        let transitions = vec![
            Transition {
                from: DeviceState::Standby,
                trigger: Trigger::Coil {
                    address: 200,
                    value: true,
                },
                to: DeviceState::Running,
            },
            Transition {
                from: DeviceState::Running,
                trigger: Trigger::Coil {
                    address: 200,
                    value: false,
                },
                to: DeviceState::Standby,
            },
        ];
        let sm = StateMachine::new(DeviceState::Standby, transitions);

        // Wrong address — no transition
        assert_eq!(sm.on_coil_write(100, true), None);
        assert_eq!(sm.current_state(), DeviceState::Standby);

        // Correct trigger
        assert_eq!(sm.on_coil_write(200, true), Some(DeviceState::Running));
        assert_eq!(sm.current_state(), DeviceState::Running);

        // Turn off
        assert_eq!(sm.on_coil_write(200, false), Some(DeviceState::Standby));
    }

    #[test]
    fn test_register_transition() {
        let transitions = vec![Transition {
            from: DeviceState::Standby,
            trigger: Trigger::Register {
                address: 2000,
                value: 1,
            },
            to: DeviceState::Running,
        }];
        let sm = StateMachine::new(DeviceState::Standby, transitions);

        assert_eq!(sm.on_register_write(2000, 0), None);
        assert_eq!(sm.on_register_write(2000, 1), Some(DeviceState::Running));
    }

    #[test]
    fn test_no_transition_from_wrong_state() {
        let transitions = vec![Transition {
            from: DeviceState::Standby,
            trigger: Trigger::Coil {
                address: 200,
                value: true,
            },
            to: DeviceState::Running,
        }];
        let sm = StateMachine::new(DeviceState::Fault, transitions);

        // Trigger exists but current state is Fault, not Standby
        assert_eq!(sm.on_coil_write(200, true), None);
        assert_eq!(sm.current_state(), DeviceState::Fault);
    }
}
