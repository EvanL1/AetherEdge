//! Rule Scheduler - Periodic rule execution scheduler
//!
//! Manages rule execution based on trigger configurations:
//! - Interval: Execute rules at fixed intervals
//! - OnChange: Execute rules when subscribed points change beyond a deadband
//!
//! Implementation uses a 100ms tick-based approach. OnChange triggers are
//! evaluated by sampling subscribed point values once per tick (no separate
//! event-driven IPC); change detection compares against per-rule last-trigger
//! state guarded by both time and value deadbands.

use crate::error::Result;
use crate::executor::{RuleExecutionResult, RuleExecutor};
use crate::live_state::RuleLiveState;
use crate::logger::RuleLoggerManager;
use crate::repository;
use crate::types::Rule;
use aether_calc::StateStore;
use aether_routing::RoutingCache;
use aether_rtdb_shm::ActionDispatch;
use sqlx::SqlitePool;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio::time::interval;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

/// Default scheduler tick interval (100ms)
pub const DEFAULT_TICK_MS: u64 = 100;

/// Reference to a single point inside an instance.
///
/// Used by `TriggerConfig::OnChange` to identify which points a rule
/// subscribes to. The on-disk JSON shape is:
/// `{"instance": 1, "point_type": "measurement", "point": 0}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct PointRef {
    pub instance: u32,
    pub point_type: PointKind,
    pub point: u32,
}

impl PointRef {
    /// Stable string key used both as snapshot key and per-rule state key.
    /// Format: `"M:{instance}:{point}"` or `"A:{instance}:{point}"`.
    pub fn cache_key(&self) -> String {
        let prefix = match self.point_type {
            PointKind::Measurement => 'M',
            PointKind::Action => 'A',
        };
        format!("{}:{}:{}", prefix, self.instance, self.point)
    }
}

/// Point kind discriminator (mirrors aether-model's PointType but kept local
/// to avoid adding a cross-crate dependency for serialization only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PointKind {
    Measurement,
    Action,
}

/// Value deadband — filters out value changes smaller than the threshold.
///
/// JSON shapes:
/// - `{"type": "absolute", "threshold": 0.5}` — |new - last| > 0.5
/// - `{"type": "percent",  "threshold": 1.0}` — |new - last| / |last| > 1%
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ValueDeadband {
    Absolute { threshold: f64 },
    Percent { threshold: f64 },
}

impl ValueDeadband {
    /// Returns true if `(last, new)` constitutes a "real" change.
    ///
    /// Both inputs must be finite (callers filter NaN upstream).
    pub fn exceeds(&self, last: f64, new: f64) -> bool {
        let delta = (new - last).abs();
        match self {
            Self::Absolute { threshold } => delta > *threshold,
            Self::Percent { threshold } => {
                let basis = last.abs();
                if basis == 0.0 {
                    // Crossing zero: any non-zero new value is a change.
                    new != 0.0
                } else {
                    (delta / basis) * 100.0 > *threshold
                }
            },
        }
    }
}

/// Rule trigger configuration
///
/// Supports JSON deserialization for database storage:
/// - `{"type": "interval", "interval_ms": 1000}`
/// - `{"type": "on_change", "point_refs": [...], "time_deadband_ms": 200,
///    "value_deadband": {"type": "absolute", "threshold": 0.5}}`
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TriggerConfig {
    /// Execute rule at fixed intervals
    Interval {
        /// Interval in milliseconds
        interval_ms: u64,
    },
    /// Execute rule when any subscribed point's value changes beyond the
    /// configured deadbands.
    ///
    /// Both deadbands are optional and combined with AND semantics:
    /// - `time_deadband_ms` limits trigger frequency (defaults to 0 — every tick)
    /// - `value_deadband` filters out micro-fluctuations (defaults to None —
    ///   any non-equal finite value counts as a change)
    ///
    /// NaN handling: NaN values are skipped (not treated as a change). The
    /// first finite value after observation triggers once (initial sample or
    /// recovery from NaN).
    OnChange {
        point_refs: Vec<PointRef>,
        #[serde(default)]
        time_deadband_ms: Option<u64>,
        #[serde(default)]
        value_deadband: Option<ValueDeadband>,
    },
}

impl Default for TriggerConfig {
    fn default() -> Self {
        // Default to 1 second interval
        TriggerConfig::Interval { interval_ms: 1000 }
    }
}

/// Per-rule OnChange state — updated only after a successful trigger.
#[derive(Debug, Default, Clone)]
pub struct OnChangeState {
    /// Last triggered finite value, keyed by `PointRef::cache_key()`.
    /// Missing entry → never triggered for that point (first observation
    /// will fire).
    pub last_value: HashMap<String, f64>,
    /// Last trigger instant (rule-level, not per-point) for time deadband.
    pub last_trigger: Option<Instant>,
}

/// Pure decision function — returns true if an OnChange rule should fire
/// given the current snapshot and prior state.
///
/// Extracted as a free function so unit tests and benchmarks can exercise
/// deadband logic without setting up a full RuleScheduler.
pub fn should_trigger_onchange(
    state: &OnChangeState,
    point_refs: &[PointRef],
    time_deadband_ms: Option<u64>,
    value_deadband: Option<&ValueDeadband>,
    snapshot: &HashMap<String, Option<f64>>,
    now: Instant,
) -> bool {
    // Time deadband gate (rule-level)
    if let (Some(td), Some(last)) = (time_deadband_ms, state.last_trigger) {
        let elapsed = now.duration_since(last).as_millis() as u64;
        if elapsed < td {
            return false;
        }
    }

    // Trigger if any subscribed point exhibits a change beyond value deadband
    for pref in point_refs {
        let key = pref.cache_key();
        let new_value = match snapshot.get(&key) {
            Some(Some(v)) if v.is_finite() => *v,
            _ => continue, // missing or NaN → ignore this point
        };

        match state.last_value.get(&key) {
            None => return true, // first finite observation triggers
            Some(last) => {
                let changed = match value_deadband {
                    Some(vd) => vd.exceeds(*last, new_value),
                    None => last.total_cmp(&new_value).is_ne(),
                };
                if changed {
                    return true;
                }
            },
        }
    }
    false
}

/// Runtime state for a scheduled rule
struct ScheduledRule {
    /// Rule wrapped in Arc to avoid cloning during execution
    rule: Arc<Rule>,
    trigger: TriggerConfig,
    last_execution: Option<Instant>,
    /// Track last cooldown trigger time
    last_cooldown_start: Option<Instant>,
    /// OnChange-specific state (last seen values + last trigger time).
    /// Default for Interval rules; populated only for OnChange rules.
    onchange_state: OnChangeState,
}

// Allow PointWatchDispatcher::rebuild_from_rules to iterate scheduled rules
// without exposing the private ScheduledRule struct.
impl crate::point_watch_dispatcher::RuleSubscriptionInfo for ScheduledRule {
    fn rule_id(&self) -> i64 {
        self.rule.id
    }
    fn is_enabled(&self) -> bool {
        self.rule.enabled
    }
    fn trigger(&self) -> &TriggerConfig {
        &self.trigger
    }
}

/// Rule Scheduler - manages periodic rule execution
///
/// Generic over `S: StateStore` for stateful calculation memory. Live point
/// values always come from the injected SHM-backed [`RuleLiveState`].
pub struct RuleScheduler<S: StateStore = aether_calc::MemoryStateStore> {
    live_state: Arc<dyn RuleLiveState>,
    /// Rule executor instance with configurable state store
    executor: Arc<RuleExecutor<S>>,
    /// SQLite pool for rule persistence
    pool: SqlitePool,
    /// Cached rules with their trigger configs
    rules: Arc<RwLock<Vec<ScheduledRule>>>,
    /// Shutdown token (unified stop signal + running state)
    shutdown: CancellationToken,
    /// Scheduler tick interval in milliseconds
    tick_ms: u64,
    /// Rule logger manager for independent rule log files
    logger_manager: RuleLoggerManager,
    /// Maximum concurrent rule executions (default: 4)
    max_concurrency: usize,
    /// PointWatch fast path: receive events from PointWatchDispatcher.
    /// When present, `start()` selects on this channel alongside the
    /// 100 ms tick and immediately executes matching OnChange rules.
    watch_rx: Option<
        tokio::sync::Mutex<tokio::sync::mpsc::Receiver<crate::point_watch_dispatcher::WatchEvent>>,
    >,
    /// PointWatch rebuild handles. When all four are Some, `reload_rules`
    /// rebuilds the subscription index after loading. None in test mode or
    /// when PointWatch isn't wired in.
    ///
    /// `pw_routing_cache` is stored separately from `routing_cache` because the
    /// latter is only retained when SHM is available (gates SHM reverse lookup),
    /// while PointWatch rebuild needs routing regardless of SHM presence.
    pw_dispatcher:
        Option<Arc<std::sync::Mutex<crate::point_watch_dispatcher::PointWatchDispatcher>>>,
    pw_routing_cache: Option<Arc<RoutingCache>>,
    pw_channel_slot_index: Option<Arc<aether_rtdb_shm::ChannelToSlotIndex>>,
    pw_bitmap: Option<Arc<aether_rtdb_shm::SubscriptionBitmap>>,
}

impl RuleScheduler<aether_calc::MemoryStateStore> {
    /// Create a new rule scheduler with configurable tick interval (uses MemoryStateStore)
    ///
    /// # Arguments
    /// * `live_state` - authoritative live-state reader
    /// * `routing_cache` - Routing cache for M2C route lookups
    /// * `pool` - SQLite pool for rule persistence
    /// * `tick_ms` - Scheduler tick interval in milliseconds
    /// * `log_root` - Root directory for rule log files (e.g., "logs/automation")
    pub fn new<L>(
        live_state: Arc<L>,
        routing_cache: Arc<RoutingCache>,
        pool: SqlitePool,
        tick_ms: u64,
        log_root: PathBuf,
    ) -> Self
    where
        L: RuleLiveState + 'static,
    {
        Self {
            live_state: Arc::clone(&live_state) as Arc<dyn RuleLiveState>,
            executor: Arc::new(RuleExecutor::new(live_state, routing_cache)),
            pool,
            rules: Arc::new(RwLock::new(Vec::new())),
            shutdown: CancellationToken::new(),
            tick_ms,
            logger_manager: RuleLoggerManager::new(log_root),
            max_concurrency: 4,
            watch_rx: None,
            pw_dispatcher: None,
            pw_routing_cache: None,
            pw_channel_slot_index: None,
            pw_bitmap: None,
        }
    }
}

impl<S: StateStore + 'static> RuleScheduler<S> {
    /// Create with custom StateStore and full SHM support
    ///
    /// Use this constructor with a custom local state store.
    #[allow(clippy::too_many_arguments)]
    pub fn with_state_store<L>(
        live_state: Arc<L>,
        routing_cache: Arc<RoutingCache>,
        pool: SqlitePool,
        tick_ms: u64,
        log_root: PathBuf,
        state_store: Arc<S>,
        action_dispatch: Option<Arc<dyn ActionDispatch>>,
    ) -> Self
    where
        L: RuleLiveState + 'static,
    {
        let mut executor =
            RuleExecutor::with_state_store(Arc::clone(&live_state), routing_cache, state_store);
        if let Some(dispatch) = action_dispatch {
            executor = executor.with_action_dispatch(dispatch);
        }
        Self {
            live_state,
            executor: Arc::new(executor),
            pool,
            rules: Arc::new(RwLock::new(Vec::new())),
            shutdown: CancellationToken::new(),
            tick_ms,
            logger_manager: RuleLoggerManager::new(log_root),
            max_concurrency: 4,
            watch_rx: None,
            pw_dispatcher: None,
            pw_routing_cache: None,
            pw_channel_slot_index: None,
            pw_bitmap: None,
        }
    }

    /// Set maximum concurrent rule executions (must be called before wrapping in Arc)
    pub fn set_max_concurrency(&mut self, n: usize) {
        self.max_concurrency = n.max(1);
    }

    /// Rebuild the `PointWatchDispatcher` subscription index from the currently
    /// loaded rules. Call this after `load_rules` / `reload_rules` to keep the
    /// event-driven path in sync.
    pub async fn rebuild_point_watch(
        &self,
        dispatcher: &mut crate::point_watch_dispatcher::PointWatchDispatcher,
        routing_cache: &aether_routing::RoutingCache,
        channel_slot_index: &aether_rtdb_shm::ChannelToSlotIndex,
        bitmap: &aether_rtdb_shm::SubscriptionBitmap,
    ) {
        let rules = self.rules.read().await;
        dispatcher.rebuild_from_rules(&rules, routing_cache, channel_slot_index, bitmap);
    }

    /// Attach a PointWatch event receiver.
    ///
    /// When set, `start()` selects on this channel alongside the 100 ms tick.
    /// Must be called before `start()` (and before wrapping in `Arc`).
    pub fn set_watch_receiver(
        &mut self,
        rx: tokio::sync::mpsc::Receiver<crate::point_watch_dispatcher::WatchEvent>,
    ) {
        self.watch_rx = Some(tokio::sync::Mutex::new(rx));
    }

    /// Store handles required to rebuild the PointWatch subscription index
    /// during `reload_rules`. Must be called before wrapping in `Arc` and
    /// before any rule reload that should reflect subscription changes.
    ///
    /// Without these handles, `reload_rules` only refreshes the rule cache —
    /// the SubscriptionBitmap and dispatcher index keep their previous state,
    /// so newly-added OnChange rules won't fire until service restart.
    pub fn set_point_watch_rebuild_handles(
        &mut self,
        dispatcher: Arc<std::sync::Mutex<crate::point_watch_dispatcher::PointWatchDispatcher>>,
        routing_cache: Arc<RoutingCache>,
        channel_slot_index: Arc<aether_rtdb_shm::ChannelToSlotIndex>,
        bitmap: Arc<aether_rtdb_shm::SubscriptionBitmap>,
    ) {
        self.pw_dispatcher = Some(dispatcher);
        self.pw_routing_cache = Some(routing_cache);
        self.pw_channel_slot_index = Some(channel_slot_index);
        self.pw_bitmap = Some(bitmap);
    }

    /// Load rules from database and initialize scheduler state
    pub async fn load_rules(&self) -> Result<usize> {
        let db_rules = repository::load_enabled_rules(&self.pool).await?;
        let count = db_rules.len();

        let scheduled: Vec<ScheduledRule> = db_rules
            .into_iter()
            .map(|rule| {
                // Parse trigger_config from database, fallback to cooldown_ms
                let trigger = rule
                    .trigger_config
                    .as_ref()
                    .and_then(|json| serde_json::from_str(json).ok())
                    .unwrap_or_else(|| {
                        // Fallback: use cooldown_ms as interval (minimum 1000ms)
                        let interval_ms = if rule.cooldown_ms > 0 {
                            rule.cooldown_ms
                        } else {
                            1000
                        };
                        TriggerConfig::Interval { interval_ms }
                    });

                ScheduledRule {
                    rule: Arc::new(rule),
                    trigger,
                    last_execution: None,
                    last_cooldown_start: None,
                    onchange_state: OnChangeState::default(),
                }
            })
            .collect();

        let mut rules = self.rules.write().await;
        *rules = scheduled;

        info!("Rules: {} loaded", count);
        Ok(count)
    }

    /// Reload rules from database (hot reload).
    ///
    /// When PointWatch rebuild handles are present (set via
    /// `set_point_watch_rebuild_handles`), this also rebuilds the
    /// subscription bitmap + dispatcher index so newly-added or
    /// newly-modified OnChange rules start receiving events immediately
    /// without a service restart.
    pub async fn reload_rules(&self) -> Result<usize> {
        info!("Rules reloading");
        let count = self.load_rules().await?;

        if let (Some(disp), Some(routing), Some(slot_idx), Some(bitmap)) = (
            &self.pw_dispatcher,
            &self.pw_routing_cache,
            &self.pw_channel_slot_index,
            &self.pw_bitmap,
        ) {
            let rules = self.rules.read().await;
            // Mutex poison only indicates a prior holder panicked; the inner
            // dispatcher state is still valid, so recover via into_inner().
            let mut d = disp.lock().unwrap_or_else(|p| p.into_inner());
            d.rebuild_from_rules(&rules, routing, slot_idx, bitmap);
            info!(
                "PointWatch subscription index rebuilt: {} (ch,pt) pairs subscribed",
                d.subscription_count()
            );
        }

        Ok(count)
    }

    /// Start the scheduler loop
    pub async fn start(&self) {
        if self.shutdown.is_cancelled() {
            warn!("Scheduler already stopped");
            return;
        }

        info!("Scheduler start ({}ms)", self.tick_ms);

        let mut tick_interval = interval(Duration::from_millis(self.tick_ms));

        loop {
            // Branch 1 (common): 100ms tick
            // Branch 2 (optional fast path): PointWatch event
            if let Some(ref watch_mutex) = self.watch_rx {
                let mut watch_guard = watch_mutex.lock().await;
                tokio::select! {
                    _ = tick_interval.tick() => {
                        drop(watch_guard);
                        if let Err(e) = self.tick().await {
                            error!("Tick err: {}", e);
                        }
                    }
                    Some(watch_event) = watch_guard.recv() => {
                        drop(watch_guard);
                        if let Err(e) = self.execute_watch_triggered(&watch_event).await {
                            error!("Watch trigger err: {}", e);
                        }
                    }
                    _ = self.shutdown.cancelled() => {
                        drop(watch_guard);
                        info!("Scheduler shutdown");
                        break;
                    }
                }
            } else {
                tokio::select! {
                    _ = tick_interval.tick() => {
                        if let Err(e) = self.tick().await {
                            error!("Tick err: {}", e);
                        }
                    }
                    _ = self.shutdown.cancelled() => {
                        info!("Scheduler shutdown");
                        break;
                    }
                }
            }
        }

        info!("Scheduler stopped");
    }

    /// Stop the scheduler
    pub fn stop(&self) {
        info!("Scheduler stopping");
        self.shutdown.cancel();
    }

    /// Check if scheduler is running
    ///
    /// Returns true if the scheduler has not been stopped yet.
    /// Note: This only indicates whether stop() was called, not whether
    /// the scheduler loop has actually exited.
    pub fn is_running(&self) -> bool {
        !self.shutdown.is_cancelled()
    }

    /// Single scheduler tick - check all rules and execute if due
    ///
    /// Snapshot execution pattern for minimal lock hold time
    /// - Phase 0: Collect OnChange subscriptions, batch-fetch values (no lock held)
    /// - Phase 1: Read lock to filter rules due for execution (~10μs)
    /// - Phase 2: Execute rules without holding any lock (bulk of time)
    /// - Phase 3: Write lock to update timestamps + onchange state (~100μs)
    async fn tick(&self) -> Result<()> {
        let now = Instant::now();

        // ── Phase 0: collect unique OnChange subscriptions ──────────────────
        let subscriptions: HashSet<PointRef> = {
            let rules = self.rules.read().await;
            rules
                .iter()
                .filter(|s| s.rule.enabled)
                .filter_map(|s| match &s.trigger {
                    TriggerConfig::OnChange { point_refs, .. } => Some(point_refs.clone()),
                    _ => None,
                })
                .flatten()
                .collect()
        };

        // ── Phase 0.5: batch-fetch current values (None = missing/non-finite) ─
        let snapshot: HashMap<String, Option<f64>> = if subscriptions.is_empty() {
            HashMap::new()
        } else {
            self.fetch_point_snapshot(&subscriptions).await
        };

        // ── Phase 1: read-lock filter (sync, fast) ──────────────────────────
        let rules_to_execute: Vec<(usize, Arc<Rule>, bool)> = {
            let rules = self.rules.read().await;
            rules
                .iter()
                .enumerate()
                .filter_map(|(idx, scheduled)| {
                    if !scheduled.rule.enabled {
                        return None;
                    }

                    let is_onchange = matches!(scheduled.trigger, TriggerConfig::OnChange { .. });
                    let should_execute = match &scheduled.trigger {
                        TriggerConfig::Interval { interval_ms } => {
                            match scheduled.last_execution {
                                None => true, // First execution
                                Some(last) => {
                                    let elapsed = now.duration_since(last).as_millis() as u64;
                                    elapsed >= *interval_ms
                                },
                            }
                        },
                        TriggerConfig::OnChange {
                            point_refs,
                            time_deadband_ms,
                            value_deadband,
                        } => should_trigger_onchange(
                            &scheduled.onchange_state,
                            point_refs,
                            *time_deadband_ms,
                            value_deadband.as_ref(),
                            &snapshot,
                            now,
                        ),
                    };

                    // Check cooldown
                    let cooldown_ok = if scheduled.rule.cooldown_ms > 0 {
                        match scheduled.last_cooldown_start {
                            None => true,
                            Some(start) => {
                                let elapsed = now.duration_since(start).as_millis() as u64;
                                elapsed >= scheduled.rule.cooldown_ms
                            },
                        }
                    } else {
                        true
                    };

                    if should_execute && cooldown_ok {
                        Some((idx, Arc::clone(&scheduled.rule), is_onchange))
                    } else {
                        None
                    }
                })
                .collect()
        }; // Read lock released here (~10μs)

        if rules_to_execute.is_empty() {
            return Ok(());
        }

        // Phase 2: Execute rules in parallel without holding any lock
        // Use buffer_unordered for concurrent execution with bounded parallelism
        use futures::stream::{self, StreamExt};

        struct ExecutionOutcome {
            idx: usize,
            rule_id: i64,
            rule_name: String,
            is_onchange: bool,
            result: Result<RuleExecutionResult>,
        }

        // Execute rules concurrently (max self.max_concurrency parallel)
        let executor = Arc::clone(&self.executor);
        let execution_futures = rules_to_execute
            .into_iter()
            .map(|(idx, rule, is_onchange)| {
                let executor = Arc::clone(&executor);
                async move {
                    debug!("Executing rule: {}", rule.id);
                    let rule_id = rule.id;
                    let rule_name = rule.name.clone();
                    let result = executor.execute(&rule).await;
                    ExecutionOutcome {
                        idx,
                        rule_id,
                        rule_name,
                        is_onchange,
                        result,
                    }
                }
            });

        let execution_results: Vec<ExecutionOutcome> = stream::iter(execution_futures)
            .buffer_unordered(self.max_concurrency)
            .collect()
            .await;

        // Process results sequentially (logging and local history writes)
        struct TimestampUpdate {
            idx: usize,
            rule_id: i64,
            start_cooldown: bool,
            is_onchange: bool,
            /// Per-point values surfaced by the executor (SHM-first read).
            /// Used to advance OnChange last_value against what executor
            /// actually saw from SHM.
            executor_point_values: Arc<HashMap<String, f64>>,
        }
        let mut updates: Vec<TimestampUpdate> = Vec::with_capacity(execution_results.len());

        for outcome in execution_results {
            match outcome.result {
                Ok(result) => {
                    // Log rule execution to independent rule log file
                    let logger = self
                        .logger_manager
                        .get_logger(outcome.rule_id, &outcome.rule_name);
                    logger.log_execution(&result, &result.variable_values);

                    // Persist locally for API/WebSocket diagnostics.
                    self.write_rule_exec(outcome.rule_id, &outcome.rule_name, &result)
                        .await;

                    let start_cooldown = result.success && !result.actions_executed.is_empty();

                    if result.success {
                        debug!(
                            "Rule {} executed successfully, {} actions",
                            result.rule_id,
                            result.actions_executed.len()
                        );
                    } else {
                        warn!("Rule {} fail: {:?}", result.rule_id, result.error);
                    }

                    let executor_point_values = Arc::clone(&result.point_values);
                    updates.push(TimestampUpdate {
                        idx: outcome.idx,
                        rule_id: outcome.rule_id,
                        start_cooldown,
                        is_onchange: outcome.is_onchange,
                        executor_point_values,
                    });
                },
                Err(e) => {
                    error!("Rule {} err: {}", outcome.rule_id, e);
                    // Still update last_execution to prevent retry spam
                    updates.push(TimestampUpdate {
                        idx: outcome.idx,
                        rule_id: outcome.rule_id,
                        start_cooldown: false,
                        is_onchange: outcome.is_onchange,
                        executor_point_values: Arc::new(HashMap::new()),
                    });
                },
            }
        }

        // Phase 3: Write lock to update timestamps + onchange state (fast)
        if !updates.is_empty() {
            let mut rules = self.rules.write().await;
            for update in updates {
                if let Some(scheduled) = rules.get_mut(update.idx) {
                    // Verify rule ID matches (safety check against concurrent modifications)
                    if scheduled.rule.id != update.rule_id {
                        continue;
                    }
                    scheduled.last_execution = Some(now);
                    if update.start_cooldown {
                        scheduled.last_cooldown_start = Some(now);
                    }
                    // For OnChange rules, advance per-point last_value to the
                    // values we just sampled in Phase 0. This is what gives
                    // the deadband its memory: future ticks compare against
                    // the value at the moment of this trigger, not the
                    // ever-changing latest sample.
                    if update.is_onchange
                        && let TriggerConfig::OnChange { point_refs, .. } = &scheduled.trigger
                    {
                        for pref in point_refs {
                            let key = pref.cache_key();
                            // Prefer the executor's actual SHM read. Fall back
                            // to the phase-0 SHM snapshot only when the executor
                            // did not use this point in the current flow.
                            let v_opt = update
                                .executor_point_values
                                .get(&key)
                                .copied()
                                .or_else(|| snapshot.get(&key).and_then(|opt| *opt));
                            if let Some(v) = v_opt
                                && v.is_finite()
                            {
                                scheduled.onchange_state.last_value.insert(key, v);
                            }
                        }
                        scheduled.onchange_state.last_trigger = Some(now);
                    }
                }
            }
        } // Write lock released here (~100μs)

        Ok(())
    }

    /// Get current rules count
    pub async fn rules_count(&self) -> usize {
        self.rules.read().await.len()
    }

    /// Get scheduler status
    pub async fn status(&self) -> SchedulerStatus {
        let rules = self.rules.read().await;
        let enabled_count = rules.iter().filter(|r| r.rule.enabled).count();

        SchedulerStatus {
            running: self.is_running(),
            total_rules: rules.len(),
            enabled_rules: enabled_count,
            tick_interval_ms: self.tick_ms,
        }
    }

    /// Execute a specific rule by ID (manual trigger)
    pub async fn execute_rule(&self, rule_id: i64) -> Result<RuleExecutionResult> {
        // Load the rule from database
        let rule = repository::get_rule_for_execution(&self.pool, rule_id).await?;

        // Execute it
        self.executor.execute(&rule).await
    }

    /// Batch-fetch current values for all subscribed points.
    ///
    /// Reads through the authoritative live-state adapter. Production binds
    /// this adapter to the current SHM generation. `None` means missing or
    /// non-finite; there is no fallback state plane.
    async fn fetch_point_snapshot(
        &self,
        subscriptions: &HashSet<PointRef>,
    ) -> HashMap<String, Option<f64>> {
        let mut out = HashMap::with_capacity(subscriptions.len());
        for pref in subscriptions {
            let instance_type = match pref.point_type {
                PointKind::Measurement => 0,
                PointKind::Action => 1,
            };
            let value = self
                .live_state
                .get_instance(pref.instance, instance_type, pref.point)
                .map(|(value, _timestamp_ms)| value)
                .filter(|value| value.is_finite());
            out.insert(pref.cache_key(), value);
        }
        out
    }

    /// Persist a rule execution in the local SQLite history.
    async fn write_rule_exec(&self, rule_id: i64, _rule_name: &str, result: &RuleExecutionResult) {
        if let Err(error) = persist_rule_execution(&self.pool, result).await {
            warn!(rule_id, %error, "failed to persist rule execution history");
        }
    }

    // ──────────────────────────────────────────────────────────────────────
    // PointWatch fast-path execution
    // ──────────────────────────────────────────────────────────────────────

    /// Execute rules triggered by a PointWatch event (event-driven fast path).
    ///
    /// Called from `start()` when a `WatchEvent` arrives on `watch_rx`.
    /// The event selects candidates only. All referenced points are re-read
    /// from the current SHM generation before deadband evaluation, so stale,
    /// duplicated, or spuriously routed hints cannot become rule inputs.
    ///
    /// Does NOT remove the 100 ms fallback — both paths run in parallel.
    /// The tick path provides the fallback when the UDS is down or hints drop.
    async fn execute_watch_triggered(
        &self,
        watch_event: &crate::point_watch_dispatcher::WatchEvent,
    ) -> Result<()> {
        let now = Instant::now();
        let rule_id_set: std::collections::HashSet<i64> =
            watch_event.rule_ids.iter().copied().collect();

        let subscriptions: HashSet<PointRef> = {
            let rules = self.rules.read().await;
            rules
                .iter()
                .filter(|scheduled| {
                    scheduled.rule.enabled && rule_id_set.contains(&scheduled.rule.id)
                })
                .filter_map(|scheduled| match &scheduled.trigger {
                    TriggerConfig::OnChange { point_refs, .. } => Some(point_refs.clone()),
                    TriggerConfig::Interval { .. } => None,
                })
                .flatten()
                .collect()
        };
        if subscriptions.is_empty() {
            return Ok(());
        }
        let snapshot = self.fetch_point_snapshot(&subscriptions).await;

        let to_execute: Vec<(usize, Arc<crate::types::Rule>)> = {
            let rules = self.rules.read().await;
            rules
                .iter()
                .enumerate()
                .filter_map(|(idx, scheduled)| {
                    if !scheduled.rule.enabled || !rule_id_set.contains(&scheduled.rule.id) {
                        return None;
                    }
                    let TriggerConfig::OnChange {
                        point_refs,
                        time_deadband_ms,
                        value_deadband,
                    } = &scheduled.trigger
                    else {
                        return None;
                    };
                    let changed = should_trigger_onchange(
                        &scheduled.onchange_state,
                        point_refs,
                        *time_deadband_ms,
                        value_deadband.as_ref(),
                        &snapshot,
                        now,
                    );
                    let cooldown_ok = scheduled.rule.cooldown_ms == 0
                        || scheduled.last_cooldown_start.is_none_or(|start| {
                            now.duration_since(start).as_millis() as u64
                                >= scheduled.rule.cooldown_ms
                        });
                    (changed && cooldown_ok).then(|| (idx, Arc::clone(&scheduled.rule)))
                })
                .collect()
        };

        if to_execute.is_empty() {
            return Ok(());
        }

        // Execute the matching rules
        use futures::stream::{self, StreamExt};
        let executor = Arc::clone(&self.executor);
        let results: Vec<(usize, i64, String, Result<RuleExecutionResult>)> =
            stream::iter(to_execute.into_iter().map(|(idx, rule)| {
                let executor = Arc::clone(&executor);
                async move {
                    let id = rule.id;
                    let name = rule.name.clone();
                    let result = executor.execute(&rule).await;
                    (idx, id, name, result)
                }
            }))
            .buffer_unordered(self.max_concurrency)
            .collect()
            .await;

        struct WatchUpdate {
            idx: usize,
            rule_id: i64,
            start_cooldown: bool,
            advance_onchange: bool,
            point_values: Arc<HashMap<String, f64>>,
        }
        let mut updates = Vec::with_capacity(results.len());
        for (idx, rule_id, rule_name, result) in results {
            match result {
                Ok(exec_result) => {
                    let logger = self.logger_manager.get_logger(rule_id, &rule_name);
                    logger.log_execution(&exec_result, &exec_result.variable_values);
                    self.write_rule_exec(rule_id, &rule_name, &exec_result)
                        .await;
                    updates.push(WatchUpdate {
                        idx,
                        rule_id,
                        start_cooldown: exec_result.success
                            && !exec_result.actions_executed.is_empty(),
                        advance_onchange: true,
                        point_values: Arc::clone(&exec_result.point_values),
                    });
                },
                Err(error) => {
                    error!("Watch-triggered rule {} err: {}", rule_id, error);
                    updates.push(WatchUpdate {
                        idx,
                        rule_id,
                        start_cooldown: false,
                        advance_onchange: false,
                        point_values: Arc::new(HashMap::new()),
                    });
                },
            }
        }

        if !updates.is_empty() {
            let mut rules = self.rules.write().await;
            for update in updates {
                let Some(scheduled) = rules.get_mut(update.idx) else {
                    continue;
                };
                if scheduled.rule.id != update.rule_id {
                    continue;
                }
                scheduled.last_execution = Some(now);
                if update.start_cooldown {
                    scheduled.last_cooldown_start = Some(now);
                }
                if update.advance_onchange
                    && let TriggerConfig::OnChange { point_refs, .. } = &scheduled.trigger
                {
                    for pref in point_refs {
                        let key = pref.cache_key();
                        let value = update
                            .point_values
                            .get(&key)
                            .copied()
                            .or_else(|| snapshot.get(&key).and_then(|value| *value));
                        if let Some(value) = value
                            && value.is_finite()
                        {
                            scheduled.onchange_state.last_value.insert(key, value);
                        }
                    }
                    scheduled.onchange_state.last_trigger = Some(now);
                }
            }
        }

        Ok(())
    }
}

async fn persist_rule_execution(pool: &SqlitePool, result: &RuleExecutionResult) -> Result<()> {
    let payload = serde_json::to_string(result)?;
    sqlx::query(
        "INSERT INTO rule_history \
         (rule_id, triggered_at, execution_result, error) \
         VALUES (?, CURRENT_TIMESTAMP, ?, ?)",
    )
    .bind(result.rule_id)
    .bind(payload)
    .bind(result.error.as_deref())
    .execute(pool)
    .await?;
    Ok(())
}

/// Scheduler status information
#[derive(Debug, Clone)]
pub struct SchedulerStatus {
    pub running: bool,
    pub total_rules: usize,
    pub enabled_rules: usize,
    pub tick_interval_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{RuleFlow, RuleNode, RuleWires};
    use std::collections::HashMap;

    #[test]
    fn test_trigger_config_default() {
        let TriggerConfig::Interval { interval_ms } = TriggerConfig::default() else {
            panic!("Default should be Interval");
        };
        assert_eq!(interval_ms, 1000);
    }

    /// Helper to create a minimal test rule
    fn create_test_rule(id: i64, name: &str, cooldown_ms: u64) -> Rule {
        let mut nodes = HashMap::new();
        nodes.insert(
            "start".to_string(),
            RuleNode::Start {
                wires: RuleWires {
                    default: vec!["end".to_string()],
                },
            },
        );
        nodes.insert("end".to_string(), RuleNode::End);

        Rule {
            id,
            name: name.to_string(),
            description: None,
            enabled: true,
            priority: 100,
            cooldown_ms,
            trigger_config: None,
            flow: RuleFlow {
                start_node: "start".to_string(),
                nodes,
            },
        }
    }

    #[test]
    fn test_scheduled_rule_trigger_interval() {
        let rule = Arc::new(create_test_rule(1, "Interval Test", 0));

        let scheduled = ScheduledRule {
            rule,
            trigger: TriggerConfig::Interval { interval_ms: 500 },
            last_execution: None,
            last_cooldown_start: None,
            onchange_state: OnChangeState::default(),
        };

        // Verify trigger config
        match scheduled.trigger {
            TriggerConfig::Interval { interval_ms } => {
                assert_eq!(interval_ms, 500);
            },
            TriggerConfig::OnChange { .. } => panic!("Expected Interval"),
        }
    }

    #[test]
    fn test_multiple_scheduled_rules_share_nothing() {
        // Create two independent rules
        let rule1 = Arc::new(create_test_rule(1, "Rule 1", 1000));
        let rule2 = Arc::new(create_test_rule(2, "Rule 2", 2000));

        let scheduled1 = ScheduledRule {
            rule: Arc::clone(&rule1),
            trigger: TriggerConfig::Interval { interval_ms: 100 },
            last_execution: None,
            last_cooldown_start: None,
            onchange_state: OnChangeState::default(),
        };

        let scheduled2 = ScheduledRule {
            rule: Arc::clone(&rule2),
            trigger: TriggerConfig::Interval { interval_ms: 200 },
            last_execution: None,
            last_cooldown_start: None,
            onchange_state: OnChangeState::default(),
        };

        // Verify they are independent
        assert!(!Arc::ptr_eq(&scheduled1.rule, &scheduled2.rule));
        assert_eq!(scheduled1.rule.id, 1);
        assert_eq!(scheduled2.rule.id, 2);
    }

    #[tokio::test]
    async fn test_parallel_execution_collects_all_results() {
        use futures::stream::{self, StreamExt};

        // Simulate parallel execution pattern used in tick()
        let items = vec![(0, 10), (1, 20), (2, 30), (3, 40)];

        let results: Vec<(usize, i32)> =
            stream::iter(items.into_iter().map(|(idx, val)| async move {
                // Simulate async work
                tokio::time::sleep(tokio::time::Duration::from_micros(10)).await;
                (idx, val * 2)
            }))
            .buffer_unordered(4)
            .collect()
            .await;

        // All results should be collected (order may vary due to unordered)
        assert_eq!(results.len(), 4);

        // Verify all values are processed correctly
        let sum: i32 = results.iter().map(|(_, v)| v).sum();
        assert_eq!(sum, 200); // (10+20+30+40) * 2 = 200
    }

    #[tokio::test]
    async fn pointwatch_payload_is_only_a_hint_and_cannot_trigger_on_stale_value() {
        let live_state = Arc::new(crate::MemoryRuleLiveState::new());
        assert!(live_state.set_instance(1, 0, 0, 10.0, 1));
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open scheduler database");
        let log_dir = tempfile::tempdir().expect("temporary rule logs");
        let scheduler = RuleScheduler::new(
            live_state,
            Arc::new(RoutingCache::new()),
            pool,
            100,
            log_dir.path().to_path_buf(),
        );
        let point = PointRef {
            instance: 1,
            point_type: PointKind::Measurement,
            point: 0,
        };
        let mut onchange_state = OnChangeState::default();
        onchange_state.last_value.insert(point.cache_key(), 10.0);
        scheduler.rules.write().await.push(ScheduledRule {
            rule: Arc::new(create_test_rule(7, "hint-only", 0)),
            trigger: TriggerConfig::OnChange {
                point_refs: vec![point],
                time_deadband_ms: None,
                value_deadband: None,
            },
            last_execution: None,
            last_cooldown_start: None,
            onchange_state,
        });

        scheduler
            .execute_watch_triggered(&crate::point_watch_dispatcher::WatchEvent {
                rule_ids: vec![7],
                channel_id: 10,
                point_id: 0,
                value: 999.0,
                raw: 999.0,
                timestamp_ms: 1,
            })
            .await
            .expect("process hint");

        let rules = scheduler.rules.read().await;
        assert!(
            rules[0].last_execution.is_none(),
            "event payload must not trigger when the re-read value is unchanged"
        );
        assert_eq!(rules[0].onchange_state.last_value[&point.cache_key()], 10.0);
    }

    #[tokio::test]
    async fn rule_execution_is_persisted_to_embedded_history() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open in-memory sqlite");
        sqlx::query(
            "CREATE TABLE rule_history (\
                 id INTEGER PRIMARY KEY AUTOINCREMENT,\
                 rule_id INTEGER NOT NULL,\
                 triggered_at TIMESTAMP NOT NULL,\
                 execution_result TEXT,\
                 error TEXT\
             )",
        )
        .execute(&pool)
        .await
        .expect("create history table");
        let result = RuleExecutionResult {
            rule_id: 17,
            success: true,
            actions_executed: Vec::new(),
            error: None,
            execution_path: vec!["start".to_string(), "end".to_string()],
            matched_condition: None,
            variable_values: Arc::new(HashMap::from([("soc".to_string(), 52.5)])),
            point_values: Arc::new(HashMap::new()),
            node_details: HashMap::new(),
        };

        persist_rule_execution(&pool, &result)
            .await
            .expect("persist local audit");

        let (rule_id, payload, error): (i64, String, Option<String>) = sqlx::query_as(
            "SELECT rule_id, execution_result, error FROM rule_history ORDER BY id DESC LIMIT 1",
        )
        .fetch_one(&pool)
        .await
        .expect("read local audit");
        let payload: serde_json::Value =
            serde_json::from_str(&payload).expect("valid execution json");
        assert_eq!(rule_id, 17);
        assert_eq!(payload["success"], true);
        assert_eq!(payload["variable_values"]["soc"], 52.5);
        assert!(error.is_none());
    }

    // ────────────────────────────────────────────────────────────────────
    // OnChange + Deadband tests
    // ────────────────────────────────────────────────────────────────────

    fn pref(instance: u32, point: u32) -> PointRef {
        PointRef {
            instance,
            point_type: PointKind::Measurement,
            point,
        }
    }

    fn snap(pairs: &[(&PointRef, Option<f64>)]) -> HashMap<String, Option<f64>> {
        pairs.iter().map(|(p, v)| (p.cache_key(), *v)).collect()
    }

    #[test]
    fn point_ref_cache_key_format() {
        let p = pref(7, 42);
        assert_eq!(p.cache_key(), "M:7:42");
        let pa = PointRef {
            instance: 1,
            point_type: PointKind::Action,
            point: 0,
        };
        assert_eq!(pa.cache_key(), "A:1:0");
    }

    #[test]
    fn value_deadband_absolute() {
        let db = ValueDeadband::Absolute { threshold: 0.5 };
        assert!(!db.exceeds(220.0, 220.4));
        assert!(!db.exceeds(220.0, 220.5)); // boundary, not strictly greater
        assert!(db.exceeds(220.0, 220.6));
        assert!(db.exceeds(220.0, 219.0)); // direction-agnostic
    }

    #[test]
    fn value_deadband_percent() {
        let db = ValueDeadband::Percent { threshold: 1.0 };
        assert!(!db.exceeds(220.0, 221.0)); // ~0.45%
        assert!(db.exceeds(220.0, 223.0)); // ~1.36%
    }

    #[test]
    fn value_deadband_percent_from_zero_basis() {
        let db = ValueDeadband::Percent { threshold: 5.0 };
        assert!(db.exceeds(0.0, 0.001));
        assert!(!db.exceeds(0.0, 0.0));
    }

    #[test]
    fn trigger_config_serde_interval() {
        let json = r#"{"type":"interval","interval_ms":1000}"#;
        let parsed: TriggerConfig = serde_json::from_str(json).unwrap();
        let TriggerConfig::Interval { interval_ms } = parsed else {
            panic!("expected Interval");
        };
        assert_eq!(interval_ms, 1000);
    }

    #[test]
    fn trigger_config_serde_onchange_full() {
        let json = r#"{
            "type":"on_change",
            "point_refs":[{"instance":1,"point_type":"measurement","point":0}],
            "time_deadband_ms":200,
            "value_deadband":{"type":"absolute","threshold":0.5}
        }"#;
        let parsed: TriggerConfig = serde_json::from_str(json).unwrap();
        match parsed {
            TriggerConfig::OnChange {
                point_refs,
                time_deadband_ms,
                value_deadband,
            } => {
                assert_eq!(point_refs.len(), 1);
                assert_eq!(point_refs[0].instance, 1);
                assert_eq!(point_refs[0].point_type, PointKind::Measurement);
                assert_eq!(time_deadband_ms, Some(200));
                assert!(matches!(
                    value_deadband,
                    Some(ValueDeadband::Absolute { threshold }) if threshold == 0.5
                ));
            },
            _ => panic!("expected OnChange"),
        }
    }

    #[test]
    fn trigger_config_serde_onchange_minimal_defaults() {
        let json = r#"{
            "type":"on_change",
            "point_refs":[{"instance":2,"point_type":"action","point":3}]
        }"#;
        let parsed: TriggerConfig = serde_json::from_str(json).unwrap();
        match parsed {
            TriggerConfig::OnChange {
                point_refs,
                time_deadband_ms,
                value_deadband,
            } => {
                assert_eq!(point_refs[0].point_type, PointKind::Action);
                assert!(time_deadband_ms.is_none());
                assert!(value_deadband.is_none());
            },
            _ => panic!("expected OnChange"),
        }
    }

    #[test]
    fn onchange_first_observation_triggers() {
        let state = OnChangeState::default();
        let p = pref(1, 0);
        let snapshot = snap(&[(&p, Some(220.0))]);
        assert!(should_trigger_onchange(
            &state,
            &[p],
            None,
            None,
            &snapshot,
            Instant::now()
        ));
    }

    #[test]
    fn onchange_no_change_no_trigger() {
        let p = pref(1, 0);
        let mut state = OnChangeState::default();
        state.last_value.insert(p.cache_key(), 220.0);
        let snapshot = snap(&[(&p, Some(220.0))]);
        assert!(!should_trigger_onchange(
            &state,
            &[p],
            None,
            None,
            &snapshot,
            Instant::now()
        ));
    }

    #[test]
    fn onchange_value_deadband_filters_noise() {
        let p = pref(1, 0);
        let mut state = OnChangeState::default();
        state.last_value.insert(p.cache_key(), 220.0);
        let snapshot = snap(&[(&p, Some(220.3))]);
        let db = ValueDeadband::Absolute { threshold: 0.5 };
        assert!(!should_trigger_onchange(
            &state,
            &[p],
            None,
            Some(&db),
            &snapshot,
            Instant::now()
        ));
    }

    #[test]
    fn onchange_value_deadband_passes_real_change() {
        let p = pref(1, 0);
        let mut state = OnChangeState::default();
        state.last_value.insert(p.cache_key(), 220.0);
        let snapshot = snap(&[(&p, Some(221.0))]);
        let db = ValueDeadband::Absolute { threshold: 0.5 };
        assert!(should_trigger_onchange(
            &state,
            &[p],
            None,
            Some(&db),
            &snapshot,
            Instant::now()
        ));
    }

    #[test]
    fn onchange_time_deadband_blocks_recent_trigger() {
        let p = pref(1, 0);
        let mut state = OnChangeState::default();
        state.last_value.insert(p.cache_key(), 220.0);
        state.last_trigger = Some(Instant::now());

        let snapshot = snap(&[(&p, Some(230.0))]);
        assert!(!should_trigger_onchange(
            &state,
            &[p],
            Some(500),
            None,
            &snapshot,
            Instant::now()
        ));
    }

    #[test]
    fn onchange_time_deadband_allows_after_window() {
        let p = pref(1, 0);
        let mut state = OnChangeState::default();
        state.last_value.insert(p.cache_key(), 220.0);
        state.last_trigger = Some(Instant::now() - Duration::from_millis(600));

        let snapshot = snap(&[(&p, Some(230.0))]);
        assert!(should_trigger_onchange(
            &state,
            &[p],
            Some(500),
            None,
            &snapshot,
            Instant::now()
        ));
    }

    #[test]
    fn onchange_both_deadbands_anded() {
        let p = pref(1, 0);
        let mut state = OnChangeState::default();
        state.last_value.insert(p.cache_key(), 220.0);
        state.last_trigger = Some(Instant::now() - Duration::from_millis(600));

        let value_db = ValueDeadband::Absolute { threshold: 0.5 };

        let snapshot_small = snap(&[(&p, Some(220.2))]);
        assert!(!should_trigger_onchange(
            &state,
            &[p],
            Some(500),
            Some(&value_db),
            &snapshot_small,
            Instant::now()
        ));

        let snapshot_big = snap(&[(&p, Some(221.0))]);
        assert!(should_trigger_onchange(
            &state,
            &[p],
            Some(500),
            Some(&value_db),
            &snapshot_big,
            Instant::now()
        ));
    }

    #[test]
    fn onchange_nan_inbound_does_not_trigger() {
        let p = pref(1, 0);
        let mut state = OnChangeState::default();
        state.last_value.insert(p.cache_key(), 220.0);
        let now = Instant::now();

        let mut snapshot: HashMap<String, Option<f64>> = HashMap::new();
        snapshot.insert(p.cache_key(), Some(f64::NAN));
        assert!(!should_trigger_onchange(
            &state,
            &[p],
            None,
            None,
            &snapshot,
            now
        ));

        let empty: HashMap<String, Option<f64>> = HashMap::new();
        assert!(!should_trigger_onchange(
            &state,
            &[p],
            None,
            None,
            &empty,
            now
        ));

        let mut snap_none: HashMap<String, Option<f64>> = HashMap::new();
        snap_none.insert(p.cache_key(), None);
        assert!(!should_trigger_onchange(
            &state,
            &[p],
            None,
            None,
            &snap_none,
            now
        ));
    }

    #[test]
    fn onchange_recovery_with_no_history_triggers() {
        let p = pref(1, 0);
        let state = OnChangeState::default();
        let snapshot = snap(&[(&p, Some(220.0))]);
        assert!(should_trigger_onchange(
            &state,
            &[p],
            Some(500),
            None,
            &snapshot,
            Instant::now()
        ));
    }

    #[test]
    fn onchange_multi_point_any_change_triggers() {
        let p1 = pref(1, 0);
        let p2 = pref(1, 1);
        let p3 = pref(1, 2);
        let mut state = OnChangeState::default();
        state.last_value.insert(p1.cache_key(), 100.0);
        state.last_value.insert(p2.cache_key(), 200.0);
        state.last_value.insert(p3.cache_key(), 300.0);

        let snapshot = snap(&[(&p1, Some(100.0)), (&p2, Some(200.0)), (&p3, Some(305.0))]);

        assert!(should_trigger_onchange(
            &state,
            &[p1, p2, p3],
            None,
            None,
            &snapshot,
            Instant::now()
        ));
    }

    #[test]
    fn onchange_percent_deadband_from_zero() {
        let p = pref(1, 0);
        let mut state = OnChangeState::default();
        state.last_value.insert(p.cache_key(), 0.0);
        let db = ValueDeadband::Percent { threshold: 5.0 };
        let snapshot = snap(&[(&p, Some(0.01))]);
        assert!(should_trigger_onchange(
            &state,
            &[p],
            None,
            Some(&db),
            &snapshot,
            Instant::now()
        ));
    }
}
