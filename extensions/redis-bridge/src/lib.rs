//! Optional Redis bridge for non-authoritative state mirroring.

use std::fmt;

use aether_domain::{PointAddress, PointKind, PointQuality, PointSample};
use aether_ports::{PortError, PortErrorKind, PortResult, StateMirror};
use async_trait::async_trait;
use redis::aio::ConnectionManager;
use serde_json::{Value, json};

/// Namespaced Redis key mapping for the external live-state view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedisKeyspace {
    prefix: String,
}

impl RedisKeyspace {
    /// Creates a keyspace, trimming trailing separators.
    #[must_use]
    pub fn new(prefix: impl Into<String>) -> Self {
        let prefix = prefix.into();
        let prefix = prefix.trim_end_matches(':');
        Self {
            prefix: if prefix.is_empty() {
                "aether".to_string()
            } else {
                prefix.to_string()
            },
        }
    }

    /// Returns the Redis hash key for one instance and point kind.
    #[must_use]
    pub fn state_key(&self, address: PointAddress) -> String {
        format!(
            "{}:state:{}:{}",
            self.prefix,
            address.instance_id().get(),
            kind_name(address.kind())
        )
    }

    /// Returns the Redis hash field for one point.
    #[must_use]
    pub fn point_field(&self, address: PointAddress) -> String {
        address.point_id().get().to_string()
    }
}

/// Optional Redis implementation of [`StateMirror`].
pub struct RedisStateMirror {
    connection: ConnectionManager,
    keyspace: RedisKeyspace,
}

impl fmt::Debug for RedisStateMirror {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RedisStateMirror")
            .field("keyspace", &self.keyspace)
            .finish_non_exhaustive()
    }
}

impl RedisStateMirror {
    /// Connects an optional Redis mirror.
    pub async fn connect(url: &str, prefix: impl Into<String>) -> PortResult<Self> {
        let client = redis::Client::open(url).map_err(|error| {
            PortError::new(
                PortErrorKind::Permanent,
                format!("invalid Redis mirror URL: {error}"),
            )
        })?;
        let connection = ConnectionManager::new(client).await.map_err(redis_error)?;
        Ok(Self::from_connection_manager(connection, prefix))
    }

    /// Creates a mirror from a host-managed Redis connection.
    #[must_use]
    pub fn from_connection_manager(
        connection: ConnectionManager,
        prefix: impl Into<String>,
    ) -> Self {
        Self {
            connection,
            keyspace: RedisKeyspace::new(prefix),
        }
    }

    /// Returns the configured keyspace.
    #[must_use]
    pub const fn keyspace(&self) -> &RedisKeyspace {
        &self.keyspace
    }
}

#[async_trait]
impl StateMirror for RedisStateMirror {
    async fn mirror(&self, samples: &[PointSample]) -> PortResult<usize> {
        if samples.is_empty() {
            return Ok(0);
        }

        let mut pipeline = redis::pipe();
        for sample in samples {
            let mut command = redis::cmd("HSET");
            command
                .arg(self.keyspace.state_key(sample.address()))
                .arg(self.keyspace.point_field(sample.address()))
                .arg(encode_sample(*sample)?);
            pipeline.add_command(command);
        }

        let mut connection = self.connection.clone();
        pipeline
            .query_async::<()>(&mut connection)
            .await
            .map_err(redis_error)?;
        Ok(samples.len())
    }
}

/// Encodes the stable mirror value without exposing Redis vocabulary to core.
pub fn encode_sample(sample: PointSample) -> PortResult<String> {
    let value = serde_json::Number::from_f64(sample.value())
        .map(Value::Number)
        .unwrap_or(Value::Null);
    serde_json::to_string(&json!({
        "value": value,
        "timestamp_ms": sample.timestamp().get(),
        "quality": quality_name(sample.quality()),
    }))
    .map_err(|error| {
        PortError::new(
            PortErrorKind::InvalidData,
            format!("cannot encode mirrored sample: {error}"),
        )
    })
}

const fn kind_name(kind: PointKind) -> &'static str {
    match kind {
        PointKind::Telemetry => "telemetry",
        PointKind::Status => "status",
        PointKind::Command => "command",
        PointKind::Action => "action",
    }
}

const fn quality_name(quality: PointQuality) -> &'static str {
    match quality {
        PointQuality::Good => "good",
        PointQuality::Uncertain => "uncertain",
        PointQuality::Bad => "bad",
        PointQuality::Unavailable => "unavailable",
    }
}

fn redis_error(error: redis::RedisError) -> PortError {
    PortError::new(
        PortErrorKind::Unavailable,
        format!("Redis mirror unavailable: {error}"),
    )
}
