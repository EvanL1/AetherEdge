//! IEC 60870-5-104 protocol adapter.
//!
//! This module provides the `Iec104Channel` adapter that integrates
//! `voltage_iec104` with the protocol layer's `ChannelRuntime` trait.
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
//! let mut channel = Iec104Channel::new(config);
//! channel.connect().await?;
//! channel.start_data_transfer().await?;
//!
//! // Take the runtime's single event receiver
//! let mut rx = channel.take_event_receiver().expect("event receiver available");
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
use crate::protocols::core::traits::{
    AdjustmentCommand, ConnectionState, ControlCommand, DataEvent, DataEventReceiver,
    DataEventSender, Diagnostics, PointFailure, PollResult, WriteResult, data_event_channel,
};
use crate::protocols::runtime::ChannelRuntime;
use aether_config::io::MAX_CHANNEL_TIMING_MS;
use aether_core::PointType;
use aether_domain::PointQuality;

/// IEC 60870-5-104 information object address owned by this adapter.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Iec104Address {
    pub ioa: u32,
    pub type_id: u8,
}

impl Iec104Address {
    pub fn new(ioa: u32, type_id: u8) -> Self {
        Self { ioa, type_id }
    }
}

/// Decode the persisted IEC 104 point mapping owned by this adapter.
///
/// Accepted representations are a JSON address string, an object containing
/// `address`, or the structured `{ "ioa": ..., "type_id": ... }` form.
pub(crate) fn parse_point_mapping(mapping: &str) -> Result<Iec104Address> {
    let value: serde_json::Value = serde_json::from_str(mapping)
        .map_err(|error| GatewayError::Config(format!("invalid IEC 104 point mapping: {error}")))?;
    parse_point_mapping_value(&value)
}

/// Decode an IEC 104 point mapping from an already parsed JSON value.
pub(crate) fn parse_point_mapping_value(mapping: &serde_json::Value) -> Result<Iec104Address> {
    let values = match mapping {
        serde_json::Value::String(address) => return parse_point_address(address),
        serde_json::Value::Object(values) => values,
        _ => {
            return Err(GatewayError::Config(
                "IEC 104 point mapping must be a string or object".to_string(),
            ));
        },
    };

    let allowed_fields = if values.contains_key("address") {
        &["address"][..]
    } else {
        &["ioa", "type_id"][..]
    };
    if let Some(field) = values
        .keys()
        .find(|field| !allowed_fields.contains(&field.as_str()))
    {
        return Err(GatewayError::Config(format!(
            "unknown IEC 104 point mapping field '{field}'"
        )));
    }

    if let Some(address) = values.get("address") {
        let address = address.as_str().ok_or_else(|| {
            GatewayError::Config("IEC 104 point 'address' must be a string".to_string())
        })?;
        return parse_point_address(address);
    }

    let ioa = values
        .get("ioa")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            GatewayError::Config(
                "IEC 104 point mapping requires 'address' or numeric 'ioa'".to_string(),
            )
        })?;
    let ioa = u32::try_from(ioa)
        .map_err(|_| GatewayError::Config("IEC 104 point 'ioa' exceeds u32".to_string()))?;
    let type_id = match values.get("type_id") {
        Some(value) => {
            let value = value.as_u64().ok_or_else(|| {
                GatewayError::Config("IEC 104 point 'type_id' must be a u8".to_string())
            })?;
            u8::try_from(value).map_err(|_| {
                GatewayError::Config("IEC 104 point 'type_id' exceeds u8".to_string())
            })?
        },
        None => 0,
    };

    Ok(Iec104Address::new(ioa, type_id))
}

fn parse_point_address(address: &str) -> Result<Iec104Address> {
    let (ioa, type_id) = address.split_once(':').unwrap_or((address, ""));
    let ioa = ioa.trim().parse::<u32>().map_err(|_| {
        GatewayError::Config(format!("invalid IEC 104 information object address: {ioa}"))
    })?;
    let type_id = if type_id.is_empty() {
        0
    } else {
        type_id.trim().parse::<u8>().map_err(|_| {
            GatewayError::Config(format!("invalid IEC 104 point type ID: {type_id}"))
        })?
    };
    Ok(Iec104Address::new(ioa, type_id))
}

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
    pub points: Vec<PointConfig<Iec104Address>>,

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
    pub fn with_points(mut self, points: Vec<PointConfig<Iec104Address>>) -> Self {
        // Build IOA mapping from point configs (includes point_type for DataPoint creation)
        for point in &points {
            self.ioa_mapping
                .insert(point.address.ioa, (point.id, point.point_type));
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
#[serde(deny_unknown_fields)]
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
    pub(crate) fn validate(&self) -> Result<()> {
        let address = self.address.trim();
        let (host, port) = address
            .rsplit_once(':')
            .ok_or_else(|| GatewayError::Config("IEC 104 address must be host:port".to_owned()))?;
        if host.trim().is_empty() || port.parse::<u16>().ok().is_none_or(|port| port == 0) {
            return Err(GatewayError::Config(
                "IEC 104 address must contain a nonblank host and nonzero u16 port".to_owned(),
            ));
        }
        if self.common_address == 0 {
            return Err(GatewayError::Config(
                "IEC 104 common_address must be greater than zero".to_owned(),
            ));
        }
        if !(1..=MAX_CHANNEL_TIMING_MS).contains(&self.connect_timeout_ms) {
            return Err(GatewayError::Config(format!(
                "IEC 104 connect_timeout_ms must be between 1 and {MAX_CHANNEL_TIMING_MS}"
            )));
        }
        let max_timeout_seconds = MAX_CHANNEL_TIMING_MS / 1_000;
        for (name, value) in [
            ("t1_timeout_s", self.t1_timeout_s),
            ("t2_timeout_s", self.t2_timeout_s),
            ("t3_timeout_s", self.t3_timeout_s),
        ] {
            if !(1..=max_timeout_seconds).contains(&value) {
                return Err(GatewayError::Config(format!(
                    "IEC 104 {name} must be between 1 and {max_timeout_seconds}"
                )));
            }
        }
        if self.t2_timeout_s >= self.t1_timeout_s {
            return Err(GatewayError::Config(
                "IEC 104 t2_timeout_s must be less than t1_timeout_s".to_owned(),
            ));
        }
        Ok(())
    }

    /// Convert to Iec104ChannelConfig.
    ///
    /// Note: Points must be set separately via `with_points()`.
    pub fn into_config(self) -> Iec104ChannelConfig {
        Iec104ChannelConfig::new(self.address)
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
/// the protocol layer's `ChannelRuntime` trait.
///
/// Note: This adapter follows the "protocol layer separated from storage" design.
/// The channel returns DataBatch via events; the service layer handles persistence.
pub struct Iec104Channel {
    config: Iec104ChannelConfig,
    client: Iec104Client,
    state: Arc<std::sync::RwLock<ConnectionState>>,
    diagnostics: Arc<AtomicDiagnostics>,
    /// Last interrogation timestamp (Unix millis, 0 = never)
    last_interrogation_ms: AtomicU64,
    /// Event sender for the unified channel task.
    event_tx: DataEventSender,
    /// Sole event receiver, taken once by the unified channel task.
    event_rx: Option<DataEventReceiver>,
    poll_task: Option<tokio::task::JoinHandle<()>>,
    /// Point ID -> index lookup for O(1) access
    point_index: HashMap<u32, usize>,
}

impl Iec104Channel {
    /// Create a new IEC 104 channel.
    pub fn new(config: Iec104ChannelConfig) -> Self {
        let client_config = config.to_client_config();
        let client = Iec104Client::new(client_config);
        let (event_tx, event_rx) = data_event_channel();

        // Build point ID -> index mapping for O(1) lookup
        let point_index: HashMap<u32, usize> = config
            .points
            .iter()
            .enumerate()
            .map(|(i, p)| (p.id, i))
            .collect();

        Self {
            config,
            client,
            state: Arc::new(std::sync::RwLock::new(ConnectionState::Disconnected)),
            diagnostics: Arc::new(AtomicDiagnostics::new()),
            last_interrogation_ms: AtomicU64::new(0),
            event_tx,
            event_rx: Some(event_rx),
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
                    .try_send(DataEvent::ConnectionChanged(ConnectionState::Connected));
            },
            Iec104Event::Disconnected => {
                self.set_state(ConnectionState::Disconnected);
                let _ = self
                    .event_tx
                    .try_send(DataEvent::ConnectionChanged(ConnectionState::Disconnected));
            },
            Iec104Event::DataTransferStarted => {
                // Data transfer is active
            },
            Iec104Event::DataTransferStopped => {
                // Data transfer stopped
            },
            Iec104Event::DataUpdate(points) => {
                let PollResult { data, failures } = self.convert_data_points(points);
                for failure in failures {
                    self.record_error(format!(
                        "IEC 104 point {} rejected: {}",
                        failure.point_id, failure.error
                    ));
                }
                if !data.is_empty() {
                    // Send event (service layer handles storage)
                    let _ = self.event_tx.try_send(DataEvent::DataUpdate(data));

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
                let _ = self.event_tx.try_send(DataEvent::Error(msg));
            },
        }
    }

    /// Convert IEC 104 data points, preserving numeric samples when peers
    /// include indeterminate or non-finite values in the same update.
    fn convert_data_points(&self, points: Vec<voltage_iec104::DataPoint>) -> PollResult {
        let mut batch = DataBatch::with_capacity(points.len());
        let mut failures = Vec::new();

        for point in points {
            // Look up (point_id, point_type) from IOA, or use IOA with Telemetry as fallback
            let (point_id, point_type) = self
                .config
                .ioa_mapping
                .get(&point.ioa)
                .copied()
                .unwrap_or((point.ioa, PointType::Telemetry));

            let value = match convert_iec104_value(&point.value) {
                Ok(value) => value,
                Err(error) => {
                    failures.push(PointFailure::new(point_id, error));
                    continue;
                },
            };

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

        if failures.is_empty() {
            PollResult::success(batch)
        } else {
            PollResult::partial(batch, failures)
        }
    }

    /// Record an error (lock-free).
    fn record_error(&self, error: String) {
        self.diagnostics.record_error(error);
    }

    /// Find point config by ID (O(1) lookup).
    fn find_point(&self, id: u32) -> Option<&PointConfig<Iec104Address>> {
        self.point_index
            .get(&id)
            .map(|&idx| &self.config.points[idx])
    }

    /// Resolve point ID to IEC 104 address. Returns error tuple for failures vec.
    fn resolve_iec104_addr(
        &self,
        id: u32,
    ) -> std::result::Result<(&PointConfig<Iec104Address>, &Iec104Address), (u32, String)> {
        let point = self
            .find_point(id)
            .ok_or_else(|| (id, "Point not found".to_string()))?;
        Ok((point, &point.address))
    }
}

impl Iec104Channel {
    fn name(&self) -> &'static str {
        "IEC 60870-5-104"
    }
}

/// Convert an IEC 104 value to the numeric live-acquisition representation.
fn convert_iec104_value(
    value: &voltage_iec104::DataValue,
) -> std::result::Result<Value, &'static str> {
    match value {
        voltage_iec104::DataValue::Single(v) => Ok(Value::Bool(*v)),
        voltage_iec104::DataValue::Double(dp) => {
            use voltage_iec104::DoublePointValue;
            match dp {
                DoublePointValue::Off => Ok(Value::Bool(false)),
                DoublePointValue::On => Ok(Value::Bool(true)),
                DoublePointValue::Indeterminate | DoublePointValue::IndeterminateOrFaulty => {
                    Err("IEC 104 double-point value is indeterminate")
                },
            }
        },
        voltage_iec104::DataValue::Normalized(v) if v.is_finite() => Ok(Value::Float(*v as f64)),
        voltage_iec104::DataValue::Float(v) if v.is_finite() => Ok(Value::Float(*v as f64)),
        voltage_iec104::DataValue::Normalized(_) | voltage_iec104::DataValue::Float(_) => {
            Err("IEC 104 floating-point value is not finite")
        },
        voltage_iec104::DataValue::Scaled(v) => Ok(Value::Integer(*v as i64)),
        voltage_iec104::DataValue::Counter(v) => Ok(Value::Integer(*v as i64)),
        voltage_iec104::DataValue::Bitstring(v) => Ok(Value::Integer(*v as i64)),
        voltage_iec104::DataValue::StepPosition(v) => Ok(Value::Integer(*v as i64)),
        voltage_iec104::DataValue::BinaryCounter { value, .. } => Ok(Value::Integer(*value as i64)),
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
fn convert_iec104_quality(quality: &voltage_iec104::Quality) -> PointQuality {
    if quality.is_good() {
        PointQuality::Good
    } else if quality.invalid {
        PointQuality::Bad
    } else {
        PointQuality::Uncertain
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

impl Iec104Channel {
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
}

#[async_trait]
impl ChannelRuntime for Iec104Channel {
    fn is_event_driven(&self) -> bool {
        true
    }

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
                self.diagnostics.add_error(1);
                // Return empty result with no failure tracking (connection-level error)
                return PollResult::success(DataBatch::new());
            },
        };

        let result = if let Some(voltage_iec104::Iec104Event::DataUpdate(points)) = event {
            self.convert_data_points(points)
        } else {
            PollResult::success(DataBatch::new())
        };

        if !result.data.is_empty() {
            // Lock-free read count increment
            self.diagnostics.inc_read();
        }

        result
    }

    async fn write_control(&mut self, commands: &[(u32, f64)]) -> Result<usize> {
        let cmds: Vec<_> = commands
            .iter()
            .map(|(id, value)| ControlCommand::latching(*id, *value != 0.0))
            .collect();
        let result = Self::write_control(self, &cmds).await?;
        Ok(result.success_count)
    }

    async fn write_adjustment(&mut self, adjustments: &[(u32, f64)]) -> Result<usize> {
        let adjs: Vec<_> = adjustments
            .iter()
            .map(|(id, value)| AdjustmentCommand::new(*id, *value))
            .collect();
        let result = Self::write_adjustment(self, &adjs).await?;
        Ok(result.success_count)
    }

    fn take_event_receiver(&mut self) -> Option<DataEventReceiver> {
        self.event_rx.take()
    }

    async fn start_events(&mut self) -> Result<()> {
        // For IEC 104, start means starting data transfer and initial GI
        self.start_data_transfer().await?;
        self.general_interrogation().await?;
        Ok(())
    }

    async fn stop_events(&mut self) -> Result<()> {
        // Abort poll task if running
        if let Some(task) = self.poll_task.take() {
            task.abort();
        }
        self.stop_data_transfer().await
    }

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
            protocol: self.name().to_string(),
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

    fn connection_state(&self) -> ConnectionState {
        self.get_state()
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // unwrap in tests
mod tests {
    use super::*;

    #[test]
    fn persisted_point_mapping_accepts_string_form() {
        let address = parse_point_mapping(r#""1001:13""#).expect("valid string mapping");

        assert_eq!((address.ioa, address.type_id), (1001, 13));
    }

    #[test]
    fn persisted_point_mapping_accepts_object_forms_without_cloning_value() {
        for (mapping, expected) in [
            (serde_json::json!({"address": "1001:13"}), (1001, 13)),
            (serde_json::json!({"ioa": 1001, "type_id": 13}), (1001, 13)),
        ] {
            let address =
                parse_point_mapping_value(&mapping).expect("valid borrowed object mapping");
            assert_eq!((address.ioa, address.type_id), expected);
        }
    }

    #[test]
    fn persisted_point_mapping_rejects_unknown_fields() {
        for mapping in [
            serde_json::json!({"address": "1001:13", "offset": 1}),
            serde_json::json!({"ioa": 1001, "type_id": 13, "offset": 1}),
        ] {
            assert!(parse_point_mapping_value(&mapping).is_err());
        }
    }

    #[test]
    fn persisted_point_mapping_rejects_malformed_values() {
        for mapping in [
            "null",
            r#"{"address":1}"#,
            r#"{"ioa":4294967296}"#,
            r#"{"ioa":1,"type_id":256}"#,
        ] {
            assert!(parse_point_mapping(mapping).is_err(), "{mapping}");
        }
    }

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
        let channel = Iec104Channel::new(config);

        assert_eq!(channel.name(), "IEC 60870-5-104");
    }

    #[tokio::test]
    async fn event_receiver_is_taken_once_and_receives_updates() {
        let mut channel = Iec104Channel::new(Iec104ChannelConfig::new("127.0.0.1:2404"));
        let mut receiver = channel
            .take_event_receiver()
            .expect("event receiver available");
        assert!(channel.take_event_receiver().is_none());

        let sent = channel
            .event_tx
            .try_send(DataEvent::ConnectionChanged(ConnectionState::Connected));

        assert!(sent.is_ok());
        assert!(matches!(
            receiver.recv().await,
            Some(DataEvent::ConnectionChanged(ConnectionState::Connected))
        ));
    }

    #[test]
    fn test_convert_iec104_value() {
        assert_eq!(
            convert_iec104_value(&voltage_iec104::DataValue::Single(true))
                .expect("single point is numeric"),
            Value::Bool(true)
        );
        assert_eq!(
            convert_iec104_value(&voltage_iec104::DataValue::Float(23.5))
                .expect("floating point is numeric"),
            Value::Float(23.5)
        );
        assert_eq!(
            convert_iec104_value(&voltage_iec104::DataValue::Scaled(100))
                .expect("scaled point is numeric"),
            Value::Integer(100)
        );
    }

    #[test]
    fn mixed_acquisition_keeps_numeric_points_and_reports_indeterminate_values() {
        use voltage_iec104::{DataPoint as Iec104DataPoint, DoublePointValue};

        let channel = Iec104Channel::new(Iec104ChannelConfig::new("127.0.0.1:2404"));
        let result = channel.convert_data_points(vec![
            Iec104DataPoint::new(10, voltage_iec104::DataValue::Float(23.5)),
            Iec104DataPoint::new(
                11,
                voltage_iec104::DataValue::Double(DoublePointValue::Indeterminate),
            ),
            Iec104DataPoint::new(12, voltage_iec104::DataValue::Single(true)),
            Iec104DataPoint::new(13, voltage_iec104::DataValue::Float(f32::NAN)),
        ]);

        assert_eq!(result.data.len(), 2);
        assert_eq!(
            result.data.iter().map(|point| point.id).collect::<Vec<_>>(),
            vec![10, 12]
        );
        assert_eq!(
            result
                .failures
                .iter()
                .map(|failure| failure.point_id)
                .collect::<Vec<_>>(),
            vec![11, 13]
        );
    }

    #[tokio::test]
    async fn mixed_event_update_emits_numeric_points_and_records_rejected_point() {
        use voltage_iec104::{DataPoint as Iec104DataPoint, DoublePointValue};

        let mut channel = Iec104Channel::new(Iec104ChannelConfig::new("127.0.0.1:2404"));
        let mut receiver = channel
            .take_event_receiver()
            .expect("event receiver available");

        channel
            .handle_iec104_event(Iec104Event::DataUpdate(vec![
                Iec104DataPoint::new(10, voltage_iec104::DataValue::Scaled(100)),
                Iec104DataPoint::new(
                    11,
                    voltage_iec104::DataValue::Double(DoublePointValue::IndeterminateOrFaulty),
                ),
            ]))
            .await;

        let DataEvent::DataUpdate(batch) = receiver.recv().await.expect("numeric update emitted")
        else {
            panic!("expected data update");
        };
        assert_eq!(batch.len(), 1);
        assert_eq!(batch.iter().next().map(|point| point.id), Some(10));
        assert_eq!(channel.diagnostics.error_count(), 1);
        assert!(
            channel
                .diagnostics
                .last_error()
                .is_some_and(|error| error.contains("point 11 rejected"))
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
