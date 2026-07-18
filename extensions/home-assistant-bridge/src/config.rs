//! Validated Home Assistant connection configuration.

use std::time::Duration;

use aether_ports::{PortError, PortErrorKind, PortResult, SecretRef};
use url::Url;

const DEFAULT_MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;
const DEFAULT_MAX_COLLECTION_ITEMS: usize = 50_000;
const DEFAULT_ACTOR_QUEUE_CAPACITY: usize = 64;

/// Home Assistant WebSocket connection policy.
#[derive(Debug, Clone)]
pub struct HomeAssistantConnectionConfig {
    websocket_url: String,
    access_token_ref: SecretRef,
    request_timeout: Duration,
    max_message_bytes: usize,
    max_collection_items: usize,
    actor_queue_capacity: usize,
    reconnect_attempts: u8,
    reconnect_base_delay: Duration,
}

impl HomeAssistantConnectionConfig {
    /// Creates a connection from a Home Assistant origin and secret reference.
    ///
    /// The origin may use HTTP for a trusted local commissioning network or
    /// HTTPS for TLS. Credentials, query strings, fragments, and non-root paths
    /// are rejected.
    pub fn new(origin: impl AsRef<str>, access_token_ref: SecretRef) -> PortResult<Self> {
        let mut url =
            Url::parse(origin.as_ref()).map_err(|_| invalid_config("origin is invalid"))?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !matches!(url.path(), "" | "/")
        {
            return Err(invalid_config(
                "origin must be an HTTP(S) root without credentials, query, or fragment",
            ));
        }
        url.set_scheme(if url.scheme() == "https" { "wss" } else { "ws" })
            .map_err(|_| invalid_config("origin scheme cannot be converted to WebSocket"))?;
        url.set_path("/api/websocket");

        Ok(Self {
            websocket_url: url.into(),
            access_token_ref,
            request_timeout: Duration::from_secs(10),
            max_message_bytes: DEFAULT_MAX_MESSAGE_BYTES,
            max_collection_items: DEFAULT_MAX_COLLECTION_ITEMS,
            actor_queue_capacity: DEFAULT_ACTOR_QUEUE_CAPACITY,
            reconnect_attempts: 3,
            reconnect_base_delay: Duration::from_millis(250),
        })
    }

    /// Sets the per-connect, send, and response deadline.
    pub fn with_request_timeout(mut self, timeout: Duration) -> PortResult<Self> {
        if !(Duration::from_millis(100)..=Duration::from_secs(60)).contains(&timeout) {
            return Err(invalid_config(
                "request timeout must be between 100 milliseconds and 60 seconds",
            ));
        }
        self.request_timeout = timeout;
        Ok(self)
    }

    /// Returns the derived WebSocket endpoint.
    #[must_use]
    pub fn websocket_url(&self) -> &str {
        &self.websocket_url
    }

    /// Returns the non-secret access-token reference.
    #[must_use]
    pub const fn access_token_ref(&self) -> &SecretRef {
        &self.access_token_ref
    }

    pub(crate) const fn request_timeout(&self) -> Duration {
        self.request_timeout
    }

    pub(crate) const fn max_message_bytes(&self) -> usize {
        self.max_message_bytes
    }

    pub(crate) const fn max_collection_items(&self) -> usize {
        self.max_collection_items
    }

    pub(crate) const fn actor_queue_capacity(&self) -> usize {
        self.actor_queue_capacity
    }

    pub(crate) const fn reconnect_attempts(&self) -> u8 {
        self.reconnect_attempts
    }

    pub(crate) const fn reconnect_base_delay(&self) -> Duration {
        self.reconnect_base_delay
    }
}

fn invalid_config(message: &str) -> PortError {
    PortError::new(PortErrorKind::Permanent, message)
}
