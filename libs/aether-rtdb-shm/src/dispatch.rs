//! Legacy SHM/UDS action-dispatch compatibility surface.
//!
//! Production automation uses `DeviceCommandSink` from `aether-ports` and the
//! typed implementation in `aether-shm-bridge`. These raw routing-context
//! types remain only for legacy tests/benchmarks. The architecture guard
//! forbids production callers; delete this module once those fixtures migrate.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use aether_routing::RouteContext;
use async_trait::async_trait;
use tracing::warn;

use crate::{ActionWriter, ShmNotifier, shared_config::SharedConfig};

/// Outcome of an action dispatch operation.
///
/// Each variant represents a mutually exclusive dispatch result.
/// Using an enum instead of multiple bools prevents invalid state combinations.
#[derive(Debug)]
#[must_use]
pub enum DispatchOutcome {
    /// SHM written + UDS notification sent (happy path, ~1-2ms).
    Delivered,
    /// SHM written but UDS notification failed or skipped.
    /// io delivery is not guaranteed without UDS.
    ShmOnly { reason: &'static str },
    /// Dispatch failed — no SHM writer configured.
    NoWriter,
    /// Dispatch failed — route resolved, but the target slot was absent from SHM.
    SlotMissing { reason: &'static str },
    /// No-op dispatch (test environment, intentionally skips all transport).
    Noop,
}

impl DispatchOutcome {
    /// Returns true if the dispatch reached io (Delivered or Noop).
    /// Callers use this to gate command success and cooldown counting.
    #[inline]
    pub fn is_delivered(&self) -> bool {
        matches!(self, DispatchOutcome::Delivered | DispatchOutcome::Noop)
    }
}

/// Trait for dispatching action commands to io.
///
/// The primary implementation uses SHM + UDS for ~1-2ms latency.
/// Test implementations use NoopDispatch (no-op, skips all transport).
#[async_trait]
pub trait ActionDispatch: Send + Sync {
    /// Dispatch an action value to the target channel via the fastest available path.
    async fn dispatch(&self, ctx: &RouteContext, value: f64) -> DispatchOutcome;
}

/// SHM + UDS dispatch implementation (production path).
///
/// Writes action values directly to shared memory, then sends a UDS notification
/// to io for immediate processing. Degrades gracefully if UDS notification fails
/// (SHM value is written but io delivery is not guaranteed without UDS).
pub struct ShmDispatch {
    writer: arc_swap::ArcSwapOption<ActionWriter>,
    config: std::sync::OnceLock<SharedConfig>,
    notifier: std::sync::OnceLock<Arc<tokio::sync::Mutex<ShmNotifier>>>,
    expected_generation: AtomicU64,
    /// Signal to trigger background SHM writer rebuild after generation mismatch.
    rebuild_trigger: Arc<tokio::sync::Notify>,
}

impl Default for ShmDispatch {
    fn default() -> Self {
        Self::new()
    }
}

impl ShmDispatch {
    /// Create a new ShmDispatch (initially unconfigured).
    ///
    /// Call `set_writer()` and `set_notifier()` after construction
    /// to enable SHM and UDS paths respectively.
    pub fn new() -> Self {
        Self {
            writer: arc_swap::ArcSwapOption::empty(),
            config: std::sync::OnceLock::new(),
            notifier: std::sync::OnceLock::new(),
            expected_generation: AtomicU64::new(0),
            rebuild_trigger: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// Configure SHM action writer for M2C via shared memory.
    pub fn set_writer(&self, writer: Arc<ActionWriter>, config: SharedConfig) {
        // Capture generation so dispatch() can detect io restarts.
        self.expected_generation
            .store(writer.generation(), Ordering::Release);
        self.writer.store(Some(writer));
        let _ = self.config.set(config);
    }

    /// Configure UDS notifier for event-driven M2C command dispatch.
    /// Returns true if set successfully, false if already set.
    pub fn set_notifier(&self, notifier: Arc<tokio::sync::Mutex<ShmNotifier>>) -> bool {
        self.notifier.set(notifier).is_ok()
    }

    /// Returns true if the SHM writer is currently configured and available.
    pub fn is_writer_available(&self) -> bool {
        self.writer.load().is_some()
    }

    /// Returns true if the UDS notifier has been configured.
    pub fn is_notifier_configured(&self) -> bool {
        self.notifier.get().is_some()
    }

    /// Get the rebuild trigger for spawning a background rebuild task.
    pub fn rebuild_trigger(&self) -> Arc<tokio::sync::Notify> {
        Arc::clone(&self.rebuild_trigger)
    }
}

#[async_trait]
impl ActionDispatch for ShmDispatch {
    async fn dispatch(&self, ctx: &RouteContext, value: f64) -> DispatchOutcome {
        // Step 1: Write action value to shared memory (zero-copy IPC).
        let writer_guard = self.writer.load();
        let Some(writer) = writer_guard.as_ref() else {
            return DispatchOutcome::NoWriter;
        };

        // Generation pre-check: detect io restarts that changed the SHM layout.
        let current_gen = writer.generation();
        let expected = self.expected_generation.load(Ordering::Acquire);
        if expected != 0 && current_gen != expected {
            warn!(
                "SHM writer generation mismatch (expected={}, actual={}). \
                 io may have restarted. Clearing writer and triggering rebuild.",
                expected, current_gen
            );
            self.writer.store(None);
            self.rebuild_trigger.notify_one();
            return DispatchOutcome::NoWriter;
        }

        let mirrored = writer.set_action(
            ctx.target_channel_id,
            ctx.target_point_type,
            ctx.target_point_id,
            value,
            ctx.timestamp_ms as u64,
        );
        if !mirrored {
            warn!(
                "SHM action mirror miss for ch={} pt={} point={}",
                ctx.target_channel_id, ctx.target_point_type, ctx.target_point_id
            );
            return DispatchOutcome::SlotMissing {
                reason: "SHM action slot missing",
            };
        }

        // Post-write generation re-check: close the TOCTOU window.
        let post_gen = writer.generation();
        if expected != 0 && post_gen != expected {
            warn!(
                "SHM generation changed mid-dispatch (expected={}, post={}); discarding write",
                expected, post_gen
            );
            self.writer.store(None);
            self.rebuild_trigger.notify_one();
            return DispatchOutcome::SlotMissing {
                reason: "generation changed mid-dispatch",
            };
        }

        // Step 2: UDS notification for event-driven dispatch (~1-2ms latency).
        let Some(notifier_lock) = self.notifier.get() else {
            return DispatchOutcome::ShmOnly {
                reason: "notifier not configured",
            };
        };

        let mut guard = match tokio::time::timeout(Duration::from_millis(100), notifier_lock.lock())
            .await
        {
            Ok(guard) => guard,
            Err(_) => {
                warn!("ShmNotifier lock timeout; SHM value written but UDS notification skipped");
                return DispatchOutcome::ShmOnly {
                    reason: "notifier lock timeout",
                };
            },
        };

        let pt = match aether_model::PointType::from_u8(ctx.target_point_type) {
            Some(pt) => pt,
            None => {
                warn!(
                    "Ch{} point_type {} invalid, UDS notification skipped",
                    ctx.target_channel_id, ctx.target_point_type
                );
                return DispatchOutcome::ShmOnly {
                    reason: "invalid point_type",
                };
            },
        };

        let result = guard
            .notify(
                ctx.target_channel_id,
                pt,
                ctx.target_point_id,
                value,
                ctx.timestamp_ms as u64,
                ctx.expires_at_ms as u64,
            )
            .await;

        if result.uds_sent {
            DispatchOutcome::Delivered
        } else {
            if result.fallback_used {
                warn!(
                    "UDS notify degraded for ch={} pt={:?} point={}",
                    ctx.target_channel_id, pt, ctx.target_point_id
                );
            }
            DispatchOutcome::ShmOnly {
                reason: if result.fallback_used {
                    "UDS degraded"
                } else {
                    "UDS disabled"
                },
            }
        }
    }
}

/// No-op dispatch for tests and environments without SHM.
pub struct NoopDispatch;

#[async_trait]
impl ActionDispatch for NoopDispatch {
    async fn dispatch(&self, _ctx: &RouteContext, _value: f64) -> DispatchOutcome {
        DispatchOutcome::Noop
    }
}
