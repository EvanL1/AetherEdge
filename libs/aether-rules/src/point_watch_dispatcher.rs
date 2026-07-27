//! PointWatch dispatcher — automation side
//!
//! Maintains a `(channel_id, point_id) → Vec<rule_id>` subscription index
//! built from the loaded rules. When the service adapter supplies a
//! [`PointWatchHint`], the dispatcher looks up matching rules and forwards a
//! [`WatchEvent`] to the `RuleScheduler`'s event channel.
//!
//! ## Subscription index lifecycle
//!
//! 1. The host reloads rules, pins one complete service topology, and calls
//!    `PointWatchDispatcher::rebuild_from_rules` with that generation's typed
//!    measurement bindings and point manifest.
//! 2. `rebuild_from_rules` builds
//!    `HashMap<(channel_id, point_id_on_channel), Vec<rule_id>>` without
//!    consulting an independently mutable route cache.
//!    It returns canonical channel-point subscriptions for the service adapter
//!    to publish through its concrete event plane.
//! 3. Incoming `PointWatchHint`s are dispatched by `dispatch()`, which tries
//!    a fast non-blocking `try_send` to the scheduler's event channel.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::mpsc;
use tracing::{debug, warn};

use aether_domain::ChannelPointAddress;

use crate::scheduler::TriggerConfig;

/// One logical measurement binding from a pinned service topology.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeasurementRouteBinding {
    instance_id: u32,
    point_id: u32,
    target: ChannelPointAddress,
}

impl MeasurementRouteBinding {
    #[must_use]
    pub const fn new(instance_id: u32, point_id: u32, target: ChannelPointAddress) -> Self {
        Self {
            instance_id,
            point_id,
            target,
        }
    }

    #[must_use]
    pub const fn instance_id(self) -> u32 {
        self.instance_id
    }

    #[must_use]
    pub const fn point_id(self) -> u32 {
        self.point_id
    }

    #[must_use]
    pub const fn target(self) -> ChannelPointAddress {
        self.target
    }
}

/// Trait for accessing rule subscription data without exposing `ScheduledRule`.
///
/// `ScheduledRule` is private to `scheduler.rs`. This trait allows
/// `PointWatchDispatcher::rebuild_from_rules` to accept a slice of any type
/// that exposes the needed fields. `ScheduledRule` implements this trait via
/// `impl RuleSubscriptionInfo for ScheduledRule` in `scheduler.rs`.
pub trait RuleSubscriptionInfo {
    fn rule_id(&self) -> i64;
    fn is_enabled(&self) -> bool;
    fn trigger(&self) -> &TriggerConfig;
}

/// Bounded capacity of the event channel from dispatcher → scheduler.
const EVENT_CHANNEL_CAPACITY: usize = 1024;

/// A wake-up event forwarded to the `RuleScheduler`.
///
/// Carries routing context for candidate selection and diagnostics. Values are
/// best-effort hints only; the scheduler re-reads every referenced point from
/// the current SHM generation before deadband evaluation.
#[derive(Debug, Clone)]
pub struct WatchEvent {
    /// Rule IDs to consider executing (filtered from sub_index).
    pub rule_ids: Vec<i64>,
    /// Source channel (for cache-key reconstruction inside scheduler).
    pub channel_id: u32,
    /// Source point ID on that channel.
    pub point_id: u32,
    /// Engineering value at the time of emission.
    pub value: f64,
    /// Raw value.
    pub raw: f64,
    /// Millisecond timestamp.
    pub timestamp_ms: u64,
}

/// Transport-neutral point-change hint accepted by the rule dispatcher.
///
/// SHM/UDS adapters translate their wire event into this value at the service
/// composition boundary. The hinted value is best effort; rule evaluation
/// still re-reads the authoritative live-state source before acting.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PointWatchHint {
    channel_id: u32,
    point_id: u32,
    value: f64,
    raw: f64,
    timestamp_ms: u64,
}

impl PointWatchHint {
    /// Creates a transport-neutral point-change hint.
    #[must_use]
    pub const fn new(
        channel_id: u32,
        point_id: u32,
        value: f64,
        raw: f64,
        timestamp_ms: u64,
    ) -> Self {
        Self {
            channel_id,
            point_id,
            value,
            raw,
            timestamp_ms,
        }
    }
}

/// automation-side PointWatch dispatcher.
///
/// Holds a subscription index keyed by `(channel_id, point_id)` and forwards
/// incoming [`PointWatchHint`] values to the `RuleScheduler` via an mpsc channel.
pub struct PointWatchDispatcher {
    /// (channel_id, point_id) → Vec<rule_id>
    ///
    /// `point_id` here is the **channel-level** point ID (i.e. the source key
    /// from the canonical channel topology), NOT the instance-level point ID.
    /// `point_type` is intentionally absent from the key: T and S can both
    /// trigger `OnChange` rules; the per-rule deadband logic in
    /// `should_trigger_onchange` handles type disambiguation if needed.
    sub_index: HashMap<(u32, u32), Vec<i64>>,

    /// Channel for forwarding wake-up events to the scheduler.
    event_tx: mpsc::Sender<WatchEvent>,

    /// Dropped events counter (observable via health endpoint).
    dropped_count: Arc<AtomicU64>,
}

impl PointWatchDispatcher {
    /// Create a new dispatcher with a fresh (empty) subscription index.
    ///
    /// Returns the dispatcher and the corresponding `mpsc::Receiver` that
    /// `RuleScheduler` should drain.
    pub fn new() -> (Self, mpsc::Receiver<WatchEvent>) {
        let (tx, rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        let d = Self {
            sub_index: HashMap::new(),
            event_tx: tx,
            dropped_count: Arc::new(AtomicU64::new(0)),
        };
        (d, rx)
    }

    /// Rebuild the subscription index from the current rule set.
    ///
    /// Algorithm (O(rules × point_refs)):
    /// 1. Clear the bitmap.
    /// 2. For each enabled `OnChange` rule, iterate its `point_refs`.
    /// 3. Look up the logical measurement in the pinned typed bindings.
    /// 4. Insert `(channel_id, channel_point_id) → rule_id` into `sub_index`.
    /// 5. Return the canonical channel points that the service composition
    ///    must publish through its concrete event adapter.
    pub fn rebuild_from_rules(
        &mut self,
        rules: &[impl RuleSubscriptionInfo],
        measurement_routes: &[MeasurementRouteBinding],
    ) -> Vec<ChannelPointAddress> {
        self.sub_index.clear();

        let mut instance_to_channel: HashMap<(u32, u32), Vec<ChannelPointAddress>> = HashMap::new();
        let mut subscriptions = HashSet::new();
        for route in measurement_routes {
            instance_to_channel
                .entry((route.instance_id(), route.point_id()))
                .or_default()
                .push(route.target());
        }

        for rule in rules {
            if !rule.is_enabled() {
                continue;
            }
            let TriggerConfig::OnChange { point_refs, .. } = rule.trigger() else {
                continue;
            };

            for pref in point_refs {
                // Map PointKind to the stable T/S wire code.
                // Note: OnChange rules subscribe to Measurement (T) or Action (A) points.
                // For bitmap purposes we track the channel-side point type.
                let lookup_point_id = pref.point;

                // Look up which channel points feed this instance point
                if let Some(channel_entries) =
                    instance_to_channel.get(&(pref.instance, lookup_point_id))
                {
                    for &target in channel_entries {
                        let channel_id = target.channel_id().get();
                        let channel_point_id = target.point_id().get();

                        // Register in sub_index
                        self.sub_index
                            .entry((channel_id, channel_point_id))
                            .or_default()
                            .push(rule.rule_id());

                        subscriptions.insert(target);
                        debug!(
                            "PointWatch: selected ch={} pt={:?} pid={} for rule={}",
                            channel_id,
                            target.kind(),
                            channel_point_id,
                            rule.rule_id()
                        );
                    }
                }
            }
        }

        let sub_count = self.sub_index.len();
        let mut subscriptions = subscriptions.into_iter().collect::<Vec<_>>();
        subscriptions.sort_by_key(|address| {
            (
                address.channel_id().get(),
                u8::from(address.kind() == aether_domain::PointKind::Status),
                address.point_id().get(),
            )
        });
        tracing::info!(
            "PointWatchDispatcher: rebuilt index — {} (ch,pt) pairs, {} physical points selected",
            sub_count,
            subscriptions.len()
        );
        subscriptions
    }

    /// Dispatch an incoming transport-neutral point-change hint to the scheduler.
    ///
    /// Non-blocking: uses `try_send`. On overflow, increments `dropped_count`.
    pub fn dispatch(&self, hint: PointWatchHint) {
        let key = (hint.channel_id, hint.point_id);
        let Some(rule_ids) = self.sub_index.get(&key) else {
            return; // No rules subscribed to this point
        };

        let watch_event = WatchEvent {
            rule_ids: rule_ids.clone(),
            channel_id: hint.channel_id,
            point_id: hint.point_id,
            value: hint.value,
            raw: hint.raw,
            timestamp_ms: hint.timestamp_ms,
        };

        if self.event_tx.try_send(watch_event).is_err() {
            self.dropped_count.fetch_add(1, Ordering::Relaxed);
            warn!(
                "PointWatchDispatcher: event dropped (channel full) ch={} pt={}",
                hint.channel_id, hint.point_id
            );
        }
    }

    /// Number of events dropped due to channel backpressure.
    pub fn dropped_count(&self) -> u64 {
        self.dropped_count.load(Ordering::Relaxed)
    }

    /// Number of (channel_id, point_id) pairs in the subscription index.
    pub fn subscription_count(&self) -> usize {
        self.sub_index.len()
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;
    use crate::scheduler::{PointKind, PointRef, TriggerConfig};
    use aether_domain::{ChannelId, PointId, PointKind as PhysicalPointKind};

    /// Minimal rule for subscription tests
    struct TestRule {
        id: i64,
        enabled: bool,
        trigger: TriggerConfig,
    }

    impl RuleSubscriptionInfo for TestRule {
        fn rule_id(&self) -> i64 {
            self.id
        }
        fn is_enabled(&self) -> bool {
            self.enabled
        }
        fn trigger(&self) -> &TriggerConfig {
            &self.trigger
        }
    }

    fn channel_point(channel_id: u32, point_id: u32) -> ChannelPointAddress {
        ChannelPointAddress::new(
            ChannelId::new(channel_id),
            PhysicalPointKind::Telemetry,
            PointId::new(point_id),
        )
        .expect("acquisition address")
    }

    fn make_bindings() -> Vec<MeasurementRouteBinding> {
        vec![
            MeasurementRouteBinding::new(5, 10, channel_point(1001, 0)),
            MeasurementRouteBinding::new(5, 11, channel_point(1001, 1)),
        ]
    }

    #[test]
    fn rebuild_subscribes_matching_ch_pt_pair() {
        let (mut disp, _rx) = PointWatchDispatcher::new();
        let bindings = make_bindings();

        let rules = vec![TestRule {
            id: 7,
            enabled: true,
            trigger: TriggerConfig::OnChange {
                point_refs: vec![PointRef {
                    instance: 5,
                    point_type: PointKind::Measurement,
                    point: 10, // maps to ch=1001, channel_pt=0
                }],
                time_deadband_ms: None,
                value_deadband: None,
            },
        }];

        disp.rebuild_from_rules(&rules, &bindings);

        // sub_index should have one entry for (ch=1001, pt=0)
        assert_eq!(disp.subscription_count(), 1);
    }

    #[test]
    fn disabled_rule_not_subscribed() {
        let (mut disp, _rx) = PointWatchDispatcher::new();
        let bindings = make_bindings();

        let rules = vec![TestRule {
            id: 8,
            enabled: false,
            trigger: TriggerConfig::OnChange {
                point_refs: vec![PointRef {
                    instance: 5,
                    point_type: PointKind::Measurement,
                    point: 10,
                }],
                time_deadband_ms: None,
                value_deadband: None,
            },
        }];

        disp.rebuild_from_rules(&rules, &bindings);
        assert_eq!(disp.subscription_count(), 0);
    }

    #[test]
    fn interval_rule_not_subscribed() {
        let (mut disp, _rx) = PointWatchDispatcher::new();
        let bindings = make_bindings();

        let rules = vec![TestRule {
            id: 9,
            enabled: true,
            trigger: TriggerConfig::Interval { interval_ms: 1000 },
        }];

        disp.rebuild_from_rules(&rules, &bindings);
        assert_eq!(disp.subscription_count(), 0);
    }

    #[test]
    fn dispatch_sends_event_to_channel() {
        let (mut disp, mut rx) = PointWatchDispatcher::new();
        let bindings = make_bindings();

        let rules = vec![TestRule {
            id: 42,
            enabled: true,
            trigger: TriggerConfig::OnChange {
                point_refs: vec![PointRef {
                    instance: 5,
                    point_type: PointKind::Measurement,
                    point: 10, // maps to ch=1001, channel_pt=0
                }],
                time_deadband_ms: None,
                value_deadband: None,
            },
        }];

        disp.rebuild_from_rules(&rules, &bindings);

        let hint = PointWatchHint::new(
            1001, 0, // channel_pt=0
            220.0, 2200.0, 12345,
        );

        disp.dispatch(hint);

        let watch_ev = rx.try_recv().expect("should have event");
        assert_eq!(watch_ev.rule_ids, vec![42]);
        assert_eq!(watch_ev.channel_id, 1001);
        assert_eq!(watch_ev.point_id, 0);
        assert!((watch_ev.value - 220.0).abs() < f64::EPSILON);
        assert!((watch_ev.raw - 2200.0).abs() < f64::EPSILON);
        assert_eq!(watch_ev.timestamp_ms, 12345);
    }

    #[test]
    fn rebuild_replaces_the_complete_route_generation() {
        let (mut dispatcher, mut events) = PointWatchDispatcher::new();
        let rules = vec![TestRule {
            id: 43,
            enabled: true,
            trigger: TriggerConfig::OnChange {
                point_refs: vec![PointRef {
                    instance: 5,
                    point_type: PointKind::Measurement,
                    point: 10,
                }],
                time_deadband_ms: None,
                value_deadband: None,
            },
        }];
        let old_target = channel_point(1001, 0);
        let new_target = channel_point(1002, 0);

        let old_subscriptions = dispatcher
            .rebuild_from_rules(&rules, &[MeasurementRouteBinding::new(5, 10, old_target)]);
        assert_eq!(old_subscriptions, [old_target]);

        let new_subscriptions = dispatcher
            .rebuild_from_rules(&rules, &[MeasurementRouteBinding::new(5, 10, new_target)]);
        assert_eq!(new_subscriptions, [new_target]);

        dispatcher.dispatch(PointWatchHint::new(1001, 0, 1.0, 1.0, 1));
        assert!(
            events.try_recv().is_err(),
            "the dispatcher must not consult the old route generation"
        );
        dispatcher.dispatch(PointWatchHint::new(1002, 0, 2.0, 2.0, 2));
        assert_eq!(
            events.try_recv().expect("replacement route event").rule_ids,
            vec![43]
        );
    }

    #[test]
    fn dispatch_miss_sends_nothing() {
        let (disp, mut rx) = PointWatchDispatcher::new();

        let hint = PointWatchHint::new(9999, 0, 0.0, 0.0, 0);
        disp.dispatch(hint);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn dispatch_overflow_increments_dropped() {
        let (tx_cap, _rx_discard) = mpsc::channel::<WatchEvent>(1);
        let dropped_count = Arc::new(AtomicU64::new(0));
        let d = PointWatchDispatcher {
            sub_index: {
                let mut m = HashMap::new();
                m.insert((1001u32, 0u32), vec![1i64]);
                m
            },
            event_tx: tx_cap,
            dropped_count: Arc::clone(&dropped_count),
        };

        let hint = PointWatchHint::new(1001, 0, 1.0, 1.0, 0);

        d.dispatch(hint); // fills the channel
        d.dispatch(hint); // overflows → dropped

        assert_eq!(d.dropped_count(), 1);
    }
}
