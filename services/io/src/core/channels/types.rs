//! Channel communication types
//!
//! Core data types for channel communication in io.
//! These types were previously in aether-comlink but are now owned by io.

use serde::{Deserialize, Serialize};

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
// Channel Types
// ============================================================================

/// Channel status
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct ChannelStatus {
    pub is_connected: bool,
    pub last_update: i64,
}

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
}
