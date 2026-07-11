//! Channel communication types
//!
//! Core data types for channel communication in io.
//! These types were previously in aether-comlink but are now owned by io.

use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashMap;

use crate::core::config::FourRemote;

// ============================================================================
// Connection State
// ============================================================================

/// Connection state for communication channels
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ConnectionState {
    /// Not initialized yet
    #[default]
    Uninitialized,
    /// Initializing connection
    Initializing,
    /// Attempting to connect
    Connecting,
    /// Successfully connected
    Connected,
    /// Connection failed, will retry
    Disconnected,
    /// In retry process
    Retrying,
    /// Connection closed normally
    Closed,
    /// Fatal error, won't retry
    Failed,
}

impl ConnectionState {
    /// Check if state represents an active connection
    pub fn is_connected(&self) -> bool {
        matches!(self, ConnectionState::Connected)
    }

    /// Convert to u8 for atomic storage
    pub const fn as_u8(self) -> u8 {
        match self {
            ConnectionState::Uninitialized => 0,
            ConnectionState::Initializing => 1,
            ConnectionState::Connecting => 2,
            ConnectionState::Connected => 3,
            ConnectionState::Disconnected => 4,
            ConnectionState::Retrying => 5,
            ConnectionState::Closed => 6,
            ConnectionState::Failed => 7,
        }
    }

    /// Convert from u8
    pub const fn from_u8(v: u8) -> Self {
        match v {
            0 => ConnectionState::Uninitialized,
            1 => ConnectionState::Initializing,
            2 => ConnectionState::Connecting,
            3 => ConnectionState::Connected,
            4 => ConnectionState::Disconnected,
            5 => ConnectionState::Retrying,
            6 => ConnectionState::Closed,
            _ => ConnectionState::Failed, // 7 or any invalid value
        }
    }
}

impl From<crate::protocols::core::traits::ConnectionState> for ConnectionState {
    fn from(protocol_state: crate::protocols::core::traits::ConnectionState) -> Self {
        use crate::protocols::core::traits::ConnectionState as P;
        match protocol_state {
            P::Connected => Self::Connected,
            P::Connecting => Self::Connecting,
            P::Reconnecting => Self::Retrying,
            P::Disconnected => Self::Disconnected,
            P::Error => Self::Failed,
        }
    }
}

impl std::fmt::Display for ConnectionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectionState::Uninitialized => write!(f, "UNINITIALIZED"),
            ConnectionState::Initializing => write!(f, "INITIALIZING"),
            ConnectionState::Connecting => write!(f, "CONNECTING"),
            ConnectionState::Connected => write!(f, "CONNECTED"),
            ConnectionState::Disconnected => write!(f, "DISCONNECTED"),
            ConnectionState::Retrying => write!(f, "RETRYING"),
            ConnectionState::Closed => write!(f, "CLOSED"),
            ConnectionState::Failed => write!(f, "FAILED"),
        }
    }
}

// ============================================================================
// Protocol Value Type
// ============================================================================

/// Value type for protocol data exchange
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ProtocolValue {
    String(Cow<'static, str>),
    Integer(i64),
    Float(f64),
    Bool(bool),
    Null,
}

impl From<f64> for ProtocolValue {
    fn from(v: f64) -> Self {
        Self::Float(v)
    }
}

impl From<i64> for ProtocolValue {
    fn from(v: i64) -> Self {
        Self::Integer(v)
    }
}

impl From<i32> for ProtocolValue {
    fn from(v: i32) -> Self {
        Self::Integer(v as i64)
    }
}

impl From<&str> for ProtocolValue {
    fn from(v: &str) -> Self {
        Self::String(Cow::Owned(v.to_string()))
    }
}

impl From<String> for ProtocolValue {
    fn from(v: String) -> Self {
        Self::String(Cow::Owned(v))
    }
}

impl From<bool> for ProtocolValue {
    fn from(v: bool) -> Self {
        Self::Bool(v)
    }
}

impl From<u16> for ProtocolValue {
    fn from(v: u16) -> Self {
        Self::Integer(v as i64)
    }
}

impl From<u32> for ProtocolValue {
    fn from(v: u32) -> Self {
        Self::Integer(v as i64)
    }
}

impl From<u8> for ProtocolValue {
    fn from(v: u8) -> Self {
        Self::Integer(v as i64)
    }
}

impl ProtocolValue {
    /// Try to convert to f64
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Float(f) => Some(*f),
            Self::Integer(i) => Some(*i as f64),
            Self::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            Self::String(s) => s.parse().ok(),
            Self::Null => None,
        }
    }

    /// Try to convert to i64
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Integer(i) => Some(*i),
            Self::Float(f) => Some(f.round() as i64),
            Self::Bool(b) => Some(if *b { 1 } else { 0 }),
            Self::String(s) => s.parse().ok(),
            Self::Null => None,
        }
    }

    /// Try to convert to bool
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            Self::Integer(i) => Some(*i != 0),
            Self::Float(f) => Some(*f != 0.0),
            Self::String(s) => match s.to_lowercase().as_str() {
                "true" | "1" | "yes" | "on" => Some(true),
                "false" | "0" | "no" | "off" => Some(false),
                _ => None,
            },
            Self::Null => None,
        }
    }

    /// Convert to String
    pub fn as_string(&self) -> String {
        match self {
            Self::String(s) => s.to_string(),
            Self::Integer(i) => i.to_string(),
            Self::Float(f) => f.to_string(),
            Self::Bool(b) => b.to_string(),
            Self::Null => String::new(),
        }
    }

    /// Check if value is null
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// Convert to u16 with bounds checking
    pub fn as_u16(&self) -> Option<u16> {
        self.as_i64().and_then(|i| {
            if i >= 0 && i <= u16::MAX as i64 {
                Some(i as u16)
            } else {
                None
            }
        })
    }
}

// ============================================================================
// Channel Types
// ============================================================================

/// Channel status
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct ChannelStatus {
    pub is_connected: bool,
    pub last_update: i64,
}

/// Point data structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PointData {
    pub value: ProtocolValue,
    pub timestamp: i64,
}

/// Point data mapping
pub type PointDataMap = HashMap<u32, PointData>;

/// Channel command enumeration
#[derive(Debug, Clone)]
pub enum ChannelCommand {
    /// Control command (YK)
    Control {
        command_id: String,
        point_id: u32,
        value: f64,
        timestamp: i64,
        expires_at_ms: i64,
    },
    /// Adjustment command (YT)
    Adjustment {
        command_id: String,
        point_id: u32,
        value: f64,
        timestamp: i64,
        expires_at_ms: i64,
    },
    /// Batch control command (multiple YK points in one send)
    BatchControl {
        command_id: String,
        points: Vec<(u32, f64)>,
        timestamp: i64,
        expires_at_ms: i64,
    },
    /// Batch adjustment command (multiple YT points in one send)
    BatchAdjustment {
        command_id: String,
        points: Vec<(u32, f64)>,
        timestamp: i64,
        expires_at_ms: i64,
    },
}

/// Batch telemetry data for channel transmission
#[derive(Debug, Clone)]
pub struct TelemetryBatch {
    pub channel_id: u32,
    pub telemetry: Vec<(u32, f64, i64)>, // (point_id, raw_value, timestamp)
    pub signal: Vec<(u32, f64, i64)>,    // (point_id, raw_value, timestamp)
}

/// Extended point data (for API and display)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtendedPointData {
    pub id: String,
    pub name: String,
    pub value: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub unit: String,
    pub description: String,
    pub telemetry_type: Option<FourRemote>,
    pub channel_id: Option<u32>,
}

/// Test channel parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestChannelParams {
    pub initial_value: f64,
    pub increment: f64,
    pub interval_ms: u64,
}

impl Default for TestChannelParams {
    fn default() -> Self {
        Self {
            initial_value: 0.0,
            increment: 1.0,
            interval_ms: 1000,
        }
    }
}

// ============================================================================
// Protocol Command (for lock-free polling task)
// ============================================================================

use crate::protocols::core::error::GatewayError;
use crate::protocols::core::traits::Diagnostics;
use tokio::sync::oneshot;

/// Commands sent to the unified channel task for protocol operations.
///
/// This enum enables lock-free communication with the polling task:
/// - External code sends commands via `mpsc::Sender<ProtocolCommand>`
/// - The polling task processes commands in its `select!` loop
/// - Results are returned via embedded `oneshot::Sender`
#[derive(Debug)]
pub enum ProtocolCommand {
    /// Connect to device
    Connect {
        /// Response channel
        response_tx: oneshot::Sender<Result<(), GatewayError>>,
    },

    /// Disconnect from device
    Disconnect {
        /// Response channel
        response_tx: oneshot::Sender<()>,
    },

    /// Get diagnostics information
    GetDiagnostics {
        /// Response channel
        response_tx: oneshot::Sender<Option<Diagnostics>>,
    },

    /// Get current connection state
    GetConnectionState {
        /// Response channel
        response_tx: oneshot::Sender<ConnectionState>,
    },

    /// Set channel log level dynamically
    SetLogLevel {
        /// Log level: "info" or "debug"
        level: String,
        /// Response channel
        response_tx: oneshot::Sender<Result<(), String>>,
    },

    /// Shutdown the channel task
    Shutdown,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::approx_constant)]
mod tests {
    use super::*;

    #[test]
    fn test_protocol_value_conversions() {
        let v = ProtocolValue::from(42i64);
        assert_eq!(v.as_i64(), Some(42));
        assert_eq!(v.as_f64(), Some(42.0));

        let v = ProtocolValue::from(3.1415f64);
        assert_eq!(v.as_f64(), Some(3.1415));
        assert_eq!(v.as_i64(), Some(3));

        let v = ProtocolValue::from(true);
        assert_eq!(v.as_bool(), Some(true));
        assert_eq!(v.as_i64(), Some(1));
    }

    #[test]
    fn test_connection_state() {
        assert!(ConnectionState::Connected.is_connected());
        assert!(!ConnectionState::Disconnected.is_connected());
    }

    #[test]
    fn test_connection_state_u8_roundtrip() {
        // Verify as_u8 → from_u8 roundtrip for all variants
        let states = [
            ConnectionState::Uninitialized,
            ConnectionState::Initializing,
            ConnectionState::Connecting,
            ConnectionState::Connected,
            ConnectionState::Disconnected,
            ConnectionState::Retrying,
            ConnectionState::Closed,
            ConnectionState::Failed,
        ];
        for state in states {
            let u8_val = state.as_u8();
            let roundtripped = ConnectionState::from_u8(u8_val);
            assert_eq!(
                state, roundtripped,
                "Roundtrip failed for {:?} (u8={})",
                state, u8_val
            );
        }
    }

    #[test]
    fn test_connection_state_from_u8_invalid() {
        // Invalid u8 values should map to Failed
        assert_eq!(ConnectionState::from_u8(8), ConnectionState::Failed);
        assert_eq!(ConnectionState::from_u8(255), ConnectionState::Failed);
    }

    #[test]
    fn test_connection_state_from_protocol_state() {
        use crate::protocols::core::traits::ConnectionState as P;

        // Verify the From<ProtocolConnectionState> mapping
        assert_eq!(
            ConnectionState::from(P::Connected),
            ConnectionState::Connected
        );
        assert_eq!(
            ConnectionState::from(P::Connecting),
            ConnectionState::Connecting
        );
        assert_eq!(
            ConnectionState::from(P::Reconnecting),
            ConnectionState::Retrying
        );
        assert_eq!(
            ConnectionState::from(P::Disconnected),
            ConnectionState::Disconnected
        );
        assert_eq!(ConnectionState::from(P::Error), ConnectionState::Failed);
    }

    #[test]
    fn test_connection_state_display() {
        assert_eq!(ConnectionState::Connected.to_string(), "CONNECTED");
        assert_eq!(ConnectionState::Disconnected.to_string(), "DISCONNECTED");
        assert_eq!(ConnectionState::Failed.to_string(), "FAILED");
        assert_eq!(ConnectionState::Retrying.to_string(), "RETRYING");
    }

    #[test]
    fn test_protocol_value_null() {
        let v = ProtocolValue::Null;
        assert!(v.is_null());
        assert_eq!(v.as_f64(), None);
        assert_eq!(v.as_i64(), None);
        assert_eq!(v.as_bool(), None);
        assert_eq!(v.as_string(), "");
    }

    #[test]
    fn test_protocol_value_string_conversions() {
        let v = ProtocolValue::from("true");
        assert_eq!(v.as_bool(), Some(true));

        let v = ProtocolValue::from("false");
        assert_eq!(v.as_bool(), Some(false));

        let v = ProtocolValue::from("42");
        assert_eq!(v.as_i64(), Some(42));
        assert_eq!(v.as_f64(), Some(42.0));

        let v = ProtocolValue::from("not_a_number");
        assert_eq!(v.as_i64(), None);
        assert_eq!(v.as_f64(), None);
        assert_eq!(v.as_bool(), None);
    }

    #[test]
    fn test_protocol_value_u16_u32_bounds() {
        let v = ProtocolValue::from(65535i64);
        assert_eq!(v.as_u16(), Some(65535));

        let v = ProtocolValue::from(65536i64);
        assert_eq!(v.as_u16(), None); // overflow

        let v = ProtocolValue::from(-1i64);
        assert_eq!(v.as_u16(), None); // negative
    }
}
