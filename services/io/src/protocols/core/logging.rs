//! Unified, protocol-agnostic channel logging system.
//!
//! Implement `ChannelLogHandler` to receive log events from all protocol channels.
//! Use `LogContext` within protocol implementations for convenient event emission.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use aether_model::PointType;

use crate::protocols::core::data::Value;
use crate::protocols::core::quality::Quality;
use crate::protocols::core::{
    AdjustmentCommand, ConnectionState, ControlCommand, ReadRequest, ReadResponse, WriteResult,
};

/// Direction of a raw packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PacketDirection {
    /// Packet sent to device/server.
    Send,
    /// Packet received from device/server.
    Receive,
}

impl std::fmt::Display for PacketDirection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Send => write!(f, ">>>"),
            Self::Receive => write!(f, "<<<"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModbusTransportType {
    Tcp,
    Rtu,
    Ascii,
}

/// Protocol-specific packet metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum PacketMetadata {
    Modbus {
        transport: ModbusTransportType,
        slave_id: u8,
        function_code: u8,
        #[serde(skip_serializing_if = "Option::is_none")]
        transaction_id: Option<u16>,
        #[serde(skip_serializing_if = "Option::is_none")]
        start_address: Option<u16>,
        #[serde(skip_serializing_if = "Option::is_none")]
        quantity: Option<u16>,
    },
    Iec104 {
        asdu_type: u8,
        cause_of_tx: u8,
        common_addr: u16,
    },
    OpcUa {
        message_type: String,
        request_id: u32,
    },
    J1939 {
        pgn: u32,
        source: u8,
        destination: u8,
    },
    Gpio,
    Virtual,
    Other {
        protocol: String,
    },
}

impl PacketMetadata {
    pub fn modbus_tcp(slave_id: u8, function_code: u8) -> Self {
        Self::Modbus {
            transport: ModbusTransportType::Tcp,
            slave_id,
            function_code,
            transaction_id: None,
            start_address: None,
            quantity: None,
        }
    }

    pub fn iec104(asdu_type: u8, cause_of_tx: u8, common_addr: u16) -> Self {
        Self::Iec104 {
            asdu_type,
            cause_of_tx,
            common_addr,
        }
    }

    pub fn protocol_name(&self) -> &str {
        match self {
            Self::Modbus { transport, .. } => match transport {
                ModbusTransportType::Tcp => "modbus-tcp",
                ModbusTransportType::Rtu => "modbus-rtu",
                ModbusTransportType::Ascii => "modbus-ascii",
            },
            Self::Iec104 { .. } => "iec104",
            Self::OpcUa { .. } => "opcua",
            Self::J1939 { .. } => "j1939",
            Self::Gpio => "gpio",
            Self::Virtual => "virtual",
            Self::Other { protocol } => protocol,
        }
    }
}

/// Context in which an error occurred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorContext {
    Connection,
    Read,
    WriteControl,
    WriteAdjustment,
    Polling,
    /// Polling failure on a specific register range.
    #[serde(rename = "polling_segment")]
    PollingSegment {
        start: u16,
        end: u16,
    },
    Protocol,
    Unknown,
}

impl std::fmt::Display for ErrorContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connection => write!(f, "connection"),
            Self::Read => write!(f, "read"),
            Self::WriteControl => write!(f, "write_control"),
            Self::WriteAdjustment => write!(f, "write_adjustment"),
            Self::Polling => write!(f, "polling"),
            Self::PollingSegment { start, end } => write!(f, "polling @{}-{}", start, end),
            Self::Protocol => write!(f, "protocol"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

/// Lightweight point value summary for logging (avoids cloning full `DataPoint`).
#[derive(Debug, Clone)]
pub struct PointValueSummary {
    pub id: u32,
    pub point_type: PointType,
    /// Pre-formatted value string (booleans as "1"/"0").
    pub value: String,
    pub quality: Quality,
}

impl PointValueSummary {
    pub fn new(id: u32, point_type: PointType, value: &Value, quality: Quality) -> Self {
        let value_str = match value {
            Value::Bool(b) => {
                if *b {
                    "1".to_string()
                } else {
                    "0".to_string()
                }
            },
            Value::Float(f) => {
                // Format floats without unnecessary trailing zeros
                if f.fract() == 0.0 {
                    format!("{:.0}", f)
                } else {
                    format!("{:.2}", f)
                }
            },
            Value::Integer(i) => i.to_string(),
            Value::String(s) => s.clone(),
            Value::Bytes(b) => format!("[{}B]", b.len()),
            Value::Null => "null".to_string(),
        };

        Self {
            id,
            point_type,
            value: value_str,
            quality,
        }
    }
}

/// All possible events that can be logged from a channel.
#[derive(Debug, Clone)]
pub enum ChannelLogEvent {
    Connected {
        timestamp: SystemTime,
        endpoint: String,
        duration_ms: u64,
    },
    Disconnected {
        timestamp: SystemTime,
        reason: Option<String>,
    },
    ReadOperation {
        timestamp: SystemTime,
        request: ReadRequest,
        result: Result<ReadResponse, String>,
        duration_ms: u64,
    },
    PollCycleCompleted {
        timestamp: SystemTime,
        points_count: usize,
        duration_ms: u64,
        success_count: usize,
        failed_count: usize,
    },
    ControlWrite {
        timestamp: SystemTime,
        commands: Vec<ControlCommand>,
        result: Result<WriteResult, String>,
        duration_ms: u64,
    },
    AdjustmentWrite {
        timestamp: SystemTime,
        commands: Vec<AdjustmentCommand>,
        result: Result<WriteResult, String>,
        duration_ms: u64,
    },
    Error {
        timestamp: SystemTime,
        error: String,
        context: ErrorContext,
    },
    ReconnectAttempt {
        timestamp: SystemTime,
        attempt: u32,
        max_attempts: Option<u32>,
        next_retry_ms: Option<u64>,
    },
    ReconnectSuccess {
        timestamp: SystemTime,
        total_attempts: u32,
        total_duration_ms: u64,
    },
    StateChanged {
        timestamp: SystemTime,
        old_state: ConnectionState,
        new_state: ConnectionState,
    },
    RawPacket {
        timestamp: SystemTime,
        direction: PacketDirection,
        data: Vec<u8>,
        metadata: PacketMetadata,
        /// Group ID for correlating packets with point values.
        group_id: Option<u32>,
    },
    /// Point values collected from poll cycle (Info level).
    PointValues {
        timestamp: SystemTime,
        values: Vec<PointValueSummary>,
        total_points: usize,
        group_id: Option<u32>,
    },
}

impl ChannelLogEvent {
    pub fn timestamp(&self) -> SystemTime {
        match self {
            Self::Connected { timestamp, .. } => *timestamp,
            Self::Disconnected { timestamp, .. } => *timestamp,
            Self::ReadOperation { timestamp, .. } => *timestamp,
            Self::PollCycleCompleted { timestamp, .. } => *timestamp,
            Self::ControlWrite { timestamp, .. } => *timestamp,
            Self::AdjustmentWrite { timestamp, .. } => *timestamp,
            Self::Error { timestamp, .. } => *timestamp,
            Self::ReconnectAttempt { timestamp, .. } => *timestamp,
            Self::ReconnectSuccess { timestamp, .. } => *timestamp,
            Self::StateChanged { timestamp, .. } => *timestamp,
            Self::RawPacket { timestamp, .. } => *timestamp,
            Self::PointValues { timestamp, .. } => *timestamp,
        }
    }

    pub fn event_type(&self) -> &'static str {
        match self {
            Self::Connected { .. } => "connected",
            Self::Disconnected { .. } => "disconnected",
            Self::ReadOperation { .. } => "read_operation",
            Self::PollCycleCompleted { .. } => "poll_cycle",
            Self::ControlWrite { .. } => "control_write",
            Self::AdjustmentWrite { .. } => "adjustment_write",
            Self::Error { .. } => "error",
            Self::ReconnectAttempt { .. } => "reconnect_attempt",
            Self::ReconnectSuccess { .. } => "reconnect_success",
            Self::StateChanged { .. } => "state_changed",
            Self::RawPacket { .. } => "raw_packet",
            Self::PointValues { .. } => "point_values",
        }
    }
}

/// Log event type for filtering configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogEventType {
    Connected,
    Disconnected,
    ReadOperation,
    PollCycle,
    ControlWrite,
    AdjustmentWrite,
    Error,
    ReconnectAttempt,
    ReconnectSuccess,
    StateChanged,
    RawPacket,
    PointValues,
}

impl LogEventType {
    pub fn all() -> HashSet<LogEventType> {
        use LogEventType::*;
        [
            Connected,
            Disconnected,
            ReadOperation,
            PollCycle,
            ControlWrite,
            AdjustmentWrite,
            Error,
            ReconnectAttempt,
            ReconnectSuccess,
            StateChanged,
            RawPacket,
            PointValues,
        ]
        .into_iter()
        .collect()
    }

    pub fn default_set() -> HashSet<LogEventType> {
        use LogEventType::*;
        [
            Connected,
            Disconnected,
            ControlWrite,
            AdjustmentWrite,
            Error,
            ReconnectAttempt,
            ReconnectSuccess,
            StateChanged,
            PointValues, // Info level: point values for operational monitoring
        ]
        .into_iter()
        .collect()
    }

    pub fn errors_and_connections() -> HashSet<LogEventType> {
        use LogEventType::*;
        [
            Connected,
            Disconnected,
            Error,
            ReconnectAttempt,
            ReconnectSuccess,
            StateChanged,
        ]
        .into_iter()
        .collect()
    }
}

/// Channel logging configuration — controls which events are logged.
#[derive(Debug, Clone)]
pub struct ChannelLogConfig {
    enabled_events: HashSet<LogEventType>,
    log_successful_reads: bool,
    log_successful_writes: bool,
    poll_cycle_sample_rate: u32,
    log_raw_packets: bool,
    max_packet_size: usize,
}

impl Default for ChannelLogConfig {
    fn default() -> Self {
        Self {
            enabled_events: LogEventType::default_set(),
            log_successful_reads: false,
            log_successful_writes: true,
            poll_cycle_sample_rate: 1,
            log_raw_packets: false,
            max_packet_size: 0,
        }
    }
}

impl ChannelLogConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn all() -> Self {
        Self {
            enabled_events: LogEventType::all(),
            log_successful_reads: true,
            log_successful_writes: true,
            poll_cycle_sample_rate: 1,
            log_raw_packets: true,
            max_packet_size: 0,
        }
    }

    pub fn errors_only() -> Self {
        Self {
            enabled_events: LogEventType::errors_and_connections(),
            log_successful_reads: false,
            log_successful_writes: false,
            poll_cycle_sample_rate: 0,
            log_raw_packets: false,
            max_packet_size: 0,
        }
    }

    pub fn disabled() -> Self {
        Self {
            enabled_events: HashSet::new(),
            log_successful_reads: false,
            log_successful_writes: false,
            poll_cycle_sample_rate: 0,
            log_raw_packets: false,
            max_packet_size: 0,
        }
    }

    #[must_use]
    pub fn enable_event(mut self, event_type: LogEventType) -> Self {
        self.enabled_events.insert(event_type);
        self
    }

    #[must_use]
    pub fn with_raw_packets(mut self, enable: bool) -> Self {
        self.log_raw_packets = enable;
        if enable && !self.enabled_events.contains(&LogEventType::RawPacket) {
            self.enabled_events.insert(LogEventType::RawPacket);
        }
        self
    }

    pub fn is_enabled(&self, event_type: LogEventType) -> bool {
        self.enabled_events.contains(&event_type)
    }

    pub fn should_log_raw_packets(&self) -> bool {
        self.log_raw_packets && self.enabled_events.contains(&LogEventType::RawPacket)
    }

    pub fn should_log(&self, event: &ChannelLogEvent) -> bool {
        let event_type = match event {
            ChannelLogEvent::Connected { .. } => LogEventType::Connected,
            ChannelLogEvent::Disconnected { .. } => LogEventType::Disconnected,
            ChannelLogEvent::ReadOperation { result, .. } => {
                if !self.enabled_events.contains(&LogEventType::ReadOperation) {
                    return false;
                }
                if result.is_ok() && !self.log_successful_reads {
                    return false;
                }
                LogEventType::ReadOperation
            },
            ChannelLogEvent::PollCycleCompleted { .. } => LogEventType::PollCycle,
            ChannelLogEvent::ControlWrite { result, .. } => {
                if !self.enabled_events.contains(&LogEventType::ControlWrite) {
                    return false;
                }
                if result.is_ok() && !self.log_successful_writes {
                    return false;
                }
                LogEventType::ControlWrite
            },
            ChannelLogEvent::AdjustmentWrite { result, .. } => {
                if !self.enabled_events.contains(&LogEventType::AdjustmentWrite) {
                    return false;
                }
                if result.is_ok() && !self.log_successful_writes {
                    return false;
                }
                LogEventType::AdjustmentWrite
            },
            ChannelLogEvent::Error { .. } => LogEventType::Error,
            ChannelLogEvent::ReconnectAttempt { .. } => LogEventType::ReconnectAttempt,
            ChannelLogEvent::ReconnectSuccess { .. } => LogEventType::ReconnectSuccess,
            ChannelLogEvent::StateChanged { .. } => LogEventType::StateChanged,
            ChannelLogEvent::RawPacket { .. } => {
                if !self.log_raw_packets {
                    return false;
                }
                LogEventType::RawPacket
            },
            ChannelLogEvent::PointValues { .. } => LogEventType::PointValues,
        };

        self.enabled_events.contains(&event_type)
    }
}

/// Protocol-agnostic channel log handler trait.
#[async_trait]
pub trait ChannelLogHandler: Send + Sync {
    async fn on_log(&self, channel_id: u32, event: ChannelLogEvent);

    /// Set the log level dynamically (for hot-reload support).
    fn set_log_level(&self, _level: &str) {}
}

/// Logging context for use within protocol implementations.
pub struct LogContext {
    channel_id: u32,
    handler: Option<Arc<dyn ChannelLogHandler>>,
    config: ChannelLogConfig,
    poll_counter: AtomicU64,
}

impl LogContext {
    pub fn new(channel_id: u32) -> Self {
        Self {
            channel_id,
            handler: None,
            config: ChannelLogConfig::default(),
            poll_counter: AtomicU64::new(0),
        }
    }

    #[must_use]
    pub fn with_handler(mut self, handler: Arc<dyn ChannelLogHandler>) -> Self {
        self.handler = Some(handler);
        self
    }

    #[must_use]
    pub fn with_config(mut self, config: ChannelLogConfig) -> Self {
        self.config = config;
        self
    }

    pub fn set_handler(&mut self, handler: Arc<dyn ChannelLogHandler>) {
        self.handler = Some(handler);
    }

    pub fn set_config(&mut self, config: ChannelLogConfig) {
        self.config = config;
    }

    pub fn config(&self) -> &ChannelLogConfig {
        &self.config
    }

    pub fn channel_id(&self) -> u32 {
        self.channel_id
    }

    pub fn handler(&self) -> Option<Arc<dyn ChannelLogHandler>> {
        self.handler.clone()
    }

    pub async fn log(&self, event: ChannelLogEvent) {
        if let Some(handler) = &self.handler
            && self.config.should_log(&event)
        {
            handler.on_log(self.channel_id, event).await;
        }
    }

    fn should_log_poll_cycle(&self) -> bool {
        if !self.config.is_enabled(LogEventType::PollCycle) {
            return false;
        }
        let rate = self.config.poll_cycle_sample_rate;
        if rate == 0 {
            return false;
        }
        if rate == 1 {
            return true;
        }
        let count = self.poll_counter.fetch_add(1, Ordering::Relaxed);
        count.is_multiple_of(rate as u64)
    }

    pub async fn log_connected(&self, endpoint: impl Into<String>, duration_ms: u64) {
        self.log(ChannelLogEvent::Connected {
            timestamp: SystemTime::now(),
            endpoint: endpoint.into(),
            duration_ms,
        })
        .await;
    }

    pub async fn log_disconnected(&self, reason: Option<String>) {
        self.log(ChannelLogEvent::Disconnected {
            timestamp: SystemTime::now(),
            reason,
        })
        .await;
    }

    pub async fn log_error(&self, error: impl Into<String>, context: ErrorContext) {
        self.log(ChannelLogEvent::Error {
            timestamp: SystemTime::now(),
            error: error.into(),
            context,
        })
        .await;
    }

    pub async fn log_state_changed(&self, old_state: ConnectionState, new_state: ConnectionState) {
        self.log(ChannelLogEvent::StateChanged {
            timestamp: SystemTime::now(),
            old_state,
            new_state,
        })
        .await;
    }

    pub async fn log_control_write(
        &self,
        commands: &[ControlCommand],
        result: Result<WriteResult, String>,
        duration_ms: u64,
    ) {
        if self.handler.is_none() || !self.config.is_enabled(LogEventType::ControlWrite) {
            return;
        }

        self.log(ChannelLogEvent::ControlWrite {
            timestamp: SystemTime::now(),
            commands: commands.to_vec(),
            result,
            duration_ms,
        })
        .await;
    }

    pub async fn log_adjustment_write(
        &self,
        commands: &[AdjustmentCommand],
        result: Result<WriteResult, String>,
        duration_ms: u64,
    ) {
        if self.handler.is_none() || !self.config.is_enabled(LogEventType::AdjustmentWrite) {
            return;
        }

        self.log(ChannelLogEvent::AdjustmentWrite {
            timestamp: SystemTime::now(),
            commands: commands.to_vec(),
            result,
            duration_ms,
        })
        .await;
    }

    pub async fn log_poll_cycle(
        &self,
        points_count: usize,
        duration_ms: u64,
        success_count: usize,
        failed_count: usize,
    ) {
        if self.should_log_poll_cycle() {
            self.log(ChannelLogEvent::PollCycleCompleted {
                timestamp: SystemTime::now(),
                points_count,
                duration_ms,
                success_count,
                failed_count,
            })
            .await;
        }
    }

    /// Log a raw packet event with optional group ID for correlation.
    pub async fn log_raw_packet(
        &self,
        direction: PacketDirection,
        mut data: Vec<u8>,
        metadata: PacketMetadata,
        group_id: Option<u32>,
    ) {
        if !self.config.should_log_raw_packets() {
            return;
        }

        // Apply max packet size truncation (in-place, no reallocation)
        if self.config.max_packet_size > 0 && data.len() > self.config.max_packet_size {
            data.truncate(self.config.max_packet_size);
        }

        self.log(ChannelLogEvent::RawPacket {
            timestamp: SystemTime::now(),
            direction,
            data,
            metadata,
            group_id,
        })
        .await;
    }

    /// Log point values with optional group ID for correlation with raw packets.
    pub async fn log_point_values(
        &self,
        points: &[(u32, PointType, Value, Quality)],
        group_id: Option<u32>,
    ) {
        // Fast path: skip if PointValues logging is disabled
        if !self.config.is_enabled(LogEventType::PointValues) {
            return;
        }

        if points.is_empty() {
            return;
        }

        // Convert to summaries
        let values: Vec<PointValueSummary> = points
            .iter()
            .map(|(id, point_type, value, quality)| {
                PointValueSummary::new(*id, *point_type, value, *quality)
            })
            .collect();

        let total_points = values.len();

        self.log(ChannelLogEvent::PointValues {
            timestamp: SystemTime::now(),
            values,
            total_points,
            group_id,
        })
        .await;
    }
}

impl Clone for LogContext {
    fn clone(&self) -> Self {
        Self {
            channel_id: self.channel_id,
            handler: self.handler.clone(),
            config: self.config.clone(),
            poll_counter: AtomicU64::new(self.poll_counter.load(Ordering::Relaxed)),
        }
    }
}

/// Trait for protocols that support logging.
pub trait LoggableProtocol {
    fn set_log_handler(&mut self, handler: Arc<dyn ChannelLogHandler>);
    fn set_log_config(&mut self, config: ChannelLogConfig);
    fn log_config(&self) -> &ChannelLogConfig;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_packet_metadata() {
        let meta = PacketMetadata::modbus_tcp(1, 0x03);
        assert_eq!(meta.protocol_name(), "modbus-tcp");

        let meta = PacketMetadata::iec104(36, 3, 1);
        assert_eq!(meta.protocol_name(), "iec104");
    }

    #[test]
    fn test_log_config() {
        let config = ChannelLogConfig::new();
        assert!(config.is_enabled(LogEventType::Connected));
        assert!(!config.is_enabled(LogEventType::PollCycle));

        let config = ChannelLogConfig::all();
        assert!(config.is_enabled(LogEventType::PollCycle));
        assert!(config.should_log_raw_packets());

        let config = ChannelLogConfig::disabled();
        assert!(!config.is_enabled(LogEventType::Connected));
    }

    #[test]
    fn test_log_event_type_sets() {
        let all = LogEventType::all();
        assert_eq!(all.len(), 12); // +1 for PointValues

        let default_set = LogEventType::default_set();
        assert!(!default_set.contains(&LogEventType::PollCycle));
        assert!(default_set.contains(&LogEventType::Error));
        assert!(default_set.contains(&LogEventType::PointValues)); // Info level includes PointValues
    }

    #[tokio::test]
    async fn test_log_context() {
        use std::sync::atomic::AtomicUsize;

        struct CountingHandler {
            count: AtomicUsize,
        }

        #[async_trait]
        impl ChannelLogHandler for CountingHandler {
            async fn on_log(&self, _channel_id: u32, _event: ChannelLogEvent) {
                self.count.fetch_add(1, Ordering::SeqCst);
            }
        }

        let handler = Arc::new(CountingHandler {
            count: AtomicUsize::new(0),
        });

        let ctx = LogContext::new(1)
            .with_handler(handler.clone())
            .with_config(ChannelLogConfig::all());

        ctx.log_connected("localhost:502", 100).await;
        ctx.log_error("test error", ErrorContext::Connection).await;

        assert_eq!(handler.count.load(Ordering::SeqCst), 2);
    }
}
