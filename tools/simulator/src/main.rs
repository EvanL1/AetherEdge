//! Modbus TCP Slave Simulator for AetherEMS CI Testing.
//!
//! This tool simulates industrial devices (PCS, BMS, PV) as Modbus TCP slaves,
//! generating realistic waveform data for testing io.
//!
//! # Usage
//!
//! ```bash
//! # Start with a scenario file
//! simulator --scenario scenarios/pcs_normal.yaml --port 5020
//!
//! # Start with fault injection enabled
//! simulator --scenario scenarios/network_fault.yaml --port 5020
//! ```

#[cfg(all(target_os = "linux", feature = "can"))]
mod can_sender;
mod coils;
mod devices;
mod http_api;
#[cfg(all(target_os = "linux", feature = "j1939"))]
mod j1939_sender;
mod rtu_server;
mod scenarios;
mod server;
mod state_machine;
mod writable;

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{Level, info, warn};
use tracing_subscriber::FmtSubscriber;

use scenarios::TriggerConfig;
use state_machine::{DeviceState, StateMachine, StateMachineStore, Transition, Trigger};

/// Modbus TCP/RTU Slave Simulator
#[derive(Parser, Debug)]
#[command(name = "simulator")]
#[command(about = "Modbus TCP/RTU slave simulator for AetherEMS CI testing")]
struct Args {
    /// Scenario configuration file path
    #[arg(short, long)]
    scenario: PathBuf,

    /// TCP port to listen on (TCP mode only)
    #[arg(short, long, default_value = "5020")]
    port: u16,

    /// Bind address (TCP mode only)
    #[arg(short, long, default_value = "0.0.0.0")]
    bind: String,

    /// RTU serial port (e.g., /dev/ttyUSB0 or /dev/pts/3)
    /// If specified, runs in RTU mode instead of TCP mode
    #[arg(long)]
    rtu: Option<String>,

    /// RTU baud rate (only used with --rtu)
    #[arg(long, default_value = "9600")]
    baud: u32,

    /// HTTP API port for state observability (0 to disable)
    #[arg(long, default_value = "0")]
    http_port: u16,

    /// Log level (trace, debug, info, warn, error)
    #[arg(short, long, default_value = "info")]
    log_level: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize logging
    let level = match args.log_level.to_lowercase().as_str() {
        "trace" => Level::TRACE,
        "debug" => Level::DEBUG,
        "info" => Level::INFO,
        "warn" => Level::WARN,
        "error" => Level::ERROR,
        _ => Level::INFO,
    };

    FmtSubscriber::builder()
        .with_max_level(level)
        .with_target(false)
        .with_thread_ids(false)
        .init();

    info!("AetherEMS Modbus Simulator v{}", env!("CARGO_PKG_VERSION"));
    info!("Loading scenario: {:?}", args.scenario);

    // Load scenario configuration
    let scenario = scenarios::load_scenario(&args.scenario)?;
    info!(
        "Scenario '{}' loaded: {} device(s)",
        scenario.name,
        scenario.devices.len()
    );

    // Build device register map
    let device_map = devices::build_device_map(&scenario.devices)?;

    // Build state machines from scenario config
    let mut sm_store = StateMachineStore::new();
    for device in &scenario.devices {
        if let Some(ref sm_config) = device.state_machine {
            let initial = sm_config
                .initial_state
                .parse::<DeviceState>()
                .unwrap_or_default();
            let transitions: Vec<Transition> = sm_config
                .transitions
                .iter()
                .filter_map(|t| {
                    let from = t.from.parse::<DeviceState>().ok()?;
                    let to = t.to.parse::<DeviceState>().ok()?;
                    let trigger = match &t.trigger {
                        TriggerConfig::Coil { address, value } => Trigger::Coil {
                            address: *address,
                            value: *value,
                        },
                        TriggerConfig::Register { address, value } => Trigger::Register {
                            address: *address,
                            value: *value,
                        },
                    };
                    Some(Transition { from, trigger, to })
                })
                .collect();
            info!(
                "State machine for unit {}: initial={}, {} transition(s)",
                device.unit_id,
                sm_config.initial_state,
                transitions.len()
            );
            sm_store.insert(
                device.unit_id,
                Arc::new(StateMachine::new(initial, transitions)),
            );
        }
    }

    let sm_store = Arc::new(sm_store);

    // Start HTTP API for state observability (if enabled)
    if args.http_port > 0 {
        let http_addr = format!("{}:{}", args.bind, args.http_port);
        let sm_clone = Arc::clone(&sm_store);
        tokio::spawn(async move {
            if let Err(e) = http_api::run_http_server(&http_addr, sm_clone).await {
                tracing::error!("HTTP API error: {}", e);
            }
        });
    }

    // Start CAN LYNK senders (Linux + can feature)
    #[cfg(all(target_os = "linux", feature = "can"))]
    for device in &scenario.devices {
        if let Some(ref can_cfg) = device.can_lynk {
            let sm_clone = Arc::clone(&sm_store);
            let iface = can_cfg.interface.clone();
            let interval = can_cfg.interval_ms;
            let uid = device.unit_id;
            tokio::spawn(async move {
                if let Err(e) = can_sender::run_can_sender(&iface, interval, sm_clone, uid).await {
                    tracing::error!("CAN sender error (unit {}): {}", uid, e);
                }
            });
        }
    }

    // Start J1939 senders (Linux + j1939 feature)
    #[cfg(all(target_os = "linux", feature = "j1939"))]
    for device in &scenario.devices {
        if let Some(ref j1939_cfg) = device.j1939 {
            let sm_clone = Arc::clone(&sm_store);
            let iface = j1939_cfg.interface.clone();
            let interval = j1939_cfg.interval_ms;
            let sa = j1939_cfg.source_address;
            let uid = device.unit_id;
            tokio::spawn(async move {
                if let Err(e) =
                    j1939_sender::run_j1939_sender(&iface, interval, sa, sm_clone, uid).await
                {
                    tracing::error!("J1939 sender error (unit {}): {}", uid, e);
                }
            });
        }
    }

    // Start Modbus server based on mode
    if let Some(rtu_port) = args.rtu {
        info!(
            "Starting Modbus RTU server on {} @ {} baud",
            rtu_port, args.baud
        );
        if scenario.devices.iter().any(|d| d.state_machine.is_some()) {
            warn!(
                "RTU mode does not support state machine triggers — state machines will be ignored"
            );
        }
        rtu_server::run_rtu_server(&rtu_port, args.baud, device_map, &scenario.devices).await?;
    } else {
        let addr = format!("{}:{}", args.bind, args.port);
        info!("Starting Modbus TCP server on {}", addr);
        server::run_server(
            &addr,
            device_map,
            scenario.faults,
            &scenario.devices,
            Arc::clone(&sm_store),
        )
        .await?;
    }

    Ok(())
}
