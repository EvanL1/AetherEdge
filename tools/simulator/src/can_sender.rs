//! CAN LYNK battery frame sender for vcan simulation.

use crate::state_machine::{DeviceState, StateMachineStore};
use anyhow::Result;
use socketcan::{CanFrame, CanSocket, EmbeddedFrame, Socket, StandardId};
use std::sync::Arc;
use std::time::Duration;
use tracing::info;

fn build_limits_frame() -> CanFrame {
    let mut data = [0u8; 8];
    let charge_v: i16 = 560;
    let charge_i: i16 = 200;
    let discharge_i: i16 = 200;
    let discharge_v: i16 = 480;
    data[0..2].copy_from_slice(&charge_v.to_le_bytes());
    data[2..4].copy_from_slice(&charge_i.to_le_bytes());
    data[4..6].copy_from_slice(&discharge_i.to_le_bytes());
    data[6..8].copy_from_slice(&discharge_v.to_le_bytes());
    let id = StandardId::new(0x351).unwrap();
    CanFrame::new(id, &data).unwrap()
}

fn build_status_frame(state: &DeviceState) -> CanFrame {
    let soc: u16 = match state {
        DeviceState::Running => 75,
        DeviceState::Standby | DeviceState::Maintenance => 50,
        DeviceState::Fault => 10,
    };
    let soh: u16 = 98;
    let mut data = [0u8; 4];
    data[0..2].copy_from_slice(&soc.to_le_bytes());
    data[2..4].copy_from_slice(&soh.to_le_bytes());
    let id = StandardId::new(0x355).unwrap();
    CanFrame::new(id, &data).unwrap()
}

fn build_measurements_frame(state: &DeviceState) -> CanFrame {
    let voltage: i16 = 520;
    let current: i16 = match state {
        DeviceState::Running => 150,
        _ => 0,
    };
    let temperature: i16 = 250;
    let mut data = [0u8; 6];
    data[0..2].copy_from_slice(&voltage.to_le_bytes());
    data[2..4].copy_from_slice(&current.to_le_bytes());
    data[4..6].copy_from_slice(&temperature.to_le_bytes());
    let id = StandardId::new(0x356).unwrap();
    CanFrame::new(id, &data).unwrap()
}

pub async fn run_can_sender(
    interface: &str,
    interval_ms: u64,
    sm_store: Arc<StateMachineStore>,
    unit_id: u8,
) -> Result<()> {
    info!("CAN sender starting on interface={interface} unit_id={unit_id}");
    let socket = CanSocket::open(interface)?;
    let mut ticker = tokio::time::interval(Duration::from_millis(interval_ms));
    loop {
        ticker.tick().await;
        let state = sm_store
            .get(&unit_id)
            .map(|sm| sm.current_state())
            .unwrap_or(DeviceState::Standby);
        socket.write_frame(&build_limits_frame())?;
        socket.write_frame(&build_status_frame(&state))?;
        socket.write_frame(&build_measurements_frame(&state))?;
    }
}
