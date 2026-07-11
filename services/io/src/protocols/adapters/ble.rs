//! BLE (Bluetooth Low Energy) Protocol Adapter
//!
//! Hybrid data collection from BLE GATT peripherals via btleplug.
//!
//! ## Design Overview
//!
//! BLE is a hybrid protocol where:
//! - Characteristics with Notify property push data via subscriptions
//! - Non-Notify characteristics are polled via GATT Read
//! - Write characteristics support Control/Adjustment commands
//!
//! ## Configuration Example
//!
//! ```json
//! {
//!   "device_address": "AA:BB:CC:DD:EE:FF",
//!   "adapter_name": null,
//!   "scan_timeout_ms": 10000,
//!   "connect_timeout_ms": 5000,
//!   "reconnect_interval_ms": 5000,
//!   "mtu": null
//! }
//! ```

use async_trait::async_trait;
use btleplug::api::{
    Central, Characteristic, Manager as _, Peripheral as _, ScanFilter, WriteType,
};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures::StreamExt;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::protocols::ChannelRuntime;
use crate::protocols::adapters::ble_config::BleConfig;
use crate::protocols::core::data::{DataBatch, DataPoint};
use crate::protocols::core::diagnostics::AtomicDiagnostics;
use crate::protocols::core::error::{GatewayError, Result};
use crate::protocols::core::metadata::{
    DriverMetadata, HasMetadata, ParameterMetadata, ParameterType,
};
use crate::protocols::core::point::{DataFormat, PointConfig, ProtocolAddress};
use crate::protocols::core::traits::{
    ConnectionState, DataEvent, DataEventReceiver, DataEventSender, Diagnostics, PollResult,
};

/// Expand a short BLE UUID (e.g., "180f") to full 128-bit format.
///
/// Short UUIDs are expanded using the Bluetooth Base UUID:
/// `0000XXXX-0000-1000-8000-00805f9b34fb`
pub fn expand_uuid(s: &str) -> Result<Uuid> {
    let trimmed = s.trim();

    // Try parsing as full UUID first
    if let Ok(uuid) = Uuid::parse_str(trimmed) {
        return Ok(uuid);
    }

    // Try as short (16-bit) or medium (32-bit) UUID
    let hex = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);

    match hex.len() {
        4 => {
            // 16-bit short UUID
            let full = format!("0000{hex}-0000-1000-8000-00805f9b34fb");
            Uuid::parse_str(&full).map_err(|e| {
                GatewayError::Config(format!("Invalid short BLE UUID '{trimmed}': {e}"))
            })
        },
        8 => {
            // 32-bit UUID
            let full = format!("{hex}-0000-1000-8000-00805f9b34fb");
            Uuid::parse_str(&full).map_err(|e| {
                GatewayError::Config(format!("Invalid 32-bit BLE UUID '{trimmed}': {e}"))
            })
        },
        _ => Err(GatewayError::Config(format!(
            "Invalid BLE UUID '{trimmed}': expected 4-char short, 8-char medium, or full 128-bit UUID"
        ))),
    }
}

/// Resolved BLE point: a point config with parsed UUIDs.
struct ResolvedBlePoint {
    point: PointConfig,
    service_uuid: Uuid,
    char_uuid: Uuid,
    data_format: DataFormat,
    notify: bool,
}

/// BLE Channel implementation.
///
/// Hybrid channel that connects to a BLE peripheral and:
/// - Subscribes to Notify characteristics for push-based data
/// - Polls Read characteristics on demand
/// - Writes to writable characteristics for Control/Adjustment
pub struct BleChannel {
    config: BleConfig,
    channel_id: u32,
    name: String,
    points: Vec<PointConfig>,
    peripheral: Option<Peripheral>,
    notify_handle: Option<tokio::task::JoinHandle<()>>,
    state: AtomicU8,
    event_tx: DataEventSender,
    diagnostics: Arc<AtomicDiagnostics>,
}

impl BleChannel {
    /// Create a new BLE channel.
    pub fn new(config: BleConfig, channel_id: u32, name: String, points: Vec<PointConfig>) -> Self {
        let (event_tx, _) = broadcast::channel(1024);

        Self {
            config,
            channel_id,
            name,
            points,
            peripheral: None,
            notify_handle: None,
            state: AtomicU8::new(ConnectionState::Disconnected as u8),
            event_tx,
            diagnostics: Arc::new(AtomicDiagnostics::new()),
        }
    }

    /// Set connection state and broadcast event.
    fn set_state(&self, state: ConnectionState) {
        self.state.store(state as u8, Ordering::SeqCst);
        let _ = self.event_tx.send(DataEvent::ConnectionChanged(state));
    }

    /// Resolve point configs into BLE-specific resolved points.
    fn resolve_points(&self) -> Result<Vec<ResolvedBlePoint>> {
        let mut resolved = Vec::with_capacity(self.points.len());

        for point in &self.points {
            let ble_addr = match &point.address {
                ProtocolAddress::Ble(addr) => addr,
                _ => {
                    warn!(
                        channel_id = self.channel_id,
                        point_id = point.id,
                        "Skipping non-BLE point address"
                    );
                    continue;
                },
            };

            let service_uuid = expand_uuid(&ble_addr.service_uuid)?;
            let char_uuid = expand_uuid(&ble_addr.characteristic_uuid)?;

            resolved.push(ResolvedBlePoint {
                point: point.clone(),
                service_uuid,
                char_uuid,
                data_format: ble_addr.data_format,
                notify: ble_addr.notify,
            });
        }

        Ok(resolved)
    }

    /// Find the Bluetooth adapter.
    async fn find_adapter(&self) -> Result<Adapter> {
        let manager = Manager::new()
            .await
            .map_err(|e| GatewayError::Connection(format!("Failed to create BLE manager: {e}")))?;

        let adapters = manager
            .adapters()
            .await
            .map_err(|e| GatewayError::Connection(format!("Failed to list BLE adapters: {e}")))?;

        if adapters.is_empty() {
            return Err(GatewayError::Connection(
                "No Bluetooth adapters found".to_string(),
            ));
        }

        match &self.config.adapter_name {
            Some(name) => {
                // Find adapter by name
                for adapter in &adapters {
                    let info = adapter.adapter_info().await.unwrap_or_default();
                    if info.contains(name) {
                        return Ok(adapter.clone());
                    }
                }
                Err(GatewayError::Config(format!(
                    "Bluetooth adapter '{}' not found",
                    name
                )))
            },
            None => {
                // Use first available adapter
                Ok(adapters.into_iter().next().unwrap())
            },
        }
    }

    /// Scan for and find the target peripheral.
    async fn find_peripheral(&self, adapter: &Adapter) -> Result<Peripheral> {
        // Start scanning
        adapter
            .start_scan(ScanFilter::default())
            .await
            .map_err(|e| GatewayError::Connection(format!("Failed to start BLE scan: {e}")))?;

        let target_addr = self.config.device_address.to_uppercase();

        // Poll discovered peripherals until timeout
        let deadline = tokio::time::Instant::now() + self.config.scan_timeout;

        loop {
            let peripherals = adapter.peripherals().await.map_err(|e| {
                GatewayError::Connection(format!("Failed to list peripherals: {e}"))
            })?;

            for peripheral in &peripherals {
                if let Ok(Some(props)) = peripheral.properties().await {
                    let addr = props.address.to_string().to_uppercase();
                    if addr == target_addr {
                        // Found the target device
                        let _ = adapter.stop_scan().await;
                        return Ok(peripheral.clone());
                    }
                }
            }

            if tokio::time::Instant::now() >= deadline {
                let _ = adapter.stop_scan().await;
                return Err(GatewayError::ConnectionTimeout(
                    self.config.scan_timeout.as_millis() as u64,
                ));
            }

            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    }

    /// Find a characteristic on the peripheral by service and characteristic UUIDs.
    fn find_characteristic(
        peripheral: &Peripheral,
        service_uuid: Uuid,
        char_uuid: Uuid,
    ) -> Option<Characteristic> {
        peripheral
            .characteristics()
            .into_iter()
            .find(|c| c.uuid == char_uuid && c.service_uuid == service_uuid)
    }

    /// Parse raw bytes from a BLE characteristic into a data value.
    fn parse_value(data: &[u8], format: DataFormat) -> Option<f64> {
        match format {
            DataFormat::Bool => data.first().map(|b| if *b != 0 { 1.0 } else { 0.0 }),
            DataFormat::UInt16 => {
                if data.len() >= 2 {
                    Some(u16::from_le_bytes([data[0], data[1]]) as f64)
                } else {
                    None
                }
            },
            DataFormat::Int16 => {
                if data.len() >= 2 {
                    Some(i16::from_le_bytes([data[0], data[1]]) as f64)
                } else {
                    None
                }
            },
            DataFormat::UInt32 => {
                if data.len() >= 4 {
                    Some(u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as f64)
                } else {
                    None
                }
            },
            DataFormat::Int32 => {
                if data.len() >= 4 {
                    Some(i32::from_le_bytes([data[0], data[1], data[2], data[3]]) as f64)
                } else {
                    None
                }
            },
            DataFormat::Float32 => {
                if data.len() >= 4 {
                    Some(f32::from_le_bytes([data[0], data[1], data[2], data[3]]) as f64)
                } else {
                    None
                }
            },
            DataFormat::Float64 => {
                if data.len() >= 8 {
                    Some(f64::from_le_bytes([
                        data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
                    ]))
                } else {
                    None
                }
            },
            DataFormat::UInt64 => {
                if data.len() >= 8 {
                    Some(u64::from_le_bytes([
                        data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
                    ]) as f64)
                } else {
                    None
                }
            },
            DataFormat::Int64 => {
                if data.len() >= 8 {
                    Some(i64::from_le_bytes([
                        data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
                    ]) as f64)
                } else {
                    None
                }
            },
            DataFormat::String => {
                // String format not meaningful as f64
                None
            },
        }
    }

    /// Encode an f64 value into bytes for BLE write.
    fn encode_value(value: f64, format: DataFormat) -> Vec<u8> {
        match format {
            DataFormat::Bool => vec![if value != 0.0 { 1 } else { 0 }],
            DataFormat::UInt16 => (value as u16).to_le_bytes().to_vec(),
            DataFormat::Int16 => (value as i16).to_le_bytes().to_vec(),
            DataFormat::UInt32 => (value as u32).to_le_bytes().to_vec(),
            DataFormat::Int32 => (value as i32).to_le_bytes().to_vec(),
            DataFormat::Float32 => (value as f32).to_le_bytes().to_vec(),
            DataFormat::Float64 => value.to_le_bytes().to_vec(),
            DataFormat::UInt64 => (value as u64).to_le_bytes().to_vec(),
            DataFormat::Int64 => (value as i64).to_le_bytes().to_vec(),
            DataFormat::String => format!("{value}").into_bytes(),
        }
    }

    /// Run the BLE notification event loop.
    async fn run_notify_loop(
        peripheral: Peripheral,
        resolved: Vec<ResolvedBlePoint>,
        channel_id: u32,
        event_tx: DataEventSender,
        diagnostics: Arc<AtomicDiagnostics>,
    ) {
        let Ok(mut notification_stream) = peripheral.notifications().await else {
            error!(channel_id, "Failed to get BLE notification stream");
            return;
        };

        // Build a lookup from characteristic UUID to point info
        let notify_points: std::collections::HashMap<Uuid, &ResolvedBlePoint> = resolved
            .iter()
            .filter(|rp| rp.notify)
            .map(|rp| (rp.char_uuid, rp))
            .collect();

        info!(
            channel_id,
            notify_count = notify_points.len(),
            "BLE notification loop started"
        );

        while let Some(notification) = notification_stream.next().await {
            if let Some(rp) = notify_points.get(&notification.uuid) {
                if let Some(value) = Self::parse_value(&notification.value, rp.data_format) {
                    let transformed = rp.point.transform.apply(value);
                    let dp = DataPoint::new(rp.point.id, rp.point.point_type, transformed);
                    let mut batch = DataBatch::with_capacity(1);
                    batch.add(dp);
                    diagnostics.inc_read();
                    let _ = event_tx.send(DataEvent::DataUpdate(Arc::new(batch)));

                    debug!(
                        channel_id,
                        point_id = rp.point.id,
                        value = transformed,
                        "BLE notification received"
                    );
                } else {
                    diagnostics.record_error(format!(
                        "Failed to parse BLE notify data for point {}",
                        rp.point.id
                    ));
                }
            }
        }

        warn!(channel_id, "BLE notification stream ended");
    }

    /// Write a value to a BLE characteristic for the given point ID.
    async fn write_point(&self, point_id: u32, value: f64) -> Result<()> {
        let peripheral = self.peripheral.as_ref().ok_or(GatewayError::NotConnected)?;

        // Find the point config
        let point = self
            .points
            .iter()
            .find(|p| p.id == point_id)
            .ok_or_else(|| GatewayError::PointNotFound(format!("Point {} not found", point_id)))?;

        let ble_addr = match &point.address {
            ProtocolAddress::Ble(addr) => addr,
            _ => {
                return Err(GatewayError::Config(format!(
                    "Point {} has non-BLE address",
                    point_id
                )));
            },
        };

        let service_uuid = expand_uuid(&ble_addr.service_uuid)?;
        let char_uuid = expand_uuid(&ble_addr.characteristic_uuid)?;

        let char =
            Self::find_characteristic(peripheral, service_uuid, char_uuid).ok_or_else(|| {
                GatewayError::Protocol(format!(
                    "Characteristic {}/{} not found on device",
                    ble_addr.service_uuid, ble_addr.characteristic_uuid
                ))
            })?;

        let reversed = point.transform.reverse_apply(value)?;
        let bytes = Self::encode_value(reversed, ble_addr.data_format);

        peripheral
            .write(&char, &bytes, WriteType::WithResponse)
            .await
            .map_err(|e| {
                GatewayError::Protocol(format!("BLE write failed for point {point_id}: {e}"))
            })?;

        self.diagnostics.inc_write();

        debug!(
            channel_id = self.channel_id,
            point_id, value, "BLE write successful"
        );

        Ok(())
    }
}

#[async_trait]
impl ChannelRuntime for BleChannel {
    fn id(&self) -> u32 {
        self.channel_id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn protocol(&self) -> &str {
        "ble"
    }

    fn is_event_driven(&self) -> bool {
        true // Hybrid: Notify push + on-demand Read
    }

    async fn connect(&mut self) -> Result<()> {
        if self.peripheral.is_some() {
            return Ok(());
        }

        self.set_state(ConnectionState::Connecting);

        info!(
            channel_id = self.channel_id,
            device = %self.config.device_address,
            "Connecting to BLE device"
        );

        // Find adapter
        let adapter = self.find_adapter().await?;

        // Scan and find peripheral
        let peripheral =
            match tokio::time::timeout(self.config.scan_timeout, self.find_peripheral(&adapter))
                .await
            {
                Ok(Ok(p)) => p,
                Ok(Err(e)) => {
                    self.set_state(ConnectionState::Error);
                    return Err(e);
                },
                Err(_) => {
                    self.set_state(ConnectionState::Error);
                    return Err(GatewayError::ConnectionTimeout(
                        self.config.scan_timeout.as_millis() as u64,
                    ));
                },
            };

        // Connect to peripheral
        match tokio::time::timeout(self.config.connect_timeout, peripheral.connect()).await {
            Ok(Ok(())) => {},
            Ok(Err(e)) => {
                self.set_state(ConnectionState::Error);
                return Err(GatewayError::Connection(format!("BLE connect failed: {e}")));
            },
            Err(_) => {
                self.set_state(ConnectionState::Error);
                return Err(GatewayError::ConnectionTimeout(
                    self.config.connect_timeout.as_millis() as u64,
                ));
            },
        }

        // Discover services and characteristics
        peripheral
            .discover_services()
            .await
            .map_err(|e| GatewayError::Connection(format!("BLE service discovery failed: {e}")))?;

        info!(
            channel_id = self.channel_id,
            characteristics = peripheral.characteristics().len(),
            "BLE service discovery complete"
        );

        // Resolve points and subscribe to Notify characteristics
        self.peripheral = Some(peripheral.clone());
        let resolved = self.resolve_points()?;

        for rp in &resolved {
            if rp.notify
                && let Some(char) =
                    Self::find_characteristic(&peripheral, rp.service_uuid, rp.char_uuid)
            {
                if let Err(e) = peripheral.subscribe(&char).await {
                    warn!(
                        channel_id = self.channel_id,
                        point_id = rp.point.id,
                        error = %e,
                        "Failed to subscribe to BLE characteristic notify"
                    );
                } else {
                    debug!(
                        channel_id = self.channel_id,
                        point_id = rp.point.id,
                        char_uuid = %rp.char_uuid,
                        "Subscribed to BLE notify"
                    );
                }
            }
        }

        // Spawn notification event loop
        let event_tx = self.event_tx.clone();
        let diagnostics = self.diagnostics.clone();
        let channel_id = self.channel_id;
        let peripheral_clone = peripheral;

        let handle = tokio::spawn(async move {
            Self::run_notify_loop(
                peripheral_clone,
                resolved,
                channel_id,
                event_tx,
                diagnostics,
            )
            .await;
        });

        self.notify_handle = Some(handle);
        self.set_state(ConnectionState::Connected);

        info!(
            channel_id = self.channel_id,
            device = %self.config.device_address,
            "BLE channel connected"
        );

        Ok(())
    }

    async fn disconnect(&mut self) -> Result<()> {
        // Abort notification loop
        if let Some(handle) = self.notify_handle.take() {
            handle.abort();
        }

        // Disconnect peripheral
        if let Some(peripheral) = self.peripheral.take() {
            let _ = peripheral.disconnect().await;
        }

        self.set_state(ConnectionState::Disconnected);

        info!(channel_id = self.channel_id, "BLE channel disconnected");
        Ok(())
    }

    async fn poll_once(&mut self) -> PollResult {
        let peripheral = match &self.peripheral {
            Some(p) => p,
            None => return PollResult::success(DataBatch::new()),
        };

        // Read non-Notify characteristics
        let resolved = match self.resolve_points() {
            Ok(r) => r,
            Err(e) => {
                self.diagnostics.record_error(e.to_string());
                return PollResult::success(DataBatch::new());
            },
        };

        let read_points: Vec<&ResolvedBlePoint> = resolved.iter().filter(|rp| !rp.notify).collect();

        if read_points.is_empty() {
            return PollResult::success(DataBatch::new());
        }

        let mut batch = DataBatch::with_capacity(read_points.len());
        let mut failures = Vec::new();

        for rp in &read_points {
            let char = match Self::find_characteristic(peripheral, rp.service_uuid, rp.char_uuid) {
                Some(c) => c,
                None => {
                    failures.push(crate::protocols::core::traits::PointFailure::with_error(
                        rp.point.id,
                        format!(
                            "Characteristic {}/{} not found",
                            rp.service_uuid, rp.char_uuid
                        ),
                    ));
                    continue;
                },
            };

            match peripheral.read(&char).await {
                Ok(data) => {
                    if let Some(value) = Self::parse_value(&data, rp.data_format) {
                        let transformed = rp.point.transform.apply(value);
                        batch.add(DataPoint::new(
                            rp.point.id,
                            rp.point.point_type,
                            transformed,
                        ));
                    } else {
                        failures.push(crate::protocols::core::traits::PointFailure::with_error(
                            rp.point.id,
                            format!(
                                "Failed to parse {} bytes as {:?}",
                                data.len(),
                                rp.data_format
                            ),
                        ));
                    }
                },
                Err(e) => {
                    failures.push(crate::protocols::core::traits::PointFailure::with_error(
                        rp.point.id,
                        format!("BLE read failed: {e}"),
                    ));
                },
            }
        }

        let read_count = batch.len() as u64;
        if read_count > 0 {
            self.diagnostics.add_read(read_count);
        }
        if !failures.is_empty() {
            self.diagnostics.add_error(failures.len() as u64);
        }

        if failures.is_empty() {
            PollResult::success(batch)
        } else {
            PollResult::partial(batch, failures)
        }
    }

    async fn write_control(&mut self, commands: &[(u32, f64)]) -> Result<usize> {
        let mut success_count = 0;
        for &(point_id, value) in commands {
            match self.write_point(point_id, value).await {
                Ok(()) => success_count += 1,
                Err(e) => {
                    self.diagnostics.record_error(e.to_string());
                    warn!(
                        channel_id = self.channel_id,
                        point_id,
                        error = %e,
                        "BLE control write failed"
                    );
                },
            }
        }
        Ok(success_count)
    }

    async fn write_adjustment(&mut self, adjustments: &[(u32, f64)]) -> Result<usize> {
        let mut success_count = 0;
        for &(point_id, value) in adjustments {
            match self.write_point(point_id, value).await {
                Ok(()) => success_count += 1,
                Err(e) => {
                    self.diagnostics.record_error(e.to_string());
                    warn!(
                        channel_id = self.channel_id,
                        point_id,
                        error = %e,
                        "BLE adjustment write failed"
                    );
                },
            }
        }
        Ok(success_count)
    }

    fn subscribe(&self) -> Option<DataEventReceiver> {
        Some(self.event_tx.subscribe())
    }

    async fn start_events(&mut self) -> Result<()> {
        if self.peripheral.is_none() {
            self.connect().await?;
        }
        Ok(())
    }

    async fn stop_events(&mut self) -> Result<()> {
        self.disconnect().await
    }

    async fn diagnostics(&self) -> Result<Diagnostics> {
        let snapshot = self.diagnostics.snapshot();
        Ok(Diagnostics {
            protocol: "ble".to_string(),
            connection_state: self.connection_state(),
            read_count: snapshot.read_count,
            write_count: snapshot.write_count,
            error_count: snapshot.error_count,
            last_error: snapshot.last_error,
            extra: json!({
                "device_address": self.config.device_address,
                "point_count": self.points.len(),
            }),
        })
    }

    fn connection_state(&self) -> ConnectionState {
        ConnectionState::from(self.state.load(Ordering::SeqCst))
    }
}

impl HasMetadata for BleChannel {
    fn metadata() -> DriverMetadata {
        DriverMetadata {
            name: "ble",
            display_name: "BLE GATT",
            description: "Bluetooth Low Energy GATT client via btleplug",
            is_recommended: true,
            example_config: json!({
                "device_address": "AA:BB:CC:DD:EE:FF",
                "scan_timeout_ms": 10000,
                "connect_timeout_ms": 5000,
                "reconnect_interval_ms": 5000,
            }),
            parameters: vec![
                ParameterMetadata::required(
                    "device_address",
                    "Device Address",
                    "Target BLE device MAC address (e.g., AA:BB:CC:DD:EE:FF)",
                    ParameterType::String,
                ),
                ParameterMetadata::optional(
                    "adapter_name",
                    "Adapter Name",
                    "Bluetooth adapter name (auto-detect if not specified)",
                    ParameterType::String,
                    serde_json::Value::Null,
                ),
                ParameterMetadata::optional(
                    "scan_timeout_ms",
                    "Scan Timeout (ms)",
                    "Timeout for BLE device scanning",
                    ParameterType::Integer,
                    json!(10000),
                ),
                ParameterMetadata::optional(
                    "connect_timeout_ms",
                    "Connect Timeout (ms)",
                    "Timeout for BLE connection",
                    ParameterType::Integer,
                    json!(5000),
                ),
                ParameterMetadata::optional(
                    "reconnect_interval_ms",
                    "Reconnect Interval (ms)",
                    "Delay between reconnection attempts",
                    ParameterType::Integer,
                    json!(5000),
                ),
                ParameterMetadata::optional(
                    "mtu",
                    "MTU",
                    "Maximum Transmission Unit for BLE communication",
                    ParameterType::Integer,
                    serde_json::Value::Null,
                ),
            ],
        }
    }
}

impl std::fmt::Debug for BleChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BleChannel")
            .field("channel_id", &self.channel_id)
            .field("name", &self.name)
            .field("device_address", &self.config.device_address)
            .field("state", &self.connection_state())
            .field("points", &self.points.len())
            .finish()
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_uuid_short() {
        let uuid = expand_uuid("180f").unwrap();
        assert_eq!(uuid.to_string(), "0000180f-0000-1000-8000-00805f9b34fb");
    }

    #[test]
    fn test_expand_uuid_short_uppercase() {
        let uuid = expand_uuid("180F").unwrap();
        assert_eq!(uuid.to_string(), "0000180f-0000-1000-8000-00805f9b34fb");
    }

    #[test]
    fn test_expand_uuid_short_with_prefix() {
        let uuid = expand_uuid("0x180f").unwrap();
        assert_eq!(uuid.to_string(), "0000180f-0000-1000-8000-00805f9b34fb");
    }

    #[test]
    fn test_expand_uuid_32bit() {
        let uuid = expand_uuid("0000180f").unwrap();
        assert_eq!(uuid.to_string(), "0000180f-0000-1000-8000-00805f9b34fb");
    }

    #[test]
    fn test_expand_uuid_full() {
        let uuid = expand_uuid("12345678-1234-1234-1234-123456789abc").unwrap();
        assert_eq!(uuid.to_string(), "12345678-1234-1234-1234-123456789abc");
    }

    #[test]
    fn test_expand_uuid_invalid() {
        assert!(expand_uuid("zzzz").is_err());
        assert!(expand_uuid("12345").is_err()); // 5 chars - neither 4 nor 8
    }

    #[test]
    fn test_parse_value_uint16() {
        let data = 1024u16.to_le_bytes();
        let result = BleChannel::parse_value(&data, DataFormat::UInt16);
        assert_eq!(result, Some(1024.0));
    }

    #[test]
    fn test_parse_value_int16() {
        let data = (-100i16).to_le_bytes();
        let result = BleChannel::parse_value(&data, DataFormat::Int16);
        assert_eq!(result, Some(-100.0));
    }

    #[test]
    fn test_parse_value_float32() {
        let data = 3.5f32.to_le_bytes();
        let result = BleChannel::parse_value(&data, DataFormat::Float32);
        assert!((result.unwrap() - 3.5).abs() < 0.001);
    }

    #[test]
    fn test_parse_value_bool() {
        assert_eq!(BleChannel::parse_value(&[1], DataFormat::Bool), Some(1.0));
        assert_eq!(BleChannel::parse_value(&[0], DataFormat::Bool), Some(0.0));
    }

    #[test]
    fn test_parse_value_insufficient_bytes() {
        assert_eq!(BleChannel::parse_value(&[0], DataFormat::UInt16), None);
        assert_eq!(BleChannel::parse_value(&[0, 0], DataFormat::Float32), None);
    }

    #[test]
    fn test_encode_value_uint16() {
        let bytes = BleChannel::encode_value(1024.0, DataFormat::UInt16);
        assert_eq!(bytes, 1024u16.to_le_bytes().to_vec());
    }

    #[test]
    fn test_encode_value_float32() {
        let bytes = BleChannel::encode_value(3.5, DataFormat::Float32);
        assert_eq!(bytes, (3.5f32).to_le_bytes().to_vec());
    }

    #[test]
    fn test_encode_value_bool() {
        assert_eq!(BleChannel::encode_value(1.0, DataFormat::Bool), vec![1]);
        assert_eq!(BleChannel::encode_value(0.0, DataFormat::Bool), vec![0]);
    }

    // === Channel creation & trait tests ===

    /// Helper to build a minimal BleConfig for unit tests.
    fn test_config() -> BleConfig {
        BleConfig {
            device_address: "AA:BB:CC:DD:EE:FF".to_string(),
            adapter_name: None,
            scan_timeout: std::time::Duration::from_secs(10),
            connect_timeout: std::time::Duration::from_secs(5),
            reconnect_interval: std::time::Duration::from_secs(5),
            mtu: None,
        }
    }

    #[test]
    fn test_ble_channel_creation() {
        let ch = BleChannel::new(test_config(), 42, "ble-sensor".to_string(), Vec::new());
        assert_eq!(ch.id(), 42);
        assert_eq!(ch.name(), "ble-sensor");
        assert_eq!(ch.protocol(), "ble");
        assert!(ch.is_event_driven());
        assert_eq!(ch.connection_state(), ConnectionState::Disconnected);
    }

    #[test]
    fn test_ble_metadata() {
        let meta = BleChannel::metadata();
        assert_eq!(meta.name, "ble");
        assert!(meta.is_recommended);
        assert!(!meta.parameters.is_empty());
        // Verify required parameter exists
        assert!(meta.parameters.iter().any(|p| p.name == "device_address"));
    }

    #[test]
    fn test_ble_subscribe_returns_some() {
        let ch = BleChannel::new(test_config(), 1, "test".to_string(), Vec::new());
        assert!(ch.subscribe().is_some());
    }

    // === Additional data format parse tests ===

    #[test]
    fn test_parse_value_uint32() {
        let data = 70000u32.to_le_bytes();
        let result = BleChannel::parse_value(&data, DataFormat::UInt32);
        assert_eq!(result, Some(70000.0));
    }

    #[test]
    fn test_parse_value_int32() {
        let data = (-50000i32).to_le_bytes();
        let result = BleChannel::parse_value(&data, DataFormat::Int32);
        assert_eq!(result, Some(-50000.0));
    }

    #[test]
    fn test_parse_value_float64() {
        let data = 2.5f64.to_le_bytes();
        let result = BleChannel::parse_value(&data, DataFormat::Float64);
        assert!((result.unwrap() - 2.5).abs() < 1e-12);
    }

    #[test]
    fn test_parse_value_uint64() {
        let val = 1_000_000_000u64;
        let data = val.to_le_bytes();
        let result = BleChannel::parse_value(&data, DataFormat::UInt64);
        assert_eq!(result, Some(val as f64));
    }

    #[test]
    fn test_parse_value_int64() {
        let val = -1_000_000_000i64;
        let data = val.to_le_bytes();
        let result = BleChannel::parse_value(&data, DataFormat::Int64);
        assert_eq!(result, Some(val as f64));
    }

    #[test]
    fn test_parse_value_string_returns_none() {
        // String format is not meaningful as f64
        let data = b"hello";
        let result = BleChannel::parse_value(data, DataFormat::String);
        assert_eq!(result, None);
    }

    // === Additional encode tests ===

    #[test]
    fn test_encode_value_int16() {
        let bytes = BleChannel::encode_value(-300.0, DataFormat::Int16);
        assert_eq!(bytes, (-300i16).to_le_bytes().to_vec());
    }

    #[test]
    fn test_encode_value_uint32() {
        let bytes = BleChannel::encode_value(70000.0, DataFormat::UInt32);
        assert_eq!(bytes, 70000u32.to_le_bytes().to_vec());
    }

    #[test]
    fn test_encode_value_int32() {
        let bytes = BleChannel::encode_value(-50000.0, DataFormat::Int32);
        assert_eq!(bytes, (-50000i32).to_le_bytes().to_vec());
    }

    #[test]
    fn test_encode_value_float64() {
        let bytes = BleChannel::encode_value(2.5, DataFormat::Float64);
        assert_eq!(bytes, 2.5f64.to_le_bytes().to_vec());
    }

    #[test]
    fn test_encode_value_uint64() {
        let bytes = BleChannel::encode_value(1_000_000.0, DataFormat::UInt64);
        assert_eq!(bytes, 1_000_000u64.to_le_bytes().to_vec());
    }

    #[test]
    fn test_encode_value_int64() {
        let bytes = BleChannel::encode_value(-1_000_000.0, DataFormat::Int64);
        assert_eq!(bytes, (-1_000_000i64).to_le_bytes().to_vec());
    }

    #[test]
    fn test_encode_value_string() {
        let bytes = BleChannel::encode_value(42.5, DataFormat::String);
        assert_eq!(bytes, b"42.5");
    }

    // === Roundtrip tests ===

    #[test]
    fn test_parse_encode_roundtrip() {
        // Test encode -> parse roundtrip for all numeric DataFormat variants
        let test_cases: Vec<(DataFormat, f64)> = vec![
            (DataFormat::Bool, 1.0),
            (DataFormat::Bool, 0.0),
            (DataFormat::UInt16, 1024.0),
            (DataFormat::Int16, -100.0),
            (DataFormat::UInt32, 70000.0),
            (DataFormat::Int32, -50000.0),
            (DataFormat::Float32, 3.5),
            (DataFormat::Float64, 2.5),
            (DataFormat::UInt64, 1_000_000.0),
            (DataFormat::Int64, -1_000_000.0),
        ];

        for (format, value) in test_cases {
            let encoded = BleChannel::encode_value(value, format);
            let decoded = BleChannel::parse_value(&encoded, format);
            match format {
                DataFormat::Float32 => {
                    // Float32 has precision loss through f64->f32->f64
                    let d = decoded.expect("Float32 roundtrip should decode");
                    assert!(
                        (d - value).abs() < 0.01,
                        "Float32 roundtrip failed: encoded {value}, decoded {d}"
                    );
                },
                _ => {
                    assert_eq!(
                        decoded,
                        Some(value),
                        "Roundtrip failed for {format:?} with value {value}"
                    );
                },
            }
        }
    }

    // === Boundary / edge cases ===

    #[test]
    fn test_parse_value_empty_bytes() {
        assert_eq!(BleChannel::parse_value(&[], DataFormat::Bool), None);
        assert_eq!(BleChannel::parse_value(&[], DataFormat::UInt16), None);
        assert_eq!(BleChannel::parse_value(&[], DataFormat::Int16), None);
        assert_eq!(BleChannel::parse_value(&[], DataFormat::UInt32), None);
        assert_eq!(BleChannel::parse_value(&[], DataFormat::Int32), None);
        assert_eq!(BleChannel::parse_value(&[], DataFormat::Float32), None);
        assert_eq!(BleChannel::parse_value(&[], DataFormat::Float64), None);
        assert_eq!(BleChannel::parse_value(&[], DataFormat::UInt64), None);
        assert_eq!(BleChannel::parse_value(&[], DataFormat::Int64), None);
        assert_eq!(BleChannel::parse_value(&[], DataFormat::String), None);
    }

    #[test]
    fn test_expand_uuid_empty_string() {
        assert!(expand_uuid("").is_err());
    }

    #[test]
    fn test_parse_value_insufficient_bytes_extended() {
        // 3 bytes is insufficient for UInt32/Int32/Float32 (need 4)
        assert_eq!(
            BleChannel::parse_value(&[0, 0, 0], DataFormat::UInt32),
            None
        );
        assert_eq!(BleChannel::parse_value(&[0, 0, 0], DataFormat::Int32), None);
        assert_eq!(
            BleChannel::parse_value(&[0, 0, 0], DataFormat::Float32),
            None
        );
        // 7 bytes is insufficient for 64-bit types (need 8)
        assert_eq!(
            BleChannel::parse_value(&[0, 0, 0, 0, 0, 0, 0], DataFormat::Float64),
            None
        );
        assert_eq!(
            BleChannel::parse_value(&[0, 0, 0, 0, 0, 0, 0], DataFormat::UInt64),
            None
        );
        assert_eq!(
            BleChannel::parse_value(&[0, 0, 0, 0, 0, 0, 0], DataFormat::Int64),
            None
        );
    }
}
