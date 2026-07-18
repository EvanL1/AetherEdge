//! Explicit default-off MQTT routes for governed Integration control.

use crate::{CloudLinkMqttError, TopicNamespace};

/// Exact offer and receipt topics activated separately from the read-only baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationControlTopicNamespace {
    root: String,
}

impl IntegrationControlTopicNamespace {
    /// Validates the existing namespace rules without adding baseline subscriptions.
    pub fn new(prefix: &str, gateway_id: &str) -> Result<Self, CloudLinkMqttError> {
        let _validated_baseline = TopicNamespace::new(prefix, gateway_id)?;
        Ok(Self {
            root: format!("{prefix}/v1/gateways/{gateway_id}"),
        })
    }

    /// Returns the only governed control downlink topic.
    #[must_use]
    pub fn offer_topic(&self) -> String {
        format!("{}/down/integration-control", self.root)
    }

    /// Returns the only governed control receipt uplink topic.
    #[must_use]
    pub fn receipt_topic(&self) -> String {
        format!("{}/up/integration-control/receipts", self.root)
    }
}
