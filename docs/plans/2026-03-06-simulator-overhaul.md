# Simulator Overhaul: State Machine + CAN/J1939 + HTTP Monitor

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Extend the existing Modbus TCP/RTU simulator with device state machines, CAN LYNK/J1939 frame senders, and an HTTP status API for E2E test monitoring.

**Architecture:** The simulator binary gains three new capabilities: (1) a `StateMachine` that reacts to Modbus writes (FC05/FC06) and drives register generators per-state, (2) CAN frame senders (`#[cfg(target_os = "linux")]`) that emit LYNK and J1939 frames onto vcan interfaces, (3) an axum HTTP server for querying device state during E2E tests. All features are additive — existing Modbus behavior is unchanged when no `state_machine` YAML is configured.

**Tech Stack:** Rust, tokio, axum (HTTP), socketcan (CAN, Linux-only), serde_yml (YAML config), voltage_j1939 (J1939 frame building)

---

## Task 1: State Machine Core (`state_machine.rs`)

**Files:**
- Create: `tools/simulator/src/state_machine.rs`
- Modify: `tools/simulator/src/main.rs` (add `mod state_machine;`)
- Test: inline `#[cfg(test)]` in `state_machine.rs`

**Step 1: Write the failing test**

Add to `tools/simulator/src/state_machine.rs`:

```rust
//! Device state machine for realistic simulation.
//!
//! Reacts to Modbus writes (coil/register) and transitions between
//! operational states (Standby, Running, Fault, etc.).

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Device operational state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DeviceState {
    #[default]
    Standby,
    Running,
    Fault,
    Maintenance,
}

impl DeviceState {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "standby" => Some(Self::Standby),
            "running" => Some(Self::Running),
            "fault" => Some(Self::Fault),
            "maintenance" => Some(Self::Maintenance),
            _ => None,
        }
    }

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
        *self.state.read().expect("state lock poisoned")
    }

    /// Called when a coil is written. Returns new state if transitioned.
    pub fn on_coil_write(&self, address: u16, value: bool) -> Option<DeviceState> {
        let current = self.current_state();
        for t in &self.transitions {
            if t.from == current {
                if let Trigger::Coil { address: a, value: v } = &t.trigger {
                    if *a == address && *v == value {
                        let mut s = self.state.write().expect("state lock poisoned");
                        *s = t.to;
                        return Some(t.to);
                    }
                }
            }
        }
        None
    }

    /// Called when a register is written. Returns new state if transitioned.
    pub fn on_register_write(&self, address: u16, value: u16) -> Option<DeviceState> {
        let current = self.current_state();
        for t in &self.transitions {
            if t.from == current {
                if let Trigger::Register { address: a, value: v } = &t.trigger {
                    if *a == address && *v == value {
                        let mut s = self.state.write().expect("state lock poisoned");
                        *s = t.to;
                        return Some(t.to);
                    }
                }
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
                trigger: Trigger::Coil { address: 200, value: true },
                to: DeviceState::Running,
            },
            Transition {
                from: DeviceState::Running,
                trigger: Trigger::Coil { address: 200, value: false },
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
            trigger: Trigger::Register { address: 2000, value: 1 },
            to: DeviceState::Running,
        }];
        let sm = StateMachine::new(DeviceState::Standby, transitions);

        assert_eq!(sm.on_register_write(2000, 0), None);
        assert_eq!(sm.on_register_write(2000, 1), Some(DeviceState::Running));
    }
}
```

**Step 2: Run test to verify it passes**

```bash
cargo test -p simulator -- state_machine
```

Expected: PASS (all 3 tests)

**Step 3: Add mod declaration**

In `tools/simulator/src/main.rs`, add after `mod writable;`:

```rust
mod state_machine;
```

**Step 4: Run full build check**

```bash
cargo check -p simulator
```

Expected: PASS

**Step 5: Commit**

```bash
git add tools/simulator/src/state_machine.rs tools/simulator/src/main.rs
git commit -m "feat(simulator): add device state machine core"
```

---

## Task 2: YAML Config for State Machine

**Files:**
- Modify: `tools/simulator/src/scenarios.rs` (add state_machine config)
- Test: inline `#[cfg(test)]` in `scenarios.rs`

**Step 1: Add state machine config types to `scenarios.rs`**

Add after `FaultScenario` enum:

```rust
/// State machine configuration for a device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateMachineConfig {
    /// Initial state name
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
```

Add to `DeviceConfig`:

```rust
    /// Optional state machine configuration
    #[serde(default)]
    pub state_machine: Option<StateMachineConfig>,
```

**Step 2: Add test for state machine YAML parsing**

```rust
#[test]
fn test_parse_state_machine() {
    let yaml = r#"
name: "State Machine Test"
devices:
  - type: pcs
    unit_id: 1
    registers: []
    state_machine:
      initial_state: standby
      transitions:
        - from: standby
          to: running
          trigger:
            type: coil
            address: 200
            value: true
        - from: running
          to: standby
          trigger:
            type: register
            address: 2000
            value: 0
faults:
  enabled: false
"#;

    let scenario: Scenario = serde_yml::from_str(yaml).unwrap();
    let sm = scenario.devices[0].state_machine.as_ref().unwrap();
    assert_eq!(sm.initial_state, "standby");
    assert_eq!(sm.transitions.len(), 2);
}
```

**Step 3: Run tests**

```bash
cargo test -p simulator -- test_parse_state_machine
```

Expected: PASS

**Step 4: Commit**

```bash
git add tools/simulator/src/scenarios.rs
git commit -m "feat(simulator): add state machine YAML config schema"
```

---

## Task 3: Wire State Machine into Server

**Files:**
- Modify: `tools/simulator/src/server.rs` (hook coil/register writes)
- Modify: `tools/simulator/src/main.rs` (build state machines)

**Step 1: Add state machine builder to `main.rs`**

In `main.rs`, after `let device_map = devices::build_device_map(...)`:

```rust
use state_machine::{DeviceState, StateMachine, StateMachineStore, Transition, Trigger};
use scenarios::{TriggerConfig, StateMachineConfig};

// Build state machines from scenario config
let mut sm_store = StateMachineStore::new();
for device in &scenario.devices {
    if let Some(ref sm_config) = device.state_machine {
        let initial = DeviceState::from_str(&sm_config.initial_state)
            .unwrap_or_default();
        let transitions: Vec<Transition> = sm_config.transitions.iter().filter_map(|t| {
            let from = DeviceState::from_str(&t.from)?;
            let to = DeviceState::from_str(&t.to)?;
            let trigger = match &t.trigger {
                TriggerConfig::Coil { address, value } => {
                    Trigger::Coil { address: *address, value: *value }
                }
                TriggerConfig::Register { address, value } => {
                    Trigger::Register { address: *address, value: *value }
                }
            };
            Some(Transition { from, trigger, to })
        }).collect();
        sm_store.insert(device.unit_id, std::sync::Arc::new(StateMachine::new(initial, transitions)));
        info!("State machine for unit {}: initial={}", device.unit_id, sm_config.initial_state);
    }
}
```

Then pass `sm_store` to `run_server`.

**Step 2: Modify `server.rs` to accept and use state machines**

Add `sm_store: StateMachineStore` to `run_server` params. Wrap it in `Arc`. In `process_request`:
- After `FC_WRITE_SINGLE_COIL` succeeds: call `sm.on_coil_write(addr, value)`
- After `FC_WRITE_SINGLE_REGISTER` succeeds: call `sm.on_register_write(addr, value)`

Add to `run_server` signature:

```rust
pub async fn run_server(
    addr: &str,
    device_map: DeviceMap,
    faults: FaultConfig,
    devices: &[DeviceConfig],
    sm_store: StateMachineStore,   // NEW
) -> Result<()> {
```

Add to `process_request` signature:

```rust
fn process_request(
    ...
    sm_store: &StateMachineStore,   // NEW
) -> Vec<u8> {
```

In FC05 handler (after `coil_store.write_coil`):

```rust
if let Some(sm) = sm_store.get(&unit_id) {
    if let Some(new_state) = sm.on_coil_write(addr, value) {
        info!("Unit {} state: {:?}", unit_id, new_state);
    }
}
```

In FC06 handler (after `writable.write_single`):

```rust
if let Some(sm) = sm_store.get(&unit_id) {
    if let Some(new_state) = sm.on_register_write(addr, value) {
        info!("Unit {} state: {:?}", unit_id, new_state);
    }
}
```

**Step 3: Build check**

```bash
cargo check -p simulator
```

**Step 4: Commit**

```bash
git add tools/simulator/src/main.rs tools/simulator/src/server.rs
git commit -m "feat(simulator): wire state machine into Modbus server"
```

---

## Task 4: HTTP Status API (`http_api.rs`)

**Files:**
- Create: `tools/simulator/src/http_api.rs`
- Modify: `tools/simulator/src/main.rs` (start HTTP server, add mod)
- Modify: `tools/simulator/Cargo.toml` (add axum + serde_json)

**Step 1: Add dependencies**

In `tools/simulator/Cargo.toml`:

```toml
# HTTP monitoring API
axum = "0.8"
serde_json = "1.0"
```

**Step 2: Create `http_api.rs`**

```rust
//! HTTP monitoring API for E2E test observability.
//!
//! Provides endpoints for querying device state during tests:
//! - GET /state         → all device states
//! - GET /state/:unit   → single device state
//! - GET /health        → simulator health check

use crate::coils::CoilStore;
use crate::state_machine::StateMachineStore;
use crate::writable::WritableRegisters;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub sm_store: Arc<StateMachineStore>,
    pub coil_store: Arc<CoilStore>,
    pub writable: Arc<WritableRegisters>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/state", get(all_states))
        .route("/state/{unit_id}", get(device_state))
        .with_state(state)
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

async fn all_states(State(state): State<AppState>) -> Json<Value> {
    let mut devices = serde_json::Map::new();
    for (unit_id, sm) in state.sm_store.iter() {
        devices.insert(
            unit_id.to_string(),
            json!({ "state": sm.current_state().as_str() }),
        );
    }
    Json(json!({ "devices": devices }))
}

async fn device_state(
    State(state): State<AppState>,
    Path(unit_id): Path<u8>,
) -> Result<Json<Value>, StatusCode> {
    if let Some(sm) = state.sm_store.get(&unit_id) {
        Ok(Json(json!({
            "unit_id": unit_id,
            "state": sm.current_state().as_str(),
        })))
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

/// Start the HTTP monitoring server on the given port.
pub async fn start_http_server(port: u16, state: AppState) {
    let app = router(state);
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port))
        .await
        .expect("failed to bind HTTP monitoring port");
    tracing::info!("HTTP monitoring API on port {}", port);
    axum::serve(listener, app).await.ok();
}
```

**Step 3: Wire into `main.rs`**

Add CLI arg:

```rust
    /// HTTP monitoring port (0 to disable)
    #[arg(long, default_value = "0")]
    http_port: u16,
```

In `main()`, after building `sm_store`, before starting server:

```rust
if args.http_port > 0 {
    let http_state = http_api::AppState {
        sm_store: Arc::new(sm_store.clone()),
        coil_store: Arc::clone(&coil_store_for_http),
        writable: Arc::clone(&writable_for_http),
    };
    tokio::spawn(http_api::start_http_server(args.http_port, http_state));
}
```

Note: This requires restructuring `main.rs` slightly so that `CoilStore` and `WritableRegisters` are created in `main()` rather than inside `run_server()`. Move their creation up.

**Step 4: Add mod declaration**

```rust
mod http_api;
```

**Step 5: Build check**

```bash
cargo check -p simulator
```

**Step 6: Commit**

```bash
git add tools/simulator/Cargo.toml tools/simulator/src/http_api.rs tools/simulator/src/main.rs
git commit -m "feat(simulator): add HTTP monitoring API for E2E observability"
```

---

## Task 5: CAN LYNK Simulator (`can_sender.rs`)

**Files:**
- Create: `tools/simulator/src/can_sender.rs`
- Modify: `tools/simulator/Cargo.toml` (add socketcan dep, linux-only)
- Modify: `tools/simulator/src/main.rs` (add mod, start sender)
- Modify: `tools/simulator/src/scenarios.rs` (add CAN config)

**Step 1: Add socketcan dependency (Linux-only)**

In `tools/simulator/Cargo.toml`:

```toml
# CAN bus (Linux only)
[target.'cfg(target_os = "linux")'.dependencies]
socketcan = "3.3"
```

**Step 2: Add CAN config to `scenarios.rs`**

Add to `DeviceConfig`:

```rust
    /// CAN LYNK sender configuration (Linux only)
    #[serde(default)]
    pub can_lynk: Option<CanLynkConfig>,
```

Add new struct:

```rust
/// CAN LYNK sender configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanLynkConfig {
    /// vcan interface name (e.g., "vcan0")
    pub interface: String,
    /// Send interval in milliseconds
    #[serde(default = "default_can_interval")]
    pub interval_ms: u64,
}

fn default_can_interval() -> u64 {
    1000
}
```

**Step 3: Create `can_sender.rs`**

```rust
//! CAN LYNK frame sender for E2E testing.
//!
//! Sends standard 11-bit CAN frames matching the LYNK protocol
//! (0x351 BatteryLimits, 0x355 BatteryStatus, 0x356 BatteryMeasurements).
//! Only compiled on Linux where SocketCAN is available.

#![cfg(target_os = "linux")]

use socketcan::{CanFrame, CanSocket, EmbeddedFrame, Frame, Socket, StandardId};
use std::sync::Arc;
use tokio::time::{interval, Duration};
use tracing::{error, info, warn};

use crate::state_machine::{DeviceState, StateMachine};

/// LYNK CAN IDs
const BATTERY_LIMITS: u16 = 0x351;
const BATTERY_STATUS: u16 = 0x355;
const BATTERY_MEASUREMENTS: u16 = 0x356;

/// Start a CAN LYNK sender task.
///
/// Periodically sends battery status frames onto the given vcan interface.
/// Frame contents vary based on the device's current state.
pub async fn start_lynk_sender(
    interface: String,
    interval_ms: u64,
    state_machine: Option<Arc<StateMachine>>,
) {
    let socket = match CanSocket::open(&interface) {
        Ok(s) => s,
        Err(e) => {
            warn!("Cannot open CAN interface {}: {} (CAN simulation disabled)", interface, e);
            return;
        }
    };

    info!("CAN LYNK sender started on {} (interval={}ms)", interface, interval_ms);

    let mut tick = interval(Duration::from_millis(interval_ms));
    let mut cycle: u64 = 0;

    loop {
        tick.tick().await;
        cycle += 1;

        let state = state_machine
            .as_ref()
            .map(|sm| sm.current_state())
            .unwrap_or(DeviceState::Standby);

        // Generate frame data based on state
        let (soc, voltage, current, temp) = match state {
            DeviceState::Standby => (50u16, 480u16, 0i16, 25i16),
            DeviceState::Running => {
                // Simulate slight variations
                let v = 490 + ((cycle % 10) as u16);
                let c = 100 + ((cycle % 20) as i16);
                (65, v, c, 30)
            }
            DeviceState::Fault => (20, 400, 0, 45),
            DeviceState::Maintenance => (80, 520, 0, 22),
        };

        // 0x355: Battery Status — SOC (u16 LE, byte 0-1), SOH (u16 LE, byte 2-3)
        let soh: u16 = 98;
        let frame_355 = build_standard_frame(BATTERY_STATUS, &[
            (soc & 0xFF) as u8, (soc >> 8) as u8,
            (soh & 0xFF) as u8, (soh >> 8) as u8,
            0, 0, 0, 0,
        ]);

        // 0x356: Battery Measurements — voltage (u16 LE), current (i16 LE), temp (i16 LE)
        let frame_356 = build_standard_frame(BATTERY_MEASUREMENTS, &[
            (voltage & 0xFF) as u8, (voltage >> 8) as u8,
            (current as u16 & 0xFF) as u8, ((current as u16) >> 8) as u8,
            (temp as u16 & 0xFF) as u8, ((temp as u16) >> 8) as u8,
            0, 0,
        ]);

        // 0x351: Battery Limits — charge voltage (u16 LE), charge current (u16 LE), etc.
        let frame_351 = build_standard_frame(BATTERY_LIMITS, &[
            0xE8, 0x03, // 1000 = max charge voltage * 10
            0x64, 0x00, // 100 = max charge current * 10
            0x40, 0x01, // 320 = max discharge voltage * 10
            0xC8, 0x00, // 200 = max discharge current * 10
        ]);

        for frame in [&frame_351, &frame_355, &frame_356] {
            if let Some(f) = frame {
                if let Err(e) = socket.write_frame(f) {
                    error!("CAN write error on {}: {}", interface, e);
                    return;
                }
            }
        }
    }
}

fn build_standard_frame(id: u16, data: &[u8; 8]) -> Option<CanFrame> {
    let std_id = StandardId::new(id)?;
    Some(CanFrame::new(std_id, data).expect("valid CAN frame"))
}
```

**Step 4: Wire into `main.rs`**

```rust
#[cfg(target_os = "linux")]
mod can_sender;
```

In `main()`, after building state machines:

```rust
#[cfg(target_os = "linux")]
for device in &scenario.devices {
    if let Some(ref can_cfg) = device.can_lynk {
        let sm = sm_store.get(&device.unit_id).cloned();
        let iface = can_cfg.interface.clone();
        let interval = can_cfg.interval_ms;
        tokio::spawn(can_sender::start_lynk_sender(iface, interval, sm));
    }
}
```

**Step 5: Build check**

```bash
cargo check -p simulator
```

**Step 6: Commit**

```bash
git add tools/simulator/Cargo.toml tools/simulator/src/can_sender.rs \
    tools/simulator/src/scenarios.rs tools/simulator/src/main.rs
git commit -m "feat(simulator): add CAN LYNK frame sender (Linux vcan)"
```

---

## Task 6: J1939 Sender (`j1939_sender.rs`)

**Files:**
- Create: `tools/simulator/src/j1939_sender.rs`
- Modify: `tools/simulator/Cargo.toml` (add voltage_j1939 dep)
- Modify: `tools/simulator/src/main.rs` (add mod, start sender)
- Modify: `tools/simulator/src/scenarios.rs` (add J1939 config)

**Step 1: Add voltage_j1939 dependency**

In `tools/simulator/Cargo.toml`:

```toml
# J1939 encoding
voltage_j1939 = "0.1"
```

Also add to the `[target.'cfg(target_os = "linux")'.dependencies]` section:

Note: `socketcan` is already added from Task 5.

**Step 2: Add J1939 config to `scenarios.rs`**

Add to `DeviceConfig`:

```rust
    /// J1939 sender configuration (Linux only)
    #[serde(default)]
    pub j1939: Option<J1939SenderConfig>,
```

```rust
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
```

**Step 3: Create `j1939_sender.rs`**

```rust
//! J1939 frame sender for E2E testing.
//!
//! Sends extended 29-bit CAN frames encoding J1939 PGNs:
//! - EEC1 (PGN 61444): Engine Speed (SPN 190), Engine Torque (SPN 513)
//! - ET1  (PGN 65262): Coolant Temperature (SPN 110)
//!
//! Only compiled on Linux where SocketCAN is available.

#![cfg(target_os = "linux")]

use socketcan::{CanFrame, CanSocket, EmbeddedFrame, ExtendedId, Frame, Socket};
use std::sync::Arc;
use tokio::time::{interval, Duration};
use tracing::{error, info, warn};
use voltage_j1939::{build_can_id, J1939Id};

use crate::state_machine::{DeviceState, StateMachine};

/// Start a J1939 sender task.
pub async fn start_j1939_sender(
    interface: String,
    source_address: u8,
    interval_ms: u64,
    state_machine: Option<Arc<StateMachine>>,
) {
    let socket = match CanSocket::open(&interface) {
        Ok(s) => s,
        Err(e) => {
            warn!("Cannot open CAN interface {}: {} (J1939 simulation disabled)", interface, e);
            return;
        }
    };

    info!(
        "J1939 sender started on {} (SA=0x{:02X}, interval={}ms)",
        interface, source_address, interval_ms
    );

    let mut tick = interval(Duration::from_millis(interval_ms));
    let mut cycle: u64 = 0;

    loop {
        tick.tick().await;
        cycle += 1;

        let state = state_machine
            .as_ref()
            .map(|sm| sm.current_state())
            .unwrap_or(DeviceState::Standby);

        // --- EEC1 (PGN 61444) ---
        // SPN 190 (Engine Speed): bytes 3-4, resolution 0.125 RPM/bit
        // SPN 513 (Engine Percent Torque): byte 2, offset -125%
        let (rpm, torque_pct) = match state {
            DeviceState::Standby => (0.0f64, 0.0f64),
            DeviceState::Running => {
                let r = 1800.0 + ((cycle % 50) as f64) * 2.0;
                (r, 50.0 + ((cycle % 10) as f64))
            }
            DeviceState::Fault => (0.0, 0.0),
            DeviceState::Maintenance => (800.0, 10.0),
        };

        let rpm_raw = (rpm / 0.125) as u16;
        let torque_raw = ((torque_pct + 125.0) as u8).min(250);

        let eec1_data: [u8; 8] = [
            0xFF,                       // SPN 899 Engine Torque Mode (not used)
            0xFF,                       // SPN 512 Driver Demand (not used)
            torque_raw,                 // SPN 513 Actual Engine Torque
            (rpm_raw & 0xFF) as u8,     // SPN 190 low byte
            (rpm_raw >> 8) as u8,       // SPN 190 high byte
            0xFF,                       // SPN 1483 (not used)
            0xFF, 0xFF,                 // reserved
        ];

        let eec1_id = build_can_id(&J1939Id {
            priority: 3,
            pgn: 61444,
            source_address,
            destination_address: 0xFF,
        });

        // --- ET1 (PGN 65262) ---
        // SPN 110 (Engine Coolant Temperature): byte 0, offset -40°C
        let coolant_temp = match state {
            DeviceState::Standby => 25.0f64,
            DeviceState::Running => 85.0 + ((cycle % 5) as f64),
            DeviceState::Fault => 110.0,
            DeviceState::Maintenance => 30.0,
        };
        let coolant_raw = ((coolant_temp + 40.0) as u8).min(250);

        let et1_data: [u8; 8] = [
            coolant_raw,  // SPN 110
            0xFF,         // SPN 174 Fuel Temperature
            0xFF, 0xFF,   // SPN 175 Engine Oil Temperature
            0xFF, 0xFF,   // SPN 176 Turbo Oil Temperature
            0xFF,         // SPN 52 Engine Intercooler Temperature
            0xFF,         // SPN 1134 Engine Intercooler Thermostat Opening
        ];

        let et1_id = build_can_id(&J1939Id {
            priority: 6,
            pgn: 65262,
            source_address,
            destination_address: 0xFF,
        });

        // Send frames
        for (can_id, data) in [(eec1_id, &eec1_data), (et1_id, &et1_data)] {
            if let Some(ext_id) = ExtendedId::new(can_id) {
                let frame = CanFrame::new(ext_id, data).expect("valid J1939 frame");
                if let Err(e) = socket.write_frame(&frame) {
                    error!("J1939 write error on {}: {}", interface, e);
                    return;
                }
            }
        }
    }
}
```

**Step 4: Wire into `main.rs`**

```rust
#[cfg(target_os = "linux")]
mod j1939_sender;
```

In `main()`:

```rust
#[cfg(target_os = "linux")]
for device in &scenario.devices {
    if let Some(ref j_cfg) = device.j1939 {
        let sm = sm_store.get(&device.unit_id).cloned();
        let iface = j_cfg.interface.clone();
        let sa = j_cfg.source_address;
        let interval = j_cfg.interval_ms;
        tokio::spawn(j1939_sender::start_j1939_sender(iface, sa, interval, sm));
    }
}
```

**Step 5: Build check**

```bash
cargo check -p simulator
```

**Step 6: Commit**

```bash
git add tools/simulator/Cargo.toml tools/simulator/src/j1939_sender.rs \
    tools/simulator/src/scenarios.rs tools/simulator/src/main.rs
git commit -m "feat(simulator): add J1939 frame sender (EEC1/ET1 PGNs)"
```

---

## Task 7: E2E Scenario Files for CAN/J1939

**Files:**
- Create: `tools/simulator/scenarios/e2e_battery_can.yaml`
- Create: `tools/simulator/scenarios/e2e_diesel_j1939.yaml`

**Step 1: Create battery CAN scenario**

```yaml
# E2E Battery with CAN LYNK + State Machine
name: E2E Battery CAN Scenario

devices:
- type: battery
  unit_id: 1
  registers:
  - address: 0
    name: T_soc
    generator:
      type: constant
      value: 5000
  coils: []

  state_machine:
    initial_state: standby
    transitions:
      - from: standby
        to: running
        trigger:
          type: coil
          address: 200
          value: true
      - from: running
        to: standby
        trigger:
          type: coil
          address: 200
          value: false
      - from: running
        to: fault
        trigger:
          type: register
          address: 2000
          value: 999

  can_lynk:
    interface: vcan0
    interval_ms: 500

faults:
  enabled: false
```

**Step 2: Create diesel J1939 scenario**

```yaml
# E2E Diesel with J1939 + State Machine
name: E2E Diesel J1939 Scenario

devices:
- type: diesel
  unit_id: 1
  registers:
  - address: 0
    name: T_rpm
    generator:
      type: constant
      value: 0
  coils: []

  state_machine:
    initial_state: standby
    transitions:
      - from: standby
        to: running
        trigger:
          type: coil
          address: 200
          value: true
      - from: running
        to: standby
        trigger:
          type: coil
          address: 200
          value: false

  j1939:
    interface: vcan1
    source_address: 0
    interval_ms: 500

faults:
  enabled: false
```

**Step 3: Commit**

```bash
git add tools/simulator/scenarios/e2e_battery_can.yaml \
    tools/simulator/scenarios/e2e_diesel_j1939.yaml
git commit -m "feat(simulator): add E2E CAN/J1939 scenario files"
```

---

## Task 8: E2E CI Integration

**Files:**
- Create: `scripts/e2e_can_readback.py` (CAN frame verification)
- Modify: `scripts/ci-e2e-test.sh` (add CAN phases 11-13)

**Step 1: Create CAN readback script**

`scripts/e2e_can_readback.py` — Uses `/state` HTTP endpoint and optional `candump` to verify CAN simulator is running and in expected state.

```python
#!/usr/bin/env python3
"""E2E CAN/J1939 readback verification via HTTP monitoring API."""

import argparse
import json
import socket
import sys
import time
import urllib.request

GREEN = "\033[0;32m"
RED = "\033[0;31m"
NC = "\033[0m"
LINE_V = "\u2502"


def http_get(url, timeout=5):
    """Simple HTTP GET, returns parsed JSON or None."""
    try:
        req = urllib.request.Request(url)
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return json.loads(resp.read().decode())
    except Exception:
        return None


def check_health(port, retries=5):
    """Check simulator health endpoint."""
    for attempt in range(retries):
        data = http_get(f"http://127.0.0.1:{port}/health")
        if data and data.get("status") == "ok":
            return True, "Health OK"
        time.sleep(0.5)
    return False, "Health endpoint unreachable"


def check_device_state(port, unit_id, expected_state, description, retries=5):
    """Check device state via HTTP API."""
    for attempt in range(retries):
        data = http_get(f"http://127.0.0.1:{port}/state/{unit_id}")
        if data and data.get("state") == expected_state:
            return True, f"{description}: state={data['state']}"
        time.sleep(0.5)
    actual = data.get("state", "unreachable") if data else "unreachable"
    return False, f"{description}: expected={expected_state} got={actual}"


def run_phase(phase, http_port):
    """Run CAN/J1939 verification for the given phase."""
    time.sleep(1.0)
    passed = 0
    failed = 0

    if phase == 11:
        # Phase 11: Verify CAN simulators are healthy
        print(f"{LINE_V}")
        print(f"{LINE_V} CAN simulator health check...")
        ok, detail = check_health(http_port)
        status = f"{GREEN}✓{NC}" if ok else f"{RED}✗{NC}"
        print(f"{LINE_V}   {status} {detail}")
        if ok:
            passed += 1
        else:
            failed += 1

    elif phase == 12:
        # Phase 12: Verify device state after coil writes
        print(f"{LINE_V}")
        print(f"{LINE_V} Device state verification after writes...")
        cases = [
            (1, "running", "Battery unit 1 state after start command"),
        ]
        for unit_id, expected, desc in cases:
            ok, detail = check_device_state(http_port, unit_id, expected, desc)
            status = f"{GREEN}✓{NC}" if ok else f"{RED}✗{NC}"
            print(f"{LINE_V}   {status} {detail}")
            if ok:
                passed += 1
            else:
                failed += 1

    print(f"{LINE_V}")
    total = passed + failed
    if failed == 0:
        print(f"{LINE_V} {GREEN}✓ CAN verification: {passed}/{total} passed{NC}")
    else:
        print(f"{LINE_V} {RED}✗ CAN verification: {failed}/{total} failed{NC}")
    return 0 if failed == 0 else 1


def main():
    parser = argparse.ArgumentParser(description="E2E CAN readback verification")
    parser.add_argument("--phase", type=int, required=True, help="Test phase")
    parser.add_argument("--http-port", type=int, default=9100, help="Simulator HTTP port")
    args = parser.parse_args()
    sys.exit(run_phase(args.phase, args.http_port))


if __name__ == "__main__":
    main()
```

**Step 2: Add CAN phases to `ci-e2e-test.sh`**

After Phase 10, add:

```bash
# ── Phase 11: CAN Simulator Health ────────────────────────────────
if command -v ip &>/dev/null && ip link show vcan0 &>/dev/null 2>&1; then
    CAN_AVAILABLE=true
else
    CAN_AVAILABLE=false
fi

if [ "$CAN_AVAILABLE" = true ]; then
    print_phase "[Phase 11] CAN Simulator Verification"
    $PYTHON_CMD scripts/e2e_can_readback.py --phase 11 --http-port 9100
    CAN_RESULT=$?
    if [ $CAN_RESULT -eq 0 ]; then
        print_phase_end "pass"
    else
        print_phase_end "fail"
        log_warn "CAN verification failed (non-fatal on this platform)"
    fi
else
    echo -e "${LINE_V} ${YELLOW}i${NC} CAN tests skipped (vcan not available)"
fi
```

**Step 3: Commit**

```bash
git add scripts/e2e_can_readback.py scripts/ci-e2e-test.sh
git commit -m "feat(e2e): add CAN/J1939 verification phases to CI"
```

---

## Dependency Graph

```
Task 1 (state_machine.rs) ──┐
Task 2 (YAML config)     ───┤
                             ├─→ Task 3 (wire into server)
                             │
Task 4 (HTTP API)        ────┤
                             │
Task 5 (CAN LYNK)       ────┤── depends on Task 1 for StateMachine type
Task 6 (J1939 sender)   ────┤
                             │
Task 7 (scenario files)  ────┘── depends on Tasks 2,5,6 for YAML schema
Task 8 (CI integration)  ──────── depends on Task 4,7
```

**Parallelizable groups:**
- Group A: Tasks 1+2 (state machine core + config) → sequential
- Group B: Task 4 (HTTP API) → can start after Task 1
- Group C: Tasks 5+6 (CAN senders) → can start after Task 1
- Group D: Tasks 7+8 (scenarios + CI) → after everything else
