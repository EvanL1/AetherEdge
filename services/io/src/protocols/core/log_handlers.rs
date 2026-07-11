//! Built-in implementations of `ChannelLogHandler`.

use async_trait::async_trait;
use std::sync::Arc;

use super::logging::{ChannelLogEvent, ChannelLogHandler};

/// Composite log handler that forwards events to multiple handlers.
pub struct CompositeLogHandler {
    handlers: Vec<Arc<dyn ChannelLogHandler>>,
}

impl CompositeLogHandler {
    pub fn new() -> Self {
        Self {
            handlers: Vec::new(),
        }
    }

    /// Add a handler (builder pattern).
    #[must_use]
    pub fn with_handler(mut self, handler: Arc<dyn ChannelLogHandler>) -> Self {
        self.handlers.push(handler);
        self
    }

    /// Add a handler (mutable).
    pub fn add_handler(&mut self, handler: Arc<dyn ChannelLogHandler>) {
        self.handlers.push(handler);
    }
}

impl Default for CompositeLogHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ChannelLogHandler for CompositeLogHandler {
    async fn on_log(&self, channel_id: u32, event: ChannelLogEvent) {
        let len = self.handlers.len();
        if len == 0 {
            return;
        }

        // Clone for first N-1 handlers, move for the last one
        for (i, handler) in self.handlers.iter().enumerate() {
            if i == len - 1 {
                handler.on_log(channel_id, event).await;
                return;
            } else {
                handler.on_log(channel_id, event.clone()).await;
            }
        }
    }

    fn set_log_level(&self, level: &str) {
        for handler in &self.handlers {
            handler.set_log_level(level);
        }
    }
}

/// Tracing log handler that integrates with the `tracing` crate.
pub struct TracingLogHandler;

#[async_trait]
impl ChannelLogHandler for TracingLogHandler {
    async fn on_log(&self, channel_id: u32, event: ChannelLogEvent) {
        use tracing::{debug, error, info, trace, warn};

        match &event {
            ChannelLogEvent::Connected {
                endpoint,
                duration_ms,
                ..
            } => {
                info!(
                    channel_id = channel_id,
                    endpoint = %endpoint,
                    duration_ms = duration_ms,
                    "Channel connected"
                );
            },
            ChannelLogEvent::Disconnected { reason, .. } => {
                if let Some(reason) = reason {
                    warn!(
                        channel_id = channel_id,
                        reason = %reason,
                        "Channel disconnected"
                    );
                } else {
                    info!(channel_id = channel_id, "Channel disconnected");
                }
            },
            ChannelLogEvent::Error { error, context, .. } => {
                error!(
                    channel_id = channel_id,
                    error = %error,
                    context = %context,
                    "Channel error"
                );
            },
            ChannelLogEvent::ControlWrite {
                commands,
                result,
                duration_ms,
                ..
            } => match result {
                Ok(write_result) => {
                    debug!(
                        channel_id = channel_id,
                        commands_count = commands.len(),
                        success_count = write_result.success_count,
                        duration_ms = duration_ms,
                        "Control write completed"
                    );
                },
                Err(e) => {
                    warn!(
                        channel_id = channel_id,
                        commands_count = commands.len(),
                        error = %e,
                        "Control write failed"
                    );
                },
            },
            ChannelLogEvent::PollCycleCompleted {
                points_count,
                duration_ms,
                success_count,
                failed_count,
                ..
            } => {
                trace!(
                    channel_id = channel_id,
                    points_count = points_count,
                    success_count = success_count,
                    failed_count = failed_count,
                    duration_ms = duration_ms,
                    "Poll cycle completed"
                );
            },
            ChannelLogEvent::RawPacket {
                direction,
                data,
                metadata,
                ..
            } => {
                let hex = data
                    .iter()
                    .take(32)
                    .fold(String::with_capacity(64), |mut s, b| {
                        use std::fmt::Write;
                        let _ = write!(s, "{:02X}", b);
                        s
                    });
                trace!(
                    channel_id = channel_id,
                    protocol = metadata.protocol_name(),
                    direction = ?direction,
                    size = data.len(),
                    data = %hex,
                    "Raw packet"
                );
            },
            ChannelLogEvent::StateChanged {
                old_state,
                new_state,
                ..
            } => {
                info!(
                    channel_id = channel_id,
                    old_state = %old_state,
                    new_state = %new_state,
                    "Connection state changed"
                );
            },
            ChannelLogEvent::PointValues {
                total_points,
                group_id,
                ..
            } => {
                trace!(
                    channel_id = channel_id,
                    total_points = total_points,
                    group_id = ?group_id,
                    "Point values collected"
                );
            },
            _ => {
                debug!(channel_id = channel_id, event = ?event, "Channel event");
            },
        }
    }
}
