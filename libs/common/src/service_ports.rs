//! Stable default ports for the six-process AetherEdge runtime.
//!
//! These are deployment/configuration identifiers, not business-domain types.
//! Runtime configuration may override them, but tooling and compatibility
//! loaders share these defaults rather than depending on the retired model
//! compatibility crate.

/// Default port for `aether-io`.
pub const IO_PORT: u16 = 6001;
/// Default port for `aether-automation`.
pub const AUTOMATION_PORT: u16 = 6002;
/// Default port for `aether-history`.
pub const HISTORY_PORT: u16 = 6004;
/// Default port for the authenticated `aether-api` gateway.
pub const API_PORT: u16 = 6005;
/// Default port for `aether-uplink`.
pub const UPLINK_PORT: u16 = 6006;
/// Default port for `aether-alarm`.
pub const ALARM_PORT: u16 = 6007;

/// Returns the default port for a canonical service name.
#[must_use]
pub const fn default_port_for(service: &str) -> Option<u16> {
    match service.as_bytes() {
        b"aether-io" => Some(IO_PORT),
        b"aether-automation" => Some(AUTOMATION_PORT),
        b"aether-history" => Some(HISTORY_PORT),
        b"aether-api" => Some(API_PORT),
        b"aether-uplink" => Some(UPLINK_PORT),
        b"aether-alarm" => Some(ALARM_PORT),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_services_have_stable_distinct_ports() {
        let ports = [
            default_port_for("aether-io"),
            default_port_for("aether-automation"),
            default_port_for("aether-history"),
            default_port_for("aether-api"),
            default_port_for("aether-uplink"),
            default_port_for("aether-alarm"),
        ];

        assert_eq!(
            ports,
            [
                Some(6001),
                Some(6002),
                Some(6004),
                Some(6005),
                Some(6006),
                Some(6007)
            ]
        );
        assert_eq!(default_port_for("unknown"), None);
    }
}
