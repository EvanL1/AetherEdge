//! IEC 60870-5-104 protocol adapter.
//!
//! This module provides the `Iec104Channel` adapter that integrates
//! `voltage_iec104` with the protocol layer's `Protocol` and `EventDrivenProtocol` traits.
//!
//! IEC 104 is an event-driven protocol - data is received via spontaneous
//! transmissions from the controlled station (RTU/substation).
//!
//! # Example
//!
//! ```rust,ignore
//! use crate::protocols::prelude::*;
//! use crate::protocols::adapters::iec104::{Iec104Channel, Iec104ChannelConfig};
//!
//! let config = Iec104ChannelConfig::new("192.168.1.100:2404")
//!     .with_common_address(1);
//!
//! let mut channel = Iec104Channel::new(config, store);
//! channel.connect().await?;
//! channel.start_data_transfer().await?;
//!
//! // Receive events via subscription
//! let mut rx = channel.subscribe();
//! while let Some(event) = rx.recv().await {
//!     match event {
//!         DataEvent::DataUpdate(batch) => { /* process data */ }
//!         _ => {}
//!     }
//! }
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use tokio::sync::broadcast;
use voltage_iec104::{ClientConfig, Cp56Time2a, Iec104Client, Iec104Event};

use async_trait::async_trait;

// ============================================================================
// Default timeout constants for IEC 104
// ============================================================================
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_T1_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_T2_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_T3_TIMEOUT: Duration = Duration::from_secs(20);

use crate::protocols::core::data::{DataBatch, DataPoint, Value};
use crate::protocols::core::diagnostics::AtomicDiagnostics;
use crate::protocols::core::error::{GatewayError, Result};
use crate::protocols::core::point::PointConfig;
use crate::protocols::core::quality::Quality;
use crate::protocols::core::traits::{
    AdjustmentCommand, CommunicationMode, ConnectionState, ControlCommand, DataEvent,
    DataEventHandler, DataEventReceiver, DataEventSender, Diagnostics, EventDrivenProtocol,
    PollResult, Protocol, ProtocolCapabilities, ProtocolClient, WriteResult,
};
use crate::protocols::gateway::ChannelRuntime;
use aether_model::PointType;

/// IEC 104 channel configuration.
#[derive(Debug, Clone)]
pub struct Iec104ChannelConfig {
    /// Target address (e.g., "192.168.1.100:2404")
    pub address: String,

    /// Common address of ASDU (station address)
    pub common_address: u16,

    /// Connection timeout
    pub connect_timeout: Duration,

    /// T1 timeout (send/receive APDU)
    pub t1_timeout: Duration,

    /// T2 timeout (no data acknowledgement)
    pub t2_timeout: Duration,

    /// T3 timeout (test frame)
    pub t3_timeout: Duration,

    /// Max unconfirmed I-frames (K parameter)
    pub k: u16,

    /// Latest ack threshold (W parameter)
    pub w: u16,

    /// Point configurations (IOA to point mapping)
    pub points: Vec<PointConfig>,

    /// IOA to (point_id, point_type) mapping (built from points)
    ioa_mapping: HashMap<u32, (u32, PointType)>,
}

impl Iec104ChannelConfig {
    /// Create a new configuration.
    pub fn new(address: impl Into<String>) -> Self {
        Self {
            address: address.into(),
            common_address: 1,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            t1_timeout: DEFAULT_T1_TIMEOUT,
            t2_timeout: DEFAULT_T2_TIMEOUT,
            t3_timeout: DEFAULT_T3_TIMEOUT,
            k: 12,
            w: 8,
            points: Vec::new(),
            ioa_mapping: HashMap::new(),
        }
    }

    /// Set common address.
    pub fn with_common_address(mut self, addr: u16) -> Self {
        self.common_address = addr;
        self
    }

    /// Set connection timeout.
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Set T1 timeout.
    pub fn with_t1_timeout(mut self, timeout: Duration) -> Self {
        self.t1_timeout = timeout;
        self
    }

    /// Set T2 timeout.
    pub fn with_t2_timeout(mut self, timeout: Duration) -> Self {
        self.t2_timeout = timeout;
        self
    }

    /// Set T3 timeout.
    pub fn with_t3_timeout(mut self, timeout: Duration) -> Self {
        self.t3_timeout = timeout;
        self
    }

    /// Add point configurations.
    pub fn with_points(mut self, points: Vec<PointConfig>) -> Self {
        // Build IOA mapping from point configs (includes point_type for DataPoint creation)
        for point in &points {
            if let crate::protocols::core::point::ProtocolAddress::Iec104(addr) = &point.address {
                self.ioa_mapping
                    .insert(addr.ioa, (point.id, point.point_type));
            }
        }
        self.points = points;
        self
    }

    /// Build voltage_iec104 ClientConfig.
    fn to_client_config(&self) -> ClientConfig {
        let mut config = ClientConfig::new(&self.address)
            .connect_timeout(self.connect_timeout)
            .t1_timeout(self.t1_timeout)
            .t2_timeout(self.t2_timeout)
            .t3_timeout(self.t3_timeout);
        config.k = self.k;
        config.w = self.w;
        config
    }
}

/// IEC 104 channel parameters for JSON configuration.
///
/// This is a serde-friendly version of the configuration that can be
/// deserialized from JSON and converted to `Iec104ChannelConfig`.
///
/// # Example JSON
///
/// ```json
/// {
///     "address": "192.168.1.100:2404",
///     "common_address": 1,
///     "connect_timeout_ms": 10000
/// }
/// ```
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Iec104ParamsConfig {
    /// Target address (e.g., "192.168.1.100:2404")
    pub address: String,

    /// Common address of ASDU (station address)
    #[serde(default = "default_common_address")]
    pub common_address: u16,

    /// Connection timeout in milliseconds
    #[serde(default = "default_connect_timeout_ms")]
    pub connect_timeout_ms: u64,

    /// T1 timeout in seconds
    #[serde(default = "default_t1_timeout")]
    pub t1_timeout_s: u64,

    /// T2 timeout in seconds
    #[serde(default = "default_t2_timeout")]
    pub t2_timeout_s: u64,

    /// T3 timeout in seconds
    #[serde(default = "default_t3_timeout")]
    pub t3_timeout_s: u64,
}

fn default_common_address() -> u16 {
    1
}

fn default_connect_timeout_ms() -> u64 {
    DEFAULT_CONNECT_TIMEOUT.as_millis() as u64
}

fn default_t1_timeout() -> u64 {
    DEFAULT_T1_TIMEOUT.as_secs()
}

fn default_t2_timeout() -> u64 {
    DEFAULT_T2_TIMEOUT.as_secs()
}

fn default_t3_timeout() -> u64 {
    DEFAULT_T3_TIMEOUT.as_secs()
}

impl Iec104ParamsConfig {
    /// Convert to Iec104ChannelConfig.
    ///
    /// Note: Points must be set separately via `with_points()`.
    pub fn to_config(&self) -> Iec104ChannelConfig {
        Iec104ChannelConfig::new(&self.address)
            .with_common_address(self.common_address)
            .with_connect_timeout(Duration::from_millis(self.connect_timeout_ms))
            .with_t1_timeout(Duration::from_secs(self.t1_timeout_s))
            .with_t2_timeout(Duration::from_secs(self.t2_timeout_s))
            .with_t3_timeout(Duration::from_secs(self.t3_timeout_s))
    }
}

/// IEC 104 channel adapter.
///
/// This struct wraps a `voltage_iec104::Iec104Client` and implements
/// the protocol layer's `Protocol`, `ProtocolClient`, and `EventDrivenProtocol` traits.
///
/// Note: This adapter follows the "protocol layer separated from storage" design.
/// The channel returns DataBatch via events; the service layer handles persistence.
pub struct Iec104Channel {
    /// Channel unique identifier.
    channel_id: u32,
    /// Channel instance name.
    name: String,
    config: Iec104ChannelConfig,
    client: Iec104Client,
    state: Arc<std::sync::RwLock<ConnectionState>>,
    diagnostics: Arc<AtomicDiagnostics>,
    /// Last interrogation timestamp (Unix millis, 0 = never)
    last_interrogation_ms: AtomicU64,
    /// Broadcast sender for event-driven subscribers (multiple subscribers supported).
    event_tx: DataEventSender,
    event_handler: Option<Arc<dyn DataEventHandler>>,
    poll_task: Option<tokio::task::JoinHandle<()>>,
    /// Point ID -> index lookup for O(1) access
    point_index: HashMap<u32, usize>,
}

impl Iec104Channel {
    /// Create a new IEC 104 channel.
    pub fn new(config: Iec104ChannelConfig, channel_id: u32, name: String) -> Self {
        let client_config = config.to_client_config();
        let client = Iec104Client::new(client_config);
        // Use broadcast channel for multiple subscribers
        let (event_tx, _) = broadcast::channel(1024);

        // Build point ID -> index mapping for O(1) lookup
        let point_index: HashMap<u32, usize> = config
            .points
            .iter()
            .enumerate()
            .map(|(i, p)| (p.id, i))
            .collect();

        Self {
            channel_id,
            name,
            config,
            client,
            state: Arc::new(std::sync::RwLock::new(ConnectionState::Disconnected)),
            diagnostics: Arc::new(AtomicDiagnostics::new()),
            last_interrogation_ms: AtomicU64::new(0),
            event_tx,
            event_handler: None,
            poll_task: None,
            point_index,
        }
    }

    /// Set connection state.
    fn set_state(&self, state: ConnectionState) {
        if let Ok(mut s) = self.state.write() {
            *s = state;
        }
    }

    /// Get connection state.
    fn get_state(&self) -> ConnectionState {
        self.state
            .read()
            .map(|s| *s)
            .unwrap_or(ConnectionState::Error)
    }

    /// Start data transfer (STARTDT).
    pub async fn start_data_transfer(&mut self) -> Result<()> {
        self.client
            .start_dt()
            .await
            .map_err(|e| GatewayError::Protocol(e.to_string()))
    }

    /// Stop data transfer (STOPDT).
    pub async fn stop_data_transfer(&mut self) -> Result<()> {
        self.client
            .stop_dt()
            .await
            .map_err(|e| GatewayError::Protocol(e.to_string()))
    }

    /// Send general interrogation command.
    pub async fn general_interrogation(&mut self) -> Result<()> {
        self.client
            .general_interrogation(self.config.common_address)
            .await
            .map_err(|e| GatewayError::Protocol(e.to_string()))?;

        // Record interrogation timestamp (lock-free)
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        self.last_interrogation_ms.store(now_ms, Ordering::Relaxed);
        Ok(())
    }

    /// Send counter interrogation command.
    ///
    /// Group 5 = request group 1 counter interrogation (general)
    pub async fn counter_interrogation(&mut self, group: u8) -> Result<()> {
        self.client
            .counter_interrogation(self.config.common_address, group)
            .await
            .map_err(|e| GatewayError::Protocol(e.to_string()))
    }

    /// Send clock synchronization command.
    pub async fn clock_sync(&mut self) -> Result<()> {
        let time = cp56time2a_now();
        self.client
            .clock_sync(self.config.common_address, time)
            .await
            .map_err(|e| GatewayError::Protocol(e.to_string()))
    }

    /// Poll for events (must be called periodically).
    pub async fn poll(&mut self) -> Result<()> {
        match self.client.poll().await {
            Ok(Some(event)) => {
                self.handle_iec104_event(event).await;
                Ok(())
            },
            Ok(None) => Ok(()),
            Err(e) => {
                let err_msg = e.to_string();
                self.record_error(err_msg.clone());
                Err(GatewayError::Protocol(err_msg))
            },
        }
    }

    /// Handle IEC 104 event.
    async fn handle_iec104_event(&self, event: Iec104Event) {
        match event {
            Iec104Event::Connected => {
                self.set_state(ConnectionState::Connected);
                let _ = self
                    .event_tx
                    .send(DataEvent::ConnectionChanged(ConnectionState::Connected));
            },
            Iec104Event::Disconnected => {
                self.set_state(ConnectionState::Disconnected);
                let _ = self
                    .event_tx
                    .send(DataEvent::ConnectionChanged(ConnectionState::Disconnected));
            },
            Iec104Event::DataTransferStarted => {
                // Data transfer is active
            },
            Iec104Event::DataTransferStopped => {
                // Data transfer stopped
            },
            Iec104Event::DataUpdate(points) => {
                let batch = self.convert_data_points(points).await;
                if !batch.is_empty() {
                    // Send event (service layer handles storage)
                    // Arc wrap for O(1) broadcast clone
                    let _ = self.event_tx.send(DataEvent::DataUpdate(Arc::new(batch)));

                    // Update diagnostics (lock-free)
                    self.diagnostics.inc_read();
                }
            },
            Iec104Event::AsduReceived(_asdu) => {
                // Raw ASDU - usually for command responses
            },
            Iec104Event::CommandConfirm { ioa, success } => {
                // Command confirmation (lock-free)
                if success {
                    self.diagnostics.inc_write();
                } else {
                    self.diagnostics
                        .record_error(format!("Command failed for IOA {}", ioa));
                }
            },
            Iec104Event::InterrogationComplete { common_address: _ } => {
                // Interrogation finished
            },
            Iec104Event::Error(msg) => {
                self.record_error(msg.clone());
                let _ = self.event_tx.send(DataEvent::Error(msg));
            },
        }
    }

    /// Convert IEC 104 data points to DataBatch.
    async fn convert_data_points(&self, points: Vec<voltage_iec104::DataPoint>) -> DataBatch {
        let mut batch = DataBatch::new();

        for point in points {
            // Look up (point_id, point_type) from IOA, or use IOA with Telemetry as fallback
            let (point_id, point_type) = self
                .config
                .ioa_mapping
                .get(&point.ioa)
                .copied()
                .unwrap_or((point.ioa, PointType::Telemetry));

            // Convert value
            let value = convert_iec104_value(&point.value);

            // Convert quality
            let quality = convert_iec104_quality(&point.quality);

            // Convert source timestamp
            let source_timestamp = point.timestamp.as_ref().and_then(cp56time2a_to_datetime);

            let dp = DataPoint {
                id: point_id,
                point_type,
                value,
                quality,
                timestamp: Utc::now(),
                source_timestamp,
            };

            batch.add(dp);
        }

        batch
    }

    /// Record an error (lock-free).
    fn record_error(&self, error: String) {
        self.diagnostics.record_error(error);
    }

    /// Find point config by ID (O(1) lookup).
    fn find_point(&self, id: u32) -> Option<&PointConfig> {
        self.point_index
            .get(&id)
            .map(|&idx| &self.config.points[idx])
    }

    /// Resolve point ID to IEC 104 address. Returns error tuple for failures vec.
    fn resolve_iec104_addr(
        &self,
        id: u32,
    ) -> std::result::Result<
        (&PointConfig, &crate::protocols::core::point::Iec104Address),
        (u32, String),
    > {
        let point = self
            .find_point(id)
            .ok_or_else(|| (id, "Point not found".to_string()))?;
        match &point.address {
            crate::protocols::core::point::ProtocolAddress::Iec104(addr) => Ok((point, addr)),
            _ => Err((id, "Invalid address type".to_string())),
        }
    }
}

impl ProtocolCapabilities for Iec104Channel {
    fn name(&self) -> &'static str {
        "IEC 60870-5-104"
    }

    fn supported_modes(&self) -> &[CommunicationMode] {
        &[CommunicationMode::EventDriven, CommunicationMode::Polling]
    }

    fn version(&self) -> &'static str {
        "1.0"
    }
}

impl Protocol for Iec104Channel {
    fn connection_state(&self) -> ConnectionState {
        self.get_state()
    }

    #[allow(clippy::disallowed_methods)] // json! macro
    async fn diagnostics(&self) -> Result<Diagnostics> {
        let state = self.get_state();

        // Calculate seconds since last interrogation (lock-free)
        let last_interrogation_secs = {
            let ms = self.last_interrogation_ms.load(Ordering::Relaxed);
            if ms == 0 {
                None
            } else {
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                Some((now_ms.saturating_sub(ms)) / 1000)
            }
        };

        Ok(Diagnostics {
            protocol: ProtocolCapabilities::name(self).to_string(),
            connection_state: state,
            read_count: self.diagnostics.read_count(),
            write_count: self.diagnostics.write_count(),
            error_count: self.diagnostics.error_count(),
            last_error: self.diagnostics.last_error(),
            extra: serde_json::json!({
                "address": self.config.address,
                "common_address": self.config.common_address,
                "points": self.config.points.len(),
                "last_interrogation": last_interrogation_secs,
            }),
        })
    }
}

impl ProtocolClient for Iec104Channel {
    async fn connect(&mut self) -> Result<()> {
        self.set_state(ConnectionState::Connecting);

        match self.client.connect().await {
            Ok(()) => {
                self.set_state(ConnectionState::Connected);
                Ok(())
            },
            Err(e) => {
                self.set_state(ConnectionState::Error);
                let err_msg = e.to_string();
                self.record_error(err_msg.clone());
                Err(GatewayError::Connection(err_msg))
            },
        }
    }

    async fn disconnect(&mut self) -> Result<()> {
        // Stop poll task if running
        if let Some(task) = self.poll_task.take() {
            task.abort();
        }

        // Stop data transfer first
        let _ = self.stop_data_transfer().await;

        // Disconnect
        match self.client.disconnect().await {
            Ok(()) => {
                self.set_state(ConnectionState::Disconnected);
                Ok(())
            },
            Err(e) => Err(GatewayError::Connection(e.to_string())),
        }
    }

    async fn poll_once(&mut self) -> PollResult {
        // IEC 104 is event-driven, so poll_once fetches any pending events
        // from the underlying client and converts them to a DataBatch.
        let event = match self.client.poll().await {
            Ok(ev) => ev,
            Err(_e) => {
                // Lock-free error recording
                self.diagnostics.inc_error();
                // Return empty result with no failure tracking (connection-level error)
                return PollResult::success(DataBatch::new());
            },
        };

        let batch = if let Some(voltage_iec104::Iec104Event::DataUpdate(points)) = event {
            self.convert_data_points(points).await
        } else {
            DataBatch::new()
        };

        if !batch.is_empty() {
            // Lock-free read count increment
            self.diagnostics.inc_read();
        }

        PollResult::success(batch)
    }

    async fn write_control(&mut self, commands: &[ControlCommand]) -> Result<WriteResult> {
        let mut success_count = 0;
        let mut failures = Vec::new();

        for cmd in commands {
            let (_point, iec_addr) = match self.resolve_iec104_addr(cmd.id) {
                Ok(v) => v,
                Err(e) => {
                    failures.push(e);
                    continue;
                },
            };
            let ioa = iec_addr.ioa;
            match self
                .client
                .single_command(self.config.common_address, ioa, cmd.value, false)
                .await
            {
                Ok(()) => success_count += 1,
                Err(e) => failures.push((cmd.id, e.to_string())),
            }
        }

        self.diagnostics.add_write(success_count as u64);
        Ok(WriteResult {
            success_count,
            failures,
        })
    }

    async fn write_adjustment(&mut self, adjustments: &[AdjustmentCommand]) -> Result<WriteResult> {
        let mut success_count = 0;
        let mut failures = Vec::new();

        for adj in adjustments {
            let (point, iec_addr) = match self.resolve_iec104_addr(adj.id) {
                Ok(v) => v,
                Err(e) => {
                    failures.push(e);
                    continue;
                },
            };
            let raw_value = match point.transform.reverse_apply(adj.value) {
                Ok(v) => v as f32,
                Err(e) => {
                    failures.push((adj.id, e.to_string()));
                    continue;
                },
            };
            let ioa = iec_addr.ioa;
            match self
                .client
                .setpoint_float(self.config.common_address, ioa, raw_value, false)
                .await
            {
                Ok(()) => success_count += 1,
                Err(e) => failures.push((adj.id, e.to_string())),
            }
        }

        self.diagnostics.add_write(success_count as u64);
        Ok(WriteResult {
            success_count,
            failures,
        })
    }
}

impl EventDrivenProtocol for Iec104Channel {
    fn subscribe(&self) -> DataEventReceiver {
        // Broadcast channel supports multiple subscribers
        // Each call to subscribe() returns a new receiver that gets all future events
        self.event_tx.subscribe()
    }

    fn set_event_handler(&mut self, handler: Arc<dyn DataEventHandler>) {
        self.event_handler = Some(handler);
    }

    async fn start(&mut self) -> Result<()> {
        // For IEC 104, start means starting data transfer and initial GI
        self.start_data_transfer().await?;
        self.general_interrogation().await?;
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        // Abort poll task if running
        if let Some(task) = self.poll_task.take() {
            task.abort();
        }
        self.stop_data_transfer().await
    }
}

/// Convert IEC 104 DataValue to Value.
fn convert_iec104_value(value: &voltage_iec104::DataValue) -> Value {
    match value {
        voltage_iec104::DataValue::Single(v) => Value::Bool(*v),
        voltage_iec104::DataValue::Double(dp) => {
            use voltage_iec104::DoublePointValue;
            match dp {
                DoublePointValue::Off => Value::Bool(false),
                DoublePointValue::On => Value::Bool(true),
                DoublePointValue::Indeterminate | DoublePointValue::IndeterminateOrFaulty => {
                    Value::Null
                },
            }
        },
        voltage_iec104::DataValue::Normalized(v) => Value::Float(*v as f64),
        voltage_iec104::DataValue::Scaled(v) => Value::Integer(*v as i64),
        voltage_iec104::DataValue::Float(v) => Value::Float(*v as f64),
        voltage_iec104::DataValue::Counter(v) => Value::Integer(*v as i64),
        voltage_iec104::DataValue::Bitstring(v) => Value::Integer(*v as i64),
        voltage_iec104::DataValue::StepPosition(v) => Value::Integer(*v as i64),
        voltage_iec104::DataValue::BinaryCounter { value, .. } => Value::Integer(*value as i64),
    }
}

/// Convert Cp56Time2a to `DateTime<Utc>`.
fn cp56time2a_to_datetime(time: &Cp56Time2a) -> Option<DateTime<Utc>> {
    if time.invalid {
        return None;
    }

    // Year is stored as offset from 2000
    let year = 2000 + time.year as i32;
    let month = time.month as u32;
    let day = time.day as u32;
    let hour = time.hours as u32;
    let minute = time.minutes as u32;
    let second = (time.milliseconds / 1000) as u32;
    let millisecond = (time.milliseconds % 1000) as u32;

    Utc.with_ymd_and_hms(year, month, day, hour, minute, second)
        .single()
        .map(|dt| dt + chrono::Duration::milliseconds(millisecond as i64))
}

/// Create Cp56Time2a from current time.
fn cp56time2a_now() -> Cp56Time2a {
    use chrono::Datelike;
    use chrono::Timelike;

    let now = Utc::now();

    Cp56Time2a {
        milliseconds: now.second() as u16 * 1000 + now.timestamp_subsec_millis() as u16,
        minutes: now.minute() as u8,
        hours: now.hour() as u8,
        day: now.day() as u8,
        day_of_week: now.weekday().num_days_from_monday() as u8 + 1, // 1=Monday
        month: now.month() as u8,
        year: ((now.year() as u16).saturating_sub(2000) & 0x7F) as u8,
        invalid: false,
        summer_time: false,
    }
}

/// Convert IEC 104 Quality to Quality.
fn convert_iec104_quality(quality: &voltage_iec104::Quality) -> Quality {
    if quality.is_good() {
        Quality::Good
    } else if quality.invalid {
        Quality::Invalid
    } else {
        Quality::Uncertain
    }
}

// ============================================================================
// HasMetadata Implementation
// ============================================================================

use crate::protocols::core::metadata::{
    DriverMetadata, HasMetadata, ParameterMetadata, ParameterType,
};

impl HasMetadata for Iec104Channel {
    #[allow(clippy::disallowed_methods)] // json! macro
    fn metadata() -> DriverMetadata {
        DriverMetadata {
            name: "iec104",
            display_name: "IEC 60870-5-104",
            description: "IEC 104 telecontrol protocol over TCP/IP for SCADA systems.",
            is_recommended: true,
            example_config: serde_json::json!({
                "address": "192.168.1.100:2404",
                "common_address": 1,
                "connect_timeout_ms": 10000,
                "t1_timeout_s": 15,
                "t2_timeout_s": 10,
                "t3_timeout_s": 20
            }),
            parameters: vec![
                ParameterMetadata::required(
                    "address",
                    "Server Address",
                    "IEC 104 server address in host:port format",
                    ParameterType::String,
                ),
                ParameterMetadata::optional(
                    "common_address",
                    "Common Address",
                    "ASDU common address (station address)",
                    ParameterType::Integer,
                    serde_json::json!(1),
                ),
                ParameterMetadata::optional(
                    "connect_timeout_ms",
                    "Connect Timeout (ms)",
                    "Connection timeout in milliseconds",
                    ParameterType::Integer,
                    serde_json::json!(10000),
                ),
                ParameterMetadata::optional(
                    "t1_timeout_s",
                    "T1 Timeout (s)",
                    "Send/receive APDU timeout in seconds",
                    ParameterType::Integer,
                    serde_json::json!(15),
                ),
                ParameterMetadata::optional(
                    "t2_timeout_s",
                    "T2 Timeout (s)",
                    "No data acknowledgement timeout in seconds",
                    ParameterType::Integer,
                    serde_json::json!(10),
                ),
                ParameterMetadata::optional(
                    "t3_timeout_s",
                    "T3 Timeout (s)",
                    "Test frame timeout in seconds",
                    ParameterType::Integer,
                    serde_json::json!(20),
                ),
            ],
        }
    }
}

// ============================================================================
// ChannelRuntime implementation (direct, no wrapper needed)
// ============================================================================

#[async_trait]
impl ChannelRuntime for Iec104Channel {
    fn id(&self) -> u32 {
        self.channel_id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn protocol(&self) -> &str {
        "iec104"
    }

    fn is_event_driven(&self) -> bool {
        true
    }

    async fn connect(&mut self) -> Result<()> {
        <Self as ProtocolClient>::connect(self).await
    }

    async fn disconnect(&mut self) -> Result<()> {
        <Self as ProtocolClient>::disconnect(self).await
    }

    async fn poll_once(&mut self) -> PollResult {
        <Self as ProtocolClient>::poll_once(self).await
    }

    async fn write_control(&mut self, commands: &[(u32, f64)]) -> Result<usize> {
        let cmds: Vec<_> = commands
            .iter()
            .map(|(id, value)| ControlCommand::latching(*id, *value != 0.0))
            .collect();
        let result = <Self as ProtocolClient>::write_control(self, &cmds).await?;
        Ok(result.success_count)
    }

    async fn write_adjustment(&mut self, adjustments: &[(u32, f64)]) -> Result<usize> {
        let adjs: Vec<_> = adjustments
            .iter()
            .map(|(id, value)| AdjustmentCommand::new(*id, *value))
            .collect();
        let result = <Self as ProtocolClient>::write_adjustment(self, &adjs).await?;
        Ok(result.success_count)
    }

    fn subscribe(&self) -> Option<DataEventReceiver> {
        Some(<Self as EventDrivenProtocol>::subscribe(self))
    }

    async fn start_events(&mut self) -> Result<()> {
        <Self as EventDrivenProtocol>::start(self).await
    }

    async fn stop_events(&mut self) -> Result<()> {
        <Self as EventDrivenProtocol>::stop(self).await
    }

    async fn diagnostics(&self) -> Result<Diagnostics> {
        <Self as Protocol>::diagnostics(self).await
    }

    fn connection_state(&self) -> ConnectionState {
        <Self as Protocol>::connection_state(self)
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // unwrap in tests
mod tests {
    use super::*;

    #[test]
    fn test_iec104_channel_config() {
        let config = Iec104ChannelConfig::new("127.0.0.1:2404")
            .with_common_address(1)
            .with_connect_timeout(Duration::from_secs(10));

        assert_eq!(config.address, "127.0.0.1:2404");
        assert_eq!(config.common_address, 1);
        assert_eq!(config.connect_timeout, Duration::from_secs(10));
    }

    #[test]
    fn test_iec104_channel_capabilities() {
        let config = Iec104ChannelConfig::new("127.0.0.1:2404");
        let channel = Iec104Channel::new(config, 1, "test_iec104".to_string());

        assert_eq!(ProtocolCapabilities::name(&channel), "IEC 60870-5-104");
        assert!(
            channel
                .supported_modes()
                .contains(&CommunicationMode::EventDriven)
        );
    }

    #[test]
    fn test_convert_iec104_value() {
        assert_eq!(
            convert_iec104_value(&voltage_iec104::DataValue::Single(true)),
            Value::Bool(true)
        );
        assert_eq!(
            convert_iec104_value(&voltage_iec104::DataValue::Float(23.5)),
            Value::Float(23.5)
        );
        assert_eq!(
            convert_iec104_value(&voltage_iec104::DataValue::Scaled(100)),
            Value::Integer(100)
        );
    }

    #[test]
    fn test_cp56time2a_conversion() {
        let time = Cp56Time2a {
            milliseconds: 30500, // 30 seconds, 500 ms
            minutes: 15,
            hours: 10,
            day: 25,
            day_of_week: 3,
            month: 12,
            year: 24, // 2024
            invalid: false,
            summer_time: false,
        };

        let dt = cp56time2a_to_datetime(&time).unwrap();
        assert_eq!(
            dt.format("%Y-%m-%d %H:%M:%S").to_string(),
            "2024-12-25 10:15:30"
        );
    }
}
