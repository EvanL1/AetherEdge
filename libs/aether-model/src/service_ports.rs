//! Service Port Constants
//!
//! Single source of truth for legacy AetherEdge service default ports.
//! These are used as fallback defaults when not overridden by configuration.

/// Default port for aether-io.
pub const IO_PORT: u16 = 6001;

/// Default port for aether-automation.
pub const AUTOMATION_PORT: u16 = 6002;

/// Default port for aether-history.
pub const HISTORY_PORT: u16 = 6004;

/// Default port for aether-api.
pub const API_PORT: u16 = 6005;

/// Default port for aether-alarm.
pub const ALARM_PORT: u16 = 6007;

/// Default port for aether-uplink.
pub const UPLINK_PORT: u16 = 6006;

/// Reserved compatibility port for downstream HTTP applications.
pub const APPS_PORT: u16 = 8080;

/// Default Redis port
pub const REDIS_PORT: u16 = 6379;

/// Get the default port for a service by name.
///
/// Returns `None` for unknown service names.
pub fn default_port_for(service: &str) -> Option<u16> {
    match service {
        "aether-io" => Some(IO_PORT),
        "aether-automation" => Some(AUTOMATION_PORT),
        "aether-history" => Some(HISTORY_PORT),
        "aether-api" => Some(API_PORT),
        "aether-alarm" => Some(ALARM_PORT),
        "aether-uplink" => Some(UPLINK_PORT),
        _ => None,
    }
}
