//! Broker-neutral MQTT v3.1.1 binding for experimental CloudLink.
//!
//! MQTT supplies TCP/TLS, broker authentication, ACL enforcement, QoS 1, and
//! reconnect. It does not supply CloudLink application acknowledgement.

mod config;
mod topics;
mod transport;

pub use config::{
    CloudLinkMigrationMode, CloudLinkMqttConfig, CloudLinkMqttError, CloudLinkTlsConfig,
    DeploymentSecurity, MqttClientIdentity, SecretString,
};
pub use topics::TopicNamespace;
pub use transport::MqttCloudLinkTransport;

/// MQTT QoS required by the candidate binding.
pub const CLOUDLINK_MQTT_QOS: rumqttc::QoS = rumqttc::QoS::AtLeastOnce;

/// Business/session messages are never retained.
pub const CLOUDLINK_MQTT_RETAIN: bool = false;
