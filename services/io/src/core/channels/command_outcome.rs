//! Bounded in-process command lifecycle observation.
//!
//! `CommandReceipt` remains the producer-side local-acceptance acknowledgement.
//! This registry records what happens after that boundary under the existing
//! durable `CommandId`, without retrying or changing delivery semantics.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

const DEFAULT_OUTCOME_CAPACITY: usize = 4_096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandDropReason {
    /// The bounded channel queue remained full through its enqueue deadline.
    Backpressure,
    /// The target runtime's command receiver was already closed.
    ChannelClosed,
    /// No runtime was registered for the target channel.
    ChannelUnavailable,
    /// The frame or command value failed validation before queueing.
    InvalidCommand,
    /// Service quiescence discarded a command that had not reached a device.
    ServiceStopping,
}

/// Latest observed state after producer-side local acceptance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandLifecycleState {
    /// Accepted into the target channel's bounded in-memory queue.
    Queued,
    /// Discarded before device execution.
    Dropped(CommandDropReason),
    /// Expired before device execution.
    Expired,
    /// Device adapter reported successful writes.
    DeviceSucceeded { writes: usize },
    /// Device execution failed, was rejected, or was cancelled.
    DeviceFailed { diagnostic: String },
}

/// Latest bounded in-process observation for one command identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutcome {
    /// Latest lifecycle state.
    pub state: CommandLifecycleState,
    /// Observation time in milliseconds since UNIX epoch.
    pub updated_at_ms: u64,
}

/// Process-lifetime command lifecycle counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CommandOutcomeStats {
    /// Number of successful queue admissions.
    pub queued: u64,
    /// Number of pre-device drops.
    pub dropped: u64,
    /// Number of expirations observed before dispatch.
    pub expired: u64,
    /// Number of successful device outcomes.
    pub device_succeeded: u64,
    /// Number of failed, rejected, or cancelled device outcomes.
    pub device_failed: u64,
    /// Number of recent command IDs retained in the bounded registry.
    pub tracked: usize,
}

struct RecentOutcomes {
    by_id: HashMap<String, CommandOutcome>,
    insertion_order: VecDeque<String>,
}

pub(crate) struct CommandOutcomeTracker {
    recent: Mutex<RecentOutcomes>,
    capacity: usize,
    queued: AtomicU64,
    dropped: AtomicU64,
    expired: AtomicU64,
    device_succeeded: AtomicU64,
    device_failed: AtomicU64,
}

impl Default for CommandOutcomeTracker {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_OUTCOME_CAPACITY)
    }
}

impl CommandOutcomeTracker {
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            recent: Mutex::new(RecentOutcomes {
                by_id: HashMap::with_capacity(capacity.max(1)),
                insertion_order: VecDeque::with_capacity(capacity.max(1)),
            }),
            capacity: capacity.max(1),
            queued: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            expired: AtomicU64::new(0),
            device_succeeded: AtomicU64::new(0),
            device_failed: AtomicU64::new(0),
        }
    }

    pub(crate) fn record(&self, command_id: &str, state: CommandLifecycleState) {
        match &state {
            CommandLifecycleState::Queued => {
                self.queued.fetch_add(1, Ordering::Relaxed);
            },
            CommandLifecycleState::Dropped(_) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            },
            CommandLifecycleState::Expired => {
                self.expired.fetch_add(1, Ordering::Relaxed);
            },
            CommandLifecycleState::DeviceSucceeded { .. } => {
                self.device_succeeded.fetch_add(1, Ordering::Relaxed);
            },
            CommandLifecycleState::DeviceFailed { .. } => {
                self.device_failed.fetch_add(1, Ordering::Relaxed);
            },
        }

        let updated_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u64::MAX as u128) as u64;
        let mut recent = match self.recent.lock() {
            Ok(recent) => recent,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(outcome) = recent.by_id.get_mut(command_id) {
            // Queue admission is recorded after `send` succeeds. A fast
            // channel task may publish its terminal outcome before this
            // listener continuation runs; never regress that terminal state
            // back to Queued. The queued counter above still correctly records
            // the successful bounded-channel admission.
            if matches!(state, CommandLifecycleState::Queued)
                && !matches!(outcome.state, CommandLifecycleState::Queued)
            {
                return;
            }
            *outcome = CommandOutcome {
                state,
                updated_at_ms,
            };
            return;
        }
        if recent.by_id.len() == self.capacity
            && let Some(evicted) = recent.insertion_order.pop_front()
        {
            recent.by_id.remove(&evicted);
        }
        recent.insertion_order.push_back(command_id.to_owned());
        recent.by_id.insert(
            command_id.to_owned(),
            CommandOutcome {
                state,
                updated_at_ms,
            },
        );
    }

    pub(crate) fn outcome(&self, command_id: &str) -> Option<CommandOutcome> {
        let recent = match self.recent.lock() {
            Ok(recent) => recent,
            Err(poisoned) => poisoned.into_inner(),
        };
        recent.by_id.get(command_id).cloned()
    }

    pub(crate) fn stats(&self) -> CommandOutcomeStats {
        CommandOutcomeStats {
            queued: self.queued.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
            expired: self.expired.load(Ordering::Relaxed),
            device_succeeded: self.device_succeeded.load(Ordering::Relaxed),
            device_failed: self.device_failed.load(Ordering::Relaxed),
            tracked: match self.recent.lock() {
                Ok(recent) => recent.by_id.len(),
                Err(poisoned) => poisoned.into_inner().by_id.len(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_bounded_and_retains_latest_state() {
        let tracker = CommandOutcomeTracker::with_capacity(2);
        tracker.record("one", CommandLifecycleState::Queued);
        tracker.record("one", CommandLifecycleState::DeviceSucceeded { writes: 1 });
        tracker.record("two", CommandLifecycleState::Expired);
        tracker.record(
            "three",
            CommandLifecycleState::Dropped(CommandDropReason::Backpressure),
        );

        assert!(tracker.outcome("one").is_none());
        assert_eq!(
            tracker.outcome("two").map(|outcome| outcome.state),
            Some(CommandLifecycleState::Expired)
        );
        assert_eq!(tracker.stats().tracked, 2);
        assert_eq!(tracker.stats().queued, 1);
        assert_eq!(tracker.stats().device_succeeded, 1);
    }

    #[test]
    fn late_queue_observation_cannot_overwrite_terminal_outcome() {
        let tracker = CommandOutcomeTracker::with_capacity(2);
        tracker.record("fast", CommandLifecycleState::DeviceSucceeded { writes: 1 });
        tracker.record("fast", CommandLifecycleState::Queued);

        assert_eq!(
            tracker.outcome("fast").map(|outcome| outcome.state),
            Some(CommandLifecycleState::DeviceSucceeded { writes: 1 })
        );
        assert_eq!(tracker.stats().queued, 1);
        assert_eq!(tracker.stats().device_succeeded, 1);
    }
}
