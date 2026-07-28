//! Versioned CloudLink MQTT topic namespace and exact route mapping.

use aether_ports::CloudLinkTransportRoute;

use crate::CloudLinkMqttError;

/// Validated per-gateway candidate MQTT namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicNamespace {
    root: String,
}

impl TopicNamespace {
    /// Creates `{prefix}/v1/gateways/{gatewayId}` after validating every segment.
    pub fn new(prefix: &str, gateway_id: &str) -> Result<Self, CloudLinkMqttError> {
        if prefix.is_empty()
            || prefix.len() > 256
            || prefix.split('/').any(|segment| !valid_segment(segment))
            || !valid_uuid(gateway_id)
        {
            return Err(CloudLinkMqttError::InvalidConfiguration(
                "CloudLink topic prefix is unsafe or Gateway ID is not a canonical lowercase UUID",
            ));
        }
        let root = format!("{prefix}/v1/gateways/{gateway_id}");
        if root.len() > 512 {
            return Err(CloudLinkMqttError::InvalidConfiguration(
                "CloudLink MQTT topic namespace exceeds 512 bytes",
            ));
        }
        Ok(Self { root })
    }

    /// Returns the exact topic for one logical route.
    #[must_use]
    pub fn topic(&self, route: CloudLinkTransportRoute) -> String {
        let suffix = match route {
            CloudLinkTransportRoute::SessionUp => "up/session",
            CloudLinkTransportRoute::SessionDown => "down/session",
            CloudLinkTransportRoute::HeartbeatUp => "up/heartbeat",
            CloudLinkTransportRoute::ManifestUp => "up/manifest",
            CloudLinkTransportRoute::TelemetryUp => "up/telemetry",
            CloudLinkTransportRoute::DataLossUp => "up/data-loss",
            CloudLinkTransportRoute::AckDown => "down/ack",
            CloudLinkTransportRoute::ReplayDown => "down/replay",
        };
        format!("{}/{suffix}", self.root)
    }

    /// Returns the five allowed edge publish topics.
    #[must_use]
    pub fn publish_topics(&self) -> Vec<String> {
        [
            CloudLinkTransportRoute::SessionUp,
            CloudLinkTransportRoute::HeartbeatUp,
            CloudLinkTransportRoute::ManifestUp,
            CloudLinkTransportRoute::TelemetryUp,
            CloudLinkTransportRoute::DataLossUp,
        ]
        .map(|route| self.topic(route))
        .to_vec()
    }

    /// Returns the three exact allowed edge subscriptions.
    #[must_use]
    pub fn subscribe_topics(&self) -> Vec<String> {
        [
            CloudLinkTransportRoute::SessionDown,
            CloudLinkTransportRoute::AckDown,
            CloudLinkTransportRoute::ReplayDown,
        ]
        .map(|route| self.topic(route))
        .to_vec()
    }

    /// Maps only exact same-gateway downlink topics.
    #[must_use]
    pub fn inbound_route(&self, topic: &str) -> Option<CloudLinkTransportRoute> {
        [
            CloudLinkTransportRoute::SessionDown,
            CloudLinkTransportRoute::AckDown,
            CloudLinkTransportRoute::ReplayDown,
        ]
        .into_iter()
        .find(|route| self.topic(*route) == topic)
    }
}

fn valid_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment.len() <= 128
        && !segment.contains(['+', '#', '\0'])
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

fn valid_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes[8] == b'-'
        && bytes[13] == b'-'
        && bytes[18] == b'-'
        && bytes[23] == b'-'
        && matches!(bytes[14], b'1'..=b'8')
        && matches!(bytes[19], b'8' | b'9' | b'a' | b'b')
        && bytes.iter().enumerate().all(|(index, byte)| {
            matches!(index, 8 | 13 | 18 | 23)
                || (byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        })
}
