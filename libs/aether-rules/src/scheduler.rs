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

use crate::RuleActionCommandFacade;
use crate::error::Result;
use crate::executor::{RuleExecutionResult, RuleExecutor};
use crate::live_state::RuleLiveState;
use crate::logger::RuleLoggerManager;
use crate::point_watch_dispatcher::MeasurementRouteBinding;
use crate::repository;
use crate::types::Rule;
use aether_calc::MemoryStateStore;
use sqlx::SqlitePool;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{OwnedSemaphorePermit, RwLock, Semaphore};
use tokio::time::interval;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

/// Default scheduler tick interval (100ms)
pub const DEFAULT_TICK_MS: u64 = 100;

/// Default upper bound for one rule evaluation, including governed actions.
pub const DEFAULT_RULE_EXECUTION_TIMEOUT: Duration = Duration::from_secs(5);

/// Local execution-history writes are diagnostic and must not hold the rule
/// flight or scheduler loop indefinitely when SQLite is contended.
const RULE_HISTORY_WRITE_TIMEOUT: Duration = Duration::from_millis(500);

/// A corrupted configuration cannot be represented as an executable trigger.
/// Keeping the fault in the cached rule set makes rule counts and diagnostics
/// honest while every execution path remains fail-closed.
const CORRUPT_TRIGGER_REASON: &str = "stored trigger configuration is invalid";

/// Protects scheduler sampling and PointWatch subscription rebuilds from an
/// unbounded persisted list.
pub const MAX_ONCHANGE_POINT_REFS: usize = 1_024;

/// Composition-independent safety limit for Tokio semaphore construction and
/// for the amount of device-control fan-out one scheduler may create.
pub const MAX_RULE_CONCURRENCY: usize = 64;

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

/// Point kind discriminator (mirrors the T/S/C/A wire representation but stays local
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
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
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

    fn validate(&self) -> Result<()> {
        let threshold = match self {
            Self::Absolute { threshold } | Self::Percent { threshold } => *threshold,
        };
        if !threshold.is_finite() || threshold < 0.0 {
            return Err(crate::RuleError::InvalidFormat(
                "value deadband threshold must be finite and non-negative".to_string(),
            ));
        }
        Ok(())
    }
}

/// Rule trigger configuration
///
/// Supports JSON deserialization for database storage:
/// - `{"type": "interval", "interval_ms": 1000}`
/// - `{"type": "on_change", "point_refs": [...], "time_deadband_ms": 200,
///    "value_deadband": {"type": "absolute", "threshold": 0.5}}`
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
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

impl TriggerConfig {
    /// Validates scheduling semantics after JSON decoding.
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Interval { interval_ms } if *interval_ms == 0 => {
                Err(crate::RuleError::InvalidFormat(
                    "interval_ms must be greater than zero".to_string(),
                ))
            },
            Self::Interval { .. } => Ok(()),
            Self::OnChange {
                point_refs,
                value_deadband,
                ..
            } => {
                if point_refs.is_empty() {
                    return Err(crate::RuleError::InvalidFormat(
                        "on_change point_refs must not be empty".to_string(),
                    ));
                }
                if point_refs.len() > MAX_ONCHANGE_POINT_REFS {
                    return Err(crate::RuleError::InvalidFormat(format!(
                        "on_change point_refs exceeds the limit of {MAX_ONCHANGE_POINT_REFS}"
                    )));
                }
                let mut unique = HashSet::with_capacity(point_refs.len());
                if !point_refs.iter().all(|point| unique.insert(*point)) {
                    return Err(crate::RuleError::InvalidFormat(
                        "on_change point_refs must not contain duplicates".to_string(),
                    ));
                }
                if let Some(deadband) = value_deadband {
                    deadband.validate()?;
                }
                Ok(())
            },
        }
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
    /// Invalid persisted scheduling data is retained for diagnostics but never
    /// reaches tick, PointWatch, or subscription publication.
    trigger_valid: bool,
    last_execution: Option<Instant>,
    /// Track last cooldown trigger time
    last_cooldown_start: Option<Instant>,
    /// OnChange-specific state (last seen values + last trigger time).
    /// Default for Interval rules; populated only for OnChange rules.
    onchange_state: OnChangeState,
    /// A failed OnChange attempt keeps its baseline unchanged but retries only
    /// after this bounded backoff, preventing a 100 ms failure storm.
    retry_not_before: Option<Instant>,
}

/// Resolve a rule's trigger from its stored `trigger_config`.
///
/// Missing and malformed trigger definitions both fail closed. Scheduling has
/// one canonical source and never infers an interval from `cooldown_ms`.
fn resolve_trigger(raw: Option<&str>) -> Result<TriggerConfig> {
    let json = raw.ok_or_else(|| {
        crate::RuleError::InvalidFormat(format!(
            "{CORRUPT_TRIGGER_REASON}: trigger_config is required"
        ))
    })?;
    let trigger: TriggerConfig = serde_json::from_str(json).map_err(|error| {
        crate::RuleError::InvalidFormat(format!("{CORRUPT_TRIGGER_REASON}: {error}"))
    })?;
    trigger.validate()?;
    Ok(trigger)
}

/// Carry per-rule scheduling state from the previous rule set into a freshly
/// loaded one, matched by rule id.
///
/// A reload rebuilds every `ScheduledRule` from the database, which has no
/// notion of cooldowns or last-seen values. Without this the state resets, so
/// editing a single rule makes every rule immediately eligible to fire again.
fn preserve_schedule_state(previous: &[ScheduledRule], loaded: &mut [ScheduledRule]) {
    let mut carried: HashMap<i64, &ScheduledRule> = HashMap::with_capacity(previous.len());
    for entry in previous {
        carried.insert(entry.rule.id, entry);
    }

    for entry in loaded.iter_mut() {
        let Some(prior) = carried.get(&entry.rule.id) else {
            continue;
        };
        if prior.trigger_valid && entry.trigger_valid && prior.trigger == entry.trigger {
            entry.last_execution = prior.last_execution;
            entry.onchange_state = prior.onchange_state.clone();
            entry.retry_not_before = prior.retry_not_before;
        }
        // A successful prior action remains under cooldown even when an
        // operator edits scheduling metadata; reload must not immediately
        // replay a recently accepted command.
        entry.last_cooldown_start = prior.last_cooldown_start;
    }
}

// Allow PointWatchDispatcher::rebuild_from_rules to iterate scheduled rules
// without exposing the private ScheduledRule struct.
impl crate::point_watch_dispatcher::RuleSubscriptionInfo for ScheduledRule {
    fn rule_id(&self) -> i64 {
        self.rule.id
    }
    fn is_enabled(&self) -> bool {
        self.rule.enabled && self.trigger_valid
    }
    fn trigger(&self) -> &TriggerConfig {
        &self.trigger
    }
}

/// Rule Scheduler - manages periodic rule execution
///
/// Stateful calculation memory lives in the in-process [`MemoryStateStore`].
/// Live point values always come from the injected SHM-backed [`RuleLiveState`].
pub struct RuleScheduler {
    live_state: Arc<dyn RuleLiveState>,
    /// Rule executor instance with configurable state store
    executor: Arc<RuleExecutor>,
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
    /// Complete execution deadline for one rule, including governed commands.
    rule_execution_timeout: Duration,
    /// Process-wide execution capacity shared by every scheduler entry path.
    /// Scheduled and PointWatch batches wait inside their existing bounded
    /// streams; manual requests never queue and return a busy error instead.
    execution_slots: Arc<Semaphore>,
    /// Per-rule execution reservations shared by scheduled, PointWatch, and
    /// manual paths. Scheduled/PointWatch overlaps are coalesced, manual
    /// overlaps report a busy error, and unrelated rule IDs remain independent.
    in_flight: Arc<RuleFlights>,
    /// Rotating cursors keep a bounded one-wave cycle fair when more rules are
    /// due than can execute concurrently.
    scheduled_cursor: AtomicUsize,
    watch_cursor: AtomicUsize,
    /// PointWatch fast path: receive events from PointWatchDispatcher.
    /// When present, `start()` selects on this channel alongside the
    /// 100 ms tick and immediately executes matching OnChange rules.
    watch_rx: Option<
        tokio::sync::Mutex<tokio::sync::mpsc::Receiver<crate::point_watch_dispatcher::WatchEvent>>,
    >,
    /// PointWatch dispatcher selected by the service composition. The rule
    /// library returns canonical subscriptions and never owns a concrete SHM
    /// bitmap or manifest.
    pw_dispatcher:
        Option<Arc<std::sync::Mutex<crate::point_watch_dispatcher::PointWatchDispatcher>>>,
}

#[derive(Default)]
struct RuleFlights {
    rule_ids: std::sync::Mutex<HashSet<i64>>,
}

impl RuleFlights {
    fn try_enter(self: &Arc<Self>, rule_id: i64) -> Option<RuleFlightGuard> {
        let mut rule_ids = self
            .rule_ids
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !rule_ids.insert(rule_id) {
            return None;
        }
        Some(RuleFlightGuard {
            rule_id,
            flights: Arc::clone(self),
        })
    }
}

struct RuleFlightGuard {
    rule_id: i64,
    flights: Arc<RuleFlights>,
}

struct ExecutionReservation {
    _rule: RuleFlightGuard,
    _slot: OwnedSemaphorePermit,
}

impl ExecutionReservation {
    /// Release process capacity as soon as rule execution finishes while
    /// retaining the per-rule reservation through logging and state updates.
    fn finish_execution(self) -> RuleFlightGuard {
        let Self { _rule, _slot } = self;
        drop(_slot);
        _rule
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReservationBusy {
    Rule,
    Capacity,
}

impl Drop for RuleFlightGuard {
    fn drop(&mut self) {
        self.flights
            .rule_ids
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.rule_id);
    }
}

const fn normalized_tick_ms(tick_ms: u64) -> u64 {
    if tick_ms == 0 {
        DEFAULT_TICK_MS
    } else {
        tick_ms
    }
}

fn bounded_fair_batch<T>(mut due: Vec<T>, cursor: &AtomicUsize, limit: usize) -> Vec<T> {
    if due.len() <= limit {
        return due;
    }
    let start = cursor.fetch_add(limit, Ordering::Relaxed) % due.len();
    due.rotate_left(start);
    due.truncate(limit);
    due
}

impl RuleScheduler {
    /// Create a new rule scheduler with configurable tick interval (uses MemoryStateStore)
    ///
    /// # Arguments
    /// * `live_state` - authoritative live-state reader
    /// * `pool` - SQLite pool for rule persistence
    /// * `tick_ms` - Scheduler tick interval in milliseconds
    /// * `log_root` - Root directory for rule log files (e.g., "logs/automation")
    pub fn new<L>(live_state: Arc<L>, pool: SqlitePool, tick_ms: u64, log_root: PathBuf) -> Self
    where
        L: RuleLiveState + 'static,
    {
        Self {
            live_state: Arc::clone(&live_state) as Arc<dyn RuleLiveState>,
            executor: Arc::new(RuleExecutor::new(live_state)),
            pool,
            rules: Arc::new(RwLock::new(Vec::new())),
            shutdown: CancellationToken::new(),
            tick_ms: normalized_tick_ms(tick_ms),
            logger_manager: RuleLoggerManager::new(log_root),
            max_concurrency: 4,
            rule_execution_timeout: DEFAULT_RULE_EXECUTION_TIMEOUT,
            execution_slots: Arc::new(Semaphore::new(4)),
            in_flight: Arc::new(RuleFlights::default()),
            scheduled_cursor: AtomicUsize::new(0),
            watch_cursor: AtomicUsize::new(0),
            watch_rx: None,
            pw_dispatcher: None,
        }
    }

    /// Create with an externally owned state store and full SHM support
    ///
    /// Use this constructor when the composition root owns the state store.
    #[allow(clippy::too_many_arguments)]
    pub fn with_state_store<L>(
        live_state: Arc<L>,
        pool: SqlitePool,
        tick_ms: u64,
        log_root: PathBuf,
        state_store: Arc<MemoryStateStore>,
        action_commands: Option<Arc<dyn RuleActionCommandFacade>>,
    ) -> Self
    where
        L: RuleLiveState + 'static,
    {
        let mut executor = RuleExecutor::with_state_store(Arc::clone(&live_state), state_store);
        if let Some(commands) = action_commands {
            executor = executor.with_action_command_facade(commands);
        }
        Self {
            live_state,
            executor: Arc::new(executor),
            pool,
            rules: Arc::new(RwLock::new(Vec::new())),
            shutdown: CancellationToken::new(),
            tick_ms: normalized_tick_ms(tick_ms),
            logger_manager: RuleLoggerManager::new(log_root),
            max_concurrency: 4,
            rule_execution_timeout: DEFAULT_RULE_EXECUTION_TIMEOUT,
            execution_slots: Arc::new(Semaphore::new(4)),
            in_flight: Arc::new(RuleFlights::default()),
            scheduled_cursor: AtomicUsize::new(0),
            watch_cursor: AtomicUsize::new(0),
            watch_rx: None,
            pw_dispatcher: None,
        }
    }

    /// Set maximum concurrent rule executions (must be called before wrapping in Arc)
    pub fn set_max_concurrency(&mut self, n: usize) -> Result<()> {
        if !(1..=MAX_RULE_CONCURRENCY).contains(&n) {
            return Err(crate::RuleError::InvalidFormat(format!(
                "max_concurrency must be in 1..={MAX_RULE_CONCURRENCY}"
            )));
        }
        self.max_concurrency = n;
        self.execution_slots = Arc::new(Semaphore::new(n));
        Ok(())
    }

    /// Configures the complete deadline for one rule, including governed
    /// commands, before the scheduler is shared.
    pub fn set_execution_timeout(&mut self, rule: Duration) -> Result<()> {
        if rule.is_zero() {
            return Err(crate::RuleError::InvalidFormat(
                "rule execution timeout must be non-zero".to_string(),
            ));
        }
        self.rule_execution_timeout = rule;
        Ok(())
    }

    fn try_reserve_execution(
        &self,
        rule_id: i64,
    ) -> std::result::Result<ExecutionReservation, ReservationBusy> {
        let rule = self
            .in_flight
            .try_enter(rule_id)
            .ok_or(ReservationBusy::Rule)?;
        let slot = Arc::clone(&self.execution_slots)
            .try_acquire_owned()
            .map_err(|_| ReservationBusy::Capacity)?;
        Ok(ExecutionReservation {
            _rule: rule,
            _slot: slot,
        })
    }

    async fn reserve_background_execution(
        &self,
        rule_id: i64,
    ) -> std::result::Result<ExecutionReservation, ReservationBusy> {
        let rule = self
            .in_flight
            .try_enter(rule_id)
            .ok_or(ReservationBusy::Rule)?;
        let slot = Arc::clone(&self.execution_slots)
            .acquire_owned()
            .await
            .map_err(|_| ReservationBusy::Capacity)?;
        Ok(ExecutionReservation {
            _rule: rule,
            _slot: slot,
        })
    }

    /// Rebuild the `PointWatchDispatcher` subscription index from the currently
    /// loaded rules. Call this after `load_rules` / `reload_rules` to keep the
    /// event-driven path in sync.
    pub async fn rebuild_point_watch(
        &self,
        measurement_routes: &[MeasurementRouteBinding],
    ) -> Option<Vec<aether_domain::ChannelPointAddress>> {
        let dispatcher = self.pw_dispatcher.as_ref()?;
        let rules = self.rules.read().await;
        let mut dispatcher = dispatcher
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Some(dispatcher.rebuild_from_rules(&rules, measurement_routes))
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

    /// Stores the dispatcher required to rebuild the PointWatch subscription
    /// index. The service composition remains responsible for publishing the
    /// returned channel-point set to its concrete event adapter.
    pub fn set_point_watch_rebuild_handle(
        &mut self,
        dispatcher: Arc<std::sync::Mutex<crate::point_watch_dispatcher::PointWatchDispatcher>>,
    ) {
        self.pw_dispatcher = Some(dispatcher);
    }

    /// Load rules from database and initialize scheduler state
    pub async fn load_rules(&self) -> Result<usize> {
        let db_rules = repository::load_enabled_rules(&self.pool).await?;
        let count = db_rules.len();

        let mut scheduled: Vec<ScheduledRule> = db_rules
            .into_iter()
            .map(|rule| {
                let (trigger, trigger_valid) = match resolve_trigger(rule.trigger_config.as_deref())
                {
                    Ok(trigger) => (trigger, true),
                    Err(error) => {
                        error!(
                            rule_id = rule.id,
                            rule_name = %rule.name,
                            %error,
                            "Rule trigger configuration is invalid; execution is disabled"
                        );
                        (TriggerConfig::default(), false)
                    },
                };

                ScheduledRule {
                    rule: Arc::new(rule),
                    trigger,
                    trigger_valid,
                    last_execution: None,
                    last_cooldown_start: None,
                    onchange_state: OnChangeState::default(),
                    retry_not_before: None,
                }
            })
            .collect();

        let mut rules = self.rules.write().await;
        preserve_schedule_state(&rules, &mut scheduled);
        *rules = scheduled;

        info!("Rules: {} loaded", count);
        Ok(count)
    }

    /// Reload rules from database (hot reload).
    ///
    /// PointWatch publication is intentionally separate: the host must pin one
    /// complete topology and pass its routes and manifest to
    /// [`Self::rebuild_point_watch`] after this reload succeeds.
    pub async fn reload_rules(&self) -> Result<usize> {
        info!("Rules reloading");
        self.load_rules().await
    }

    /// Start the scheduler loop
    pub async fn start(&self) {
        if self.shutdown.is_cancelled() {
            warn!("Scheduler already stopped");
            return;
        }

        info!("Scheduler start ({}ms)", self.tick_ms);

        let mut tick_interval = interval(Duration::from_millis(self.tick_ms));
        tick_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

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
                .filter(|s| s.rule.enabled && s.trigger_valid)
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
                    if !scheduled.rule.enabled || !scheduled.trigger_valid {
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
                    let retry_ok = scheduled
                        .retry_not_before
                        .is_none_or(|deadline| now >= deadline);

                    if should_execute && cooldown_ok && retry_ok {
                        Some((idx, Arc::clone(&scheduled.rule), is_onchange))
                    } else {
                        None
                    }
                })
                .collect()
        }; // Read lock released here (~10μs)
        // One cycle starts at most one process-capacity wave. This bounds
        // PointWatch latency and shutdown drain without cancelling a batch
        // after a command may already have crossed the transport edge.
        let rules_to_execute = bounded_fair_batch(
            rules_to_execute,
            &self.scheduled_cursor,
            self.max_concurrency,
        );

        if rules_to_execute.is_empty() {
            return Ok(());
        }

        // Phase 2: Execute rules in parallel without holding any lock
        // Use buffer_unordered for concurrent execution with bounded parallelism
        use futures::stream::{self, StreamExt};

        struct ExecutionOutcome {
            _rule: RuleFlightGuard,
            idx: usize,
            rule_id: i64,
            rule_name: String,
            is_onchange: bool,
            result: Result<RuleExecutionResult>,
        }

        // Execute rules concurrently (max self.max_concurrency parallel)
        let executor = Arc::clone(&self.executor);
        let execution_timeout = self.rule_execution_timeout;
        let execution_futures = rules_to_execute
            .into_iter()
            .map(|(idx, rule, is_onchange)| {
                let executor = Arc::clone(&executor);
                async move {
                    if self.shutdown.is_cancelled() {
                        return None;
                    }
                    let reservation = match self.reserve_background_execution(rule.id).await {
                        Ok(reservation) => reservation,
                        Err(ReservationBusy::Rule) => {
                            debug!(
                                rule_id = rule.id,
                                "Coalesced scheduled trigger while the rule is already executing"
                            );
                            return None;
                        },
                        Err(ReservationBusy::Capacity) => {
                            error!(
                                rule_id = rule.id,
                                "Rule execution semaphore closed unexpectedly"
                            );
                            return None;
                        },
                    };
                    debug!("Executing rule: {}", rule.id);
                    let rule_id = rule.id;
                    let rule_name = rule.name.clone();
                    let result = execute_with_timeout(&executor, &rule, execution_timeout).await;
                    Some(ExecutionOutcome {
                        _rule: reservation.finish_execution(),
                        idx,
                        rule_id,
                        rule_name,
                        is_onchange,
                        result,
                    })
                }
            });

        let execution_results: Vec<ExecutionOutcome> = stream::iter(execution_futures)
            .buffer_unordered(self.max_concurrency)
            .filter_map(std::future::ready)
            .collect()
            .await;

        // Process results sequentially (logging and local history writes)
        struct TimestampUpdate {
            /// Keep the rule reserved until trigger/cooldown state reflects
            /// the completed execution.
            _rule: RuleFlightGuard,
            idx: usize,
            rule_id: i64,
            start_cooldown: bool,
            is_onchange: bool,
            /// Once a device action may have been accepted, advance the trigger
            /// baseline even when the command result is a failure. The current
            /// UDS wire has no end-to-end deduplication identity, so replaying
            /// an ambiguous command would be less safe than recording the
            /// attempted trigger.
            advance_onchange: bool,
            /// Per-point values surfaced by the executor (SHM-first read).
            /// Used to advance OnChange last_value against what executor
            /// actually saw from SHM.
            executor_point_values: Arc<HashMap<String, f64>>,
        }
        let mut updates: Vec<TimestampUpdate> = Vec::with_capacity(execution_results.len());
        let mut history_results = Vec::with_capacity(execution_results.len());

        for outcome in execution_results {
            match outcome.result {
                Ok(result) => {
                    // Log rule execution to independent rule log file
                    let logger = self
                        .logger_manager
                        .get_logger(outcome.rule_id, &outcome.rule_name);
                    logger.log_execution(&result, &result.variable_values);

                    history_results.push((outcome.rule_id, result.clone()));

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
                    let advance_onchange = result
                        .actions_executed
                        .iter()
                        .any(crate::ActionResult::delivery_possible);
                    updates.push(TimestampUpdate {
                        _rule: outcome._rule,
                        idx: outcome.idx,
                        rule_id: outcome.rule_id,
                        start_cooldown,
                        is_onchange: outcome.is_onchange,
                        advance_onchange,
                        executor_point_values,
                    });
                },
                Err(e) => {
                    error!("Rule {} err: {}", outcome.rule_id, e);
                    // Still update last_execution to prevent retry spam
                    updates.push(TimestampUpdate {
                        _rule: outcome._rule,
                        idx: outcome.idx,
                        rule_id: outcome.rule_id,
                        start_cooldown: false,
                        is_onchange: outcome.is_onchange,
                        // The only executor-level error today is the outer
                        // deadline. Cancellation may have interrupted an
                        // in-flight command after local transport acceptance,
                        // so retrying automatically would risk a duplicate.
                        advance_onchange: outcome.is_onchange,
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
                    scheduled.retry_not_before =
                        (!update.advance_onchange).then_some(now + Duration::from_secs(1));
                    if update.start_cooldown {
                        scheduled.last_cooldown_start = Some(now);
                    }
                    // For OnChange rules, advance per-point last_value to the
                    // values we just sampled in Phase 0. This is what gives
                    // the deadband its memory: future ticks compare against
                    // the value at the moment of this trigger, not the
                    // ever-changing latest sample.
                    if update.is_onchange
                        && update.advance_onchange
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

        // Control state is committed in memory before best-effort diagnostics.
        // The entire history wave shares one deadline, so N contended writes
        // cannot turn a 500 ms policy into N x 500 ms scheduler latency.
        persist_rule_execution_batch(&self.pool, history_results, RULE_HISTORY_WRITE_TIMEOUT).await;

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
        let (point_watch_configured, point_watch_subscriptions, point_watch_dropped_events) = self
            .pw_dispatcher
            .as_ref()
            .map(|dispatcher| {
                let dispatcher = dispatcher
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                (
                    true,
                    dispatcher.subscription_count(),
                    dispatcher.dropped_count(),
                )
            })
            .unwrap_or((false, 0, 0));

        SchedulerStatus {
            running: self.is_running(),
            total_rules: rules.len(),
            enabled_rules: enabled_count,
            invalid_enabled_rules: rules
                .iter()
                .filter(|rule| rule.rule.enabled && !rule.trigger_valid)
                .count(),
            tick_interval_ms: self.tick_ms,
            point_watch_configured,
            point_watch_subscriptions,
            point_watch_dropped_events,
            rule_log: self.logger_manager.stats(),
        }
    }

    /// Drains the bounded rule-log worker. Call from `spawn_blocking` after all
    /// scheduler entry paths have stopped producing records.
    pub fn shutdown_rule_logging(&self) -> std::io::Result<()> {
        self.logger_manager.shutdown_blocking()
    }

    /// Execute a specific rule by ID (manual trigger).
    ///
    /// Manual requests preserve their synchronous acceptance semantics: if the
    /// same rule is already running, the request receives a scheduler-busy
    /// error instead of silently claiming that another execution was its own.
    pub async fn execute_rule(&self, rule_id: i64) -> Result<RuleExecutionResult> {
        // Load the rule from database
        let rule = repository::get_rule_for_execution(&self.pool, rule_id).await?;

        let _reservation = self
            .try_reserve_execution(rule_id)
            .map_err(|busy| match busy {
                ReservationBusy::Rule => crate::RuleError::SchedulerError(format!(
                    "rule {rule_id} is already executing; overlapping manual execution was rejected"
                )),
                ReservationBusy::Capacity => crate::RuleError::SchedulerError(format!(
                    "rule execution capacity is busy (max_concurrency={}); manual execution of rule {rule_id} was rejected",
                    self.max_concurrency
                )),
            })?;

        // Manual execution validates the persisted trigger as well. A damaged
        // rule cannot bypass scheduler fail-closed behavior via the API.
        resolve_trigger(rule.trigger_config.as_deref())?;
        execute_with_timeout(&self.executor, &rule, self.rule_execution_timeout).await
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
    /// Does NOT remove the 100 ms fallback. The paths may run in parallel for
    /// different rules; same-rule overlap is coalesced. The tick path provides
    /// the fallback when the UDS is down or hints drop.
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
                    scheduled.rule.enabled
                        && scheduled.trigger_valid
                        && rule_id_set.contains(&scheduled.rule.id)
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
                    if !scheduled.rule.enabled
                        || !scheduled.trigger_valid
                        || !rule_id_set.contains(&scheduled.rule.id)
                    {
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
                    let retry_ok = scheduled
                        .retry_not_before
                        .is_none_or(|deadline| now >= deadline);
                    (changed && cooldown_ok && retry_ok).then(|| (idx, Arc::clone(&scheduled.rule)))
                })
                .collect()
        };
        let to_execute = bounded_fair_batch(to_execute, &self.watch_cursor, self.max_concurrency);

        if to_execute.is_empty() {
            return Ok(());
        }

        // Execute the matching rules
        use futures::stream::{self, StreamExt};
        let executor = Arc::clone(&self.executor);
        let execution_timeout = self.rule_execution_timeout;
        let results: Vec<(
            RuleFlightGuard,
            usize,
            i64,
            String,
            Result<RuleExecutionResult>,
        )> = stream::iter(to_execute.into_iter().map(|(idx, rule)| {
            let executor = Arc::clone(&executor);
            async move {
                if self.shutdown.is_cancelled() {
                    return None;
                }
                let reservation = match self.reserve_background_execution(rule.id).await {
                    Ok(reservation) => reservation,
                    Err(ReservationBusy::Rule) => {
                        debug!(
                            rule_id = rule.id,
                            "Coalesced PointWatch trigger while the rule is already executing"
                        );
                        return None;
                    },
                    Err(ReservationBusy::Capacity) => {
                        error!(
                            rule_id = rule.id,
                            "Rule execution semaphore closed unexpectedly"
                        );
                        return None;
                    },
                };
                let id = rule.id;
                let name = rule.name.clone();
                let result = execute_with_timeout(&executor, &rule, execution_timeout).await;
                Some((reservation.finish_execution(), idx, id, name, result))
            }
        }))
        .buffer_unordered(self.max_concurrency)
        .filter_map(std::future::ready)
        .collect()
        .await;

        struct WatchUpdate {
            /// Keep the rule reserved until OnChange/cooldown state reflects
            /// the completed execution.
            _rule: RuleFlightGuard,
            idx: usize,
            rule_id: i64,
            start_cooldown: bool,
            advance_onchange: bool,
            point_values: Arc<HashMap<String, f64>>,
        }
        let mut updates = Vec::with_capacity(results.len());
        let mut history_results = Vec::with_capacity(results.len());
        for (rule_reservation, idx, rule_id, rule_name, result) in results {
            match result {
                Ok(exec_result) => {
                    let logger = self.logger_manager.get_logger(rule_id, &rule_name);
                    logger.log_execution(&exec_result, &exec_result.variable_values);
                    history_results.push((rule_id, exec_result.clone()));
                    updates.push(WatchUpdate {
                        _rule: rule_reservation,
                        idx,
                        rule_id,
                        start_cooldown: exec_result.success
                            && !exec_result.actions_executed.is_empty(),
                        advance_onchange: exec_result
                            .actions_executed
                            .iter()
                            .any(crate::ActionResult::delivery_possible),
                        point_values: Arc::clone(&exec_result.point_values),
                    });
                },
                Err(error) => {
                    error!("Watch-triggered rule {} err: {}", rule_id, error);
                    updates.push(WatchUpdate {
                        _rule: rule_reservation,
                        idx,
                        rule_id,
                        start_cooldown: false,
                        // See the scheduled-path deadline note above: an
                        // automatic retry is unsafe after ambiguous delivery.
                        advance_onchange: true,
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
                scheduled.retry_not_before =
                    (!update.advance_onchange).then_some(now + Duration::from_secs(1));
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

        persist_rule_execution_batch(&self.pool, history_results, RULE_HISTORY_WRITE_TIMEOUT).await;

        Ok(())
    }
}

#[async_trait::async_trait]
impl aether_ports::AutomationRuleExecutor for RuleScheduler {
    async fn execute(
        &self,
        rule_id: aether_domain::RuleId,
    ) -> aether_ports::PortResult<aether_ports::RuleExecutionReceipt> {
        let database_id = i64::try_from(rule_id.get()).map_err(|_| {
            aether_ports::PortError::new(
                aether_ports::PortErrorKind::InvalidData,
                format!("rule id {} exceeds the supported range", rule_id.get()),
            )
        })?;
        let result = self
            .execute_rule(database_id)
            .await
            .map_err(rule_execution_port_error)?;
        let attempted = u32::try_from(result.actions_executed.len()).map_err(|_| {
            aether_ports::PortError::new(
                aether_ports::PortErrorKind::InvalidData,
                "rule result contains too many action outcomes",
            )
        })?;
        let succeeded = u32::try_from(
            result
                .actions_executed
                .iter()
                .filter(|action| action.success)
                .count(),
        )
        .map_err(|_| {
            aether_ports::PortError::new(
                aether_ports::PortErrorKind::InvalidData,
                "rule result contains too many successful action outcomes",
            )
        })?;
        if !result.success {
            return Err(aether_ports::PortError::new(
                aether_ports::PortErrorKind::Rejected,
                result
                    .error
                    .unwrap_or_else(|| "rule execution did not complete successfully".to_string()),
            ));
        }
        let completed_at = chrono::Utc::now().timestamp_millis().max(0) as u64;
        Ok(aether_ports::RuleExecutionReceipt::new(
            rule_id,
            aether_domain::TimestampMs::new(completed_at),
            attempted,
            succeeded,
        ))
    }
}

fn rule_execution_port_error(error: crate::RuleError) -> aether_ports::PortError {
    use aether_ports::{PortError, PortErrorKind};

    let kind = match error {
        crate::RuleError::NotFound(_)
        | crate::RuleError::InvalidFormat(_)
        | crate::RuleError::ParseError(_)
        | crate::RuleError::SerializationError(_) => PortErrorKind::InvalidData,
        crate::RuleError::AlreadyExists(_) => PortErrorKind::Conflict,
        crate::RuleError::ExecutionError(_)
        | crate::RuleError::ConditionError(_)
        | crate::RuleError::ActionError(_)
        | crate::RuleError::RoutingError(_) => PortErrorKind::Rejected,
        crate::RuleError::DatabaseError(_) | crate::RuleError::SchedulerError(_) => {
            PortErrorKind::Unavailable
        },
    };
    PortError::new(kind, error.to_string())
}

async fn execute_with_timeout(
    executor: &RuleExecutor,
    rule: &Rule,
    deadline: Duration,
) -> Result<RuleExecutionResult> {
    tokio::time::timeout(deadline, executor.execute(rule))
        .await
        .map_err(|_| {
            crate::RuleError::ExecutionError(format!(
                "rule {} execution exceeded {} ms",
                rule.id,
                deadline.as_millis()
            ))
        })?
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

async fn persist_rule_execution_batch(
    pool: &SqlitePool,
    results: Vec<(i64, RuleExecutionResult)>,
    deadline: Duration,
) {
    if results.is_empty() {
        return;
    }
    use futures::stream::{self, StreamExt};

    let count = results.len();
    let writes =
        stream::iter(results).for_each_concurrent(Some(8), |(rule_id, result)| async move {
            if let Err(error) = persist_rule_execution(pool, &result).await {
                warn!(rule_id, %error, "failed to persist rule execution history");
            }
        });
    if tokio::time::timeout(deadline, writes).await.is_err() {
        warn!(
            count,
            timeout_ms = deadline.as_millis(),
            "Rule execution history batch exceeded its total deadline; unfinished diagnostics were dropped"
        );
    }
}

/// Scheduler status information
#[derive(Debug, Clone)]
pub struct SchedulerStatus {
    pub running: bool,
    pub total_rules: usize,
    pub enabled_rules: usize,
    pub invalid_enabled_rules: usize,
    pub tick_interval_ms: u64,
    pub point_watch_configured: bool,
    pub point_watch_subscriptions: usize,
    pub point_watch_dropped_events: u64,
    pub rule_log: crate::logger::RuleLogStats,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{RuleFlow, RuleNode, RuleValueAssignment, RuleVariable, RuleWires};
    use crate::{RuleActionCommand, RuleActionCommandFacade};
    use aether_domain::{CommandId, TimestampMs};
    use aether_ports::{CommandReceipt, PortError, PortErrorKind, PortResult};
    use async_trait::async_trait;
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Notify;

    struct FailingActionCommands;

    #[async_trait]
    impl RuleActionCommandFacade for FailingActionCommands {
        async fn write_action(&self, _command: RuleActionCommand) -> PortResult<CommandReceipt> {
            Err(PortError::new(
                PortErrorKind::Unavailable,
                "simulated governed action failure",
            ))
        }
    }

    struct CountingFailingActionCommands {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl RuleActionCommandFacade for CountingFailingActionCommands {
        async fn write_action(&self, _command: RuleActionCommand) -> PortResult<CommandReceipt> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(PortError::new(
                PortErrorKind::Unavailable,
                "simulated lost/failed command receipt",
            ))
        }
    }

    struct BlockingActionCommands {
        calls: AtomicUsize,
        entered: Notify,
        release: Notify,
    }

    impl BlockingActionCommands {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
                entered: Notify::new(),
                release: Notify::new(),
            }
        }
    }

    #[async_trait]
    impl RuleActionCommandFacade for BlockingActionCommands {
        async fn write_action(&self, _command: RuleActionCommand) -> PortResult<CommandReceipt> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            self.entered.notify_one();
            self.release.notified().await;
            Ok(CommandReceipt::new(
                CommandId::new(call as u128),
                TimestampMs::new(1),
            ))
        }
    }

    #[test]
    fn test_trigger_config_default() {
        let TriggerConfig::Interval { interval_ms } = TriggerConfig::default() else {
            panic!("Default should be Interval");
        };
        assert_eq!(interval_ms, 1000);
    }

    #[test]
    fn single_flight_is_per_rule_and_releases_with_its_guard() {
        let flights = Arc::new(RuleFlights::default());
        let first = flights.try_enter(7).expect("first rule-7 execution");
        assert!(
            flights.try_enter(7).is_none(),
            "the same rule must not overlap"
        );
        let other = flights
            .try_enter(8)
            .expect("a different rule must remain independent");

        drop(first);
        assert!(
            flights.try_enter(7).is_some(),
            "dropping the execution guard makes the rule eligible again"
        );
        drop(other);
    }

    #[test]
    fn bounded_cycle_is_one_wave_and_rotates_fairly() {
        let cursor = AtomicUsize::new(0);
        let due = vec![0, 1, 2, 3, 4];

        assert_eq!(bounded_fair_batch(due.clone(), &cursor, 2), [0, 1]);
        assert_eq!(bounded_fair_batch(due.clone(), &cursor, 2), [2, 3]);
        assert_eq!(bounded_fair_batch(due, &cursor, 2), [4, 0]);
    }

    #[tokio::test]
    async fn zero_tick_uses_the_safe_default_interval() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open scheduler database");
        let logs = tempfile::tempdir().expect("temporary rule logs");
        let scheduler = RuleScheduler::new(
            Arc::new(crate::MemoryRuleLiveState::new()),
            pool,
            0,
            logs.path().to_path_buf(),
        );

        assert_eq!(scheduler.status().await.tick_interval_ms, DEFAULT_TICK_MS);
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

    fn create_action_rule(id: i64, cooldown_ms: u64) -> Rule {
        let nodes = HashMap::from([
            (
                "start".to_string(),
                RuleNode::Start {
                    wires: RuleWires {
                        default: vec!["action".to_string()],
                    },
                },
            ),
            (
                "action".to_string(),
                RuleNode::ChangeValue {
                    variables: vec![RuleVariable {
                        name: "TARGET".to_string(),
                        instance: Some(42),
                        point_type: Some("action".to_string()),
                        point: Some(7),
                        formula: Vec::new(),
                    }],
                    rule: vec![RuleValueAssignment {
                        variables: "TARGET".to_string(),
                        value: json!(1.0),
                    }],
                    wires: RuleWires {
                        default: vec!["end".to_string()],
                    },
                },
            ),
            ("end".to_string(), RuleNode::End),
        ]);
        Rule {
            id,
            name: format!("action-{id}"),
            description: None,
            enabled: true,
            priority: 100,
            cooldown_ms,
            trigger_config: Some(r#"{"type":"interval","interval_ms":1}"#.to_string()),
            flow: RuleFlow {
                start_node: "start".to_string(),
                nodes,
            },
        }
    }

    fn create_predispatch_skipped_action_rule(id: i64) -> Rule {
        let mut rule = create_action_rule(id, 0);
        let RuleNode::ChangeValue { rule: actions, .. } =
            rule.flow.nodes.get_mut("action").expect("action node")
        else {
            panic!("test rule action node must be ChangeValue");
        };
        actions[0].value = json!("UNKNOWN_ASSIGNMENT_VALUE");
        rule
    }

    async fn rule_pool(rule: &Rule) -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open scheduler database");
        sqlx::query(
            "CREATE TABLE rules (\
                 id INTEGER PRIMARY KEY,\
                 name TEXT NOT NULL,\
                 description TEXT,\
                 enabled INTEGER NOT NULL,\
                 priority INTEGER NOT NULL,\
                 cooldown_ms INTEGER NOT NULL,\
                 trigger_config TEXT,\
                 nodes_json TEXT NOT NULL\
             )",
        )
        .execute(&pool)
        .await
        .expect("create rules table");
        insert_rule(&pool, rule).await;
        pool
    }

    async fn insert_rule(pool: &SqlitePool, rule: &Rule) {
        sqlx::query(
            "INSERT INTO rules \
             (id, name, description, enabled, priority, cooldown_ms, trigger_config, nodes_json) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(rule.id)
        .bind(&rule.name)
        .bind(&rule.description)
        .bind(i64::from(rule.enabled))
        .bind(i64::from(rule.priority))
        .bind(i64::try_from(rule.cooldown_ms).expect("test cooldown fits i64"))
        .bind(&rule.trigger_config)
        .bind(serde_json::to_string(&rule.flow).expect("serialize test flow"))
        .execute(pool)
        .await
        .expect("insert action rule");
    }

    fn failing_action_scheduler(pool: SqlitePool, log_root: PathBuf) -> RuleScheduler {
        RuleScheduler::with_state_store(
            Arc::new(crate::MemoryRuleLiveState::new()),
            pool,
            1,
            log_root,
            Arc::new(aether_calc::MemoryStateStore::new()),
            Some(Arc::new(FailingActionCommands)),
        )
    }

    #[tokio::test]
    async fn application_rule_port_rejects_failed_action_execution() {
        use aether_domain::RuleId;
        use aether_ports::AutomationRuleExecutor;

        let rule = create_action_rule(31, 1_000);
        let pool = rule_pool(&rule).await;
        let logs = tempfile::tempdir().expect("temporary rule logs");
        let scheduler = failing_action_scheduler(pool, logs.path().to_path_buf());

        let error = AutomationRuleExecutor::execute(&scheduler, RuleId::new(31))
            .await
            .expect_err("failed device action must fail manual rule execution");

        assert_eq!(error.kind(), PortErrorKind::Rejected);
        assert!(
            error
                .message()
                .contains("1 of 1 attempted rule actions failed")
        );
    }

    #[tokio::test]
    async fn failed_scheduled_action_does_not_start_cooldown() {
        let rule = create_action_rule(32, 60_000);
        let pool = rule_pool(&rule).await;
        let logs = tempfile::tempdir().expect("temporary rule logs");
        let scheduler = failing_action_scheduler(pool, logs.path().to_path_buf());
        scheduler.rules.write().await.push(ScheduledRule {
            rule: Arc::new(rule),
            trigger: TriggerConfig::Interval { interval_ms: 1 },
            last_execution: None,
            last_cooldown_start: None,
            onchange_state: OnChangeState::default(),
            trigger_valid: true,
            retry_not_before: None,
        });

        scheduler.tick().await.expect("run scheduler tick");

        let rules = scheduler.rules.read().await;
        assert!(rules[0].last_execution.is_some());
        assert!(
            rules[0].last_cooldown_start.is_none(),
            "failed action must remain eligible for retry after its interval"
        );
    }

    #[tokio::test]
    async fn onchange_predispatch_skip_preserves_baseline_and_applies_backoff() {
        let rule = create_predispatch_skipped_action_rule(320);
        let pool = rule_pool(&rule).await;
        let live_state = Arc::new(crate::MemoryRuleLiveState::new());
        assert!(live_state.set_instance(1, 0, 0, 10.0, 1));
        let logs = tempfile::tempdir().expect("temporary rule logs");
        let scheduler = RuleScheduler::with_state_store(
            live_state,
            pool,
            1,
            logs.path().to_path_buf(),
            Arc::new(aether_calc::MemoryStateStore::new()),
            None,
        );
        let point = pref(1, 0);
        scheduler.rules.write().await.push(ScheduledRule {
            rule: Arc::new(rule),
            trigger: TriggerConfig::OnChange {
                point_refs: vec![point],
                time_deadband_ms: None,
                value_deadband: None,
            },
            trigger_valid: true,
            last_execution: None,
            last_cooldown_start: None,
            onchange_state: OnChangeState::default(),
            retry_not_before: None,
        });

        scheduler.tick().await.expect("evaluate skipped action");

        let rules = scheduler.rules.read().await;
        assert!(rules[0].last_execution.is_some());
        assert!(
            rules[0].onchange_state.last_value.is_empty(),
            "a skip before the command facade must not consume the input transition"
        );
        assert!(
            rules[0].onchange_state.last_trigger.is_none(),
            "a pre-dispatch skip is not an OnChange delivery attempt"
        );
        assert!(
            rules[0]
                .retry_not_before
                .is_some_and(|retry| retry > Instant::now()),
            "pre-dispatch failures get bounded retry backoff"
        );
    }

    #[tokio::test]
    async fn onchange_failed_receipt_advances_baseline_and_is_not_replayed() {
        let rule = create_action_rule(321, 0);
        let pool = rule_pool(&rule).await;
        let live_state = Arc::new(crate::MemoryRuleLiveState::new());
        assert!(live_state.set_instance(1, 0, 0, 10.0, 1));
        let commands = Arc::new(CountingFailingActionCommands {
            calls: AtomicUsize::new(0),
        });
        let command_facade: Arc<dyn RuleActionCommandFacade> = commands.clone();
        let logs = tempfile::tempdir().expect("temporary rule logs");
        let scheduler = RuleScheduler::with_state_store(
            live_state,
            pool,
            1,
            logs.path().to_path_buf(),
            Arc::new(aether_calc::MemoryStateStore::new()),
            Some(command_facade),
        );
        let point = pref(1, 0);
        scheduler.rules.write().await.push(ScheduledRule {
            rule: Arc::new(rule),
            trigger: TriggerConfig::OnChange {
                point_refs: vec![point],
                time_deadband_ms: None,
                value_deadband: None,
            },
            trigger_valid: true,
            last_execution: None,
            last_cooldown_start: None,
            onchange_state: OnChangeState::default(),
            retry_not_before: None,
        });

        scheduler.tick().await.expect("first delivery attempt");
        scheduler.tick().await.expect("same-value follow-up tick");

        assert_eq!(
            commands.calls.load(Ordering::SeqCst),
            1,
            "a failed/ambiguous command receipt must not replay the same transition"
        );
        let rules = scheduler.rules.read().await;
        assert_eq!(rules[0].onchange_state.last_value[&point.cache_key()], 10.0);
        assert!(rules[0].onchange_state.last_trigger.is_some());
        assert!(rules[0].retry_not_before.is_none());
    }

    #[tokio::test]
    async fn onchange_command_deadline_advances_baseline_and_is_not_replayed() {
        let rule = create_action_rule(322, 0);
        let pool = rule_pool(&rule).await;
        let live_state = Arc::new(crate::MemoryRuleLiveState::new());
        assert!(live_state.set_instance(1, 0, 0, 10.0, 1));
        let commands = Arc::new(BlockingActionCommands::new());
        let command_facade: Arc<dyn RuleActionCommandFacade> = commands.clone();
        let logs = tempfile::tempdir().expect("temporary rule logs");
        let mut scheduler = RuleScheduler::with_state_store(
            live_state,
            pool,
            1,
            logs.path().to_path_buf(),
            Arc::new(aether_calc::MemoryStateStore::new()),
            Some(command_facade),
        );
        scheduler
            .set_execution_timeout(Duration::from_millis(10))
            .expect("valid test timeout");
        let point = pref(1, 0);
        scheduler.rules.write().await.push(ScheduledRule {
            rule: Arc::new(rule),
            trigger: TriggerConfig::OnChange {
                point_refs: vec![point],
                time_deadband_ms: None,
                value_deadband: None,
            },
            trigger_valid: true,
            last_execution: None,
            last_cooldown_start: None,
            onchange_state: OnChangeState::default(),
            retry_not_before: None,
        });

        scheduler
            .tick()
            .await
            .expect("deadline outcome is recorded");
        scheduler.tick().await.expect("same-value follow-up tick");

        assert_eq!(
            commands.calls.load(Ordering::SeqCst),
            1,
            "an ambiguous command timeout must not replay the same transition"
        );
        let rules = scheduler.rules.read().await;
        assert_eq!(rules[0].onchange_state.last_value[&point.cache_key()], 10.0);
        assert!(rules[0].onchange_state.last_trigger.is_some());
    }

    #[tokio::test]
    async fn tick_pointwatch_and_manual_share_one_rule_flight() {
        let rule = create_action_rule(33, 0);
        let pool = rule_pool(&rule).await;
        let live_state = Arc::new(crate::MemoryRuleLiveState::new());
        assert!(live_state.set_instance(1, 0, 0, 10.0, 1));
        assert!(live_state.set_instance(42, 1, 7, 0.0, 1));
        let commands = Arc::new(BlockingActionCommands::new());
        let command_facade: Arc<dyn RuleActionCommandFacade> = commands.clone();
        let logs = tempfile::tempdir().expect("temporary rule logs");
        let scheduler = Arc::new(RuleScheduler::with_state_store(
            live_state,
            pool,
            1,
            logs.path().to_path_buf(),
            Arc::new(aether_calc::MemoryStateStore::new()),
            Some(command_facade),
        ));
        let point = PointRef {
            instance: 1,
            point_type: PointKind::Measurement,
            point: 0,
        };
        scheduler.rules.write().await.push(ScheduledRule {
            rule: Arc::new(rule),
            trigger: TriggerConfig::OnChange {
                point_refs: vec![point],
                time_deadband_ms: None,
                value_deadband: None,
            },
            last_execution: None,
            last_cooldown_start: None,
            onchange_state: OnChangeState::default(),
            trigger_valid: true,
            retry_not_before: None,
        });

        let tick_task = {
            let scheduler = Arc::clone(&scheduler);
            tokio::spawn(async move { scheduler.tick().await })
        };
        tokio::time::timeout(Duration::from_secs(1), commands.entered.notified())
            .await
            .expect("tick entered the governed action");

        let manual = scheduler
            .execute_rule(33)
            .await
            .expect_err("manual overlap must be rejected");
        assert!(
            matches!(manual, crate::RuleError::SchedulerError(message) if message.contains("already executing"))
        );
        scheduler
            .execute_watch_triggered(&crate::point_watch_dispatcher::WatchEvent {
                rule_ids: vec![33],
                channel_id: 10,
                point_kind: aether_domain::PointKind::Telemetry,
                point_id: 0,
                value: 999.0,
                raw: 999.0,
                timestamp_ms: 1,
            })
            .await
            .expect("PointWatch overlap is coalesced");
        assert_eq!(
            commands.calls.load(Ordering::SeqCst),
            1,
            "PointWatch and manual paths must not dispatch a second command"
        );

        commands.release.notify_one();
        tick_task
            .await
            .expect("tick task join")
            .expect("tick completed");
        assert_eq!(commands.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn process_limit_is_shared_by_different_rules_across_every_entry_path() {
        let tick_rule = create_action_rule(34, 0);
        let watch_rule = create_action_rule(35, 0);
        let manual_rule = create_action_rule(36, 0);
        let pool = rule_pool(&tick_rule).await;
        insert_rule(&pool, &watch_rule).await;
        insert_rule(&pool, &manual_rule).await;
        let live_state = Arc::new(crate::MemoryRuleLiveState::new());
        assert!(live_state.set_instance(1, 0, 0, 10.0, 1));
        assert!(live_state.set_instance(2, 0, 0, 20.0, 1));
        assert!(live_state.set_instance(42, 1, 7, 0.0, 1));
        let commands = Arc::new(BlockingActionCommands::new());
        let command_facade: Arc<dyn RuleActionCommandFacade> = commands.clone();
        let logs = tempfile::tempdir().expect("temporary rule logs");
        let mut scheduler = RuleScheduler::with_state_store(
            Arc::clone(&live_state),
            pool,
            1,
            logs.path().to_path_buf(),
            Arc::new(aether_calc::MemoryStateStore::new()),
            Some(command_facade),
        );
        scheduler
            .set_max_concurrency(1)
            .expect("valid test concurrency");
        assert_eq!(scheduler.execution_slots.available_permits(), 1);
        let scheduler = Arc::new(scheduler);
        scheduler.rules.write().await.extend([
            ScheduledRule {
                rule: Arc::new(tick_rule),
                trigger: TriggerConfig::OnChange {
                    point_refs: vec![pref(1, 0)],
                    time_deadband_ms: None,
                    value_deadband: None,
                },
                last_execution: None,
                last_cooldown_start: None,
                onchange_state: OnChangeState::default(),
                trigger_valid: true,
                retry_not_before: None,
            },
            ScheduledRule {
                rule: Arc::new(watch_rule),
                trigger: TriggerConfig::OnChange {
                    point_refs: vec![pref(2, 0)],
                    time_deadband_ms: None,
                    value_deadband: None,
                },
                last_execution: None,
                last_cooldown_start: None,
                onchange_state: OnChangeState {
                    last_value: HashMap::from([(pref(2, 0).cache_key(), 20.0)]),
                    last_trigger: None,
                },
                trigger_valid: true,
                retry_not_before: None,
            },
        ]);

        let tick_task = {
            let scheduler = Arc::clone(&scheduler);
            tokio::spawn(async move { scheduler.tick().await })
        };
        tokio::time::timeout(Duration::from_secs(1), commands.entered.notified())
            .await
            .expect("first scheduled rule acquired the only process slot");

        assert!(live_state.set_instance(2, 0, 0, 30.0, 2));
        let watch_task = {
            let scheduler = Arc::clone(&scheduler);
            tokio::spawn(async move {
                scheduler
                    .execute_watch_triggered(&crate::point_watch_dispatcher::WatchEvent {
                        rule_ids: vec![35],
                        channel_id: 10,
                        point_kind: aether_domain::PointKind::Telemetry,
                        point_id: 0,
                        value: 30.0,
                        raw: 30.0,
                        timestamp_ms: 2,
                    })
                    .await
            })
        };
        tokio::task::yield_now().await;
        let manual = scheduler
            .execute_rule(36)
            .await
            .expect_err("manual capacity overlap must return busy");
        assert!(
            matches!(manual, crate::RuleError::SchedulerError(message) if message.contains("capacity is busy") && message.contains("max_concurrency=1"))
        );
        assert_eq!(
            commands.calls.load(Ordering::SeqCst),
            1,
            "different tick, PointWatch, and manual rules share one global slot"
        );

        commands.release.notify_one();
        tokio::time::timeout(Duration::from_secs(1), commands.entered.notified())
            .await
            .expect("queued PointWatch rule acquired the released process slot");
        assert_eq!(
            commands.calls.load(Ordering::SeqCst),
            2,
            "background work waits for capacity instead of exceeding the limit"
        );
        commands.release.notify_one();
        tick_task
            .await
            .expect("tick task join")
            .expect("tick completed");
        watch_task
            .await
            .expect("PointWatch task join")
            .expect("PointWatch completed");
    }

    #[tokio::test]
    async fn invalid_concurrency_is_rejected_without_constructing_a_semaphore() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open scheduler database");
        let logs = tempfile::tempdir().expect("temporary rule logs");
        let mut scheduler = RuleScheduler::new(
            Arc::new(crate::MemoryRuleLiveState::new()),
            pool,
            100,
            logs.path().to_path_buf(),
        );

        assert!(scheduler.set_max_concurrency(0).is_err());
        assert!(
            scheduler
                .set_max_concurrency(MAX_RULE_CONCURRENCY + 1)
                .is_err()
        );
        assert_eq!(scheduler.max_concurrency, 4);
        assert_eq!(scheduler.execution_slots.available_permits(), 4);
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
            trigger_valid: true,
            retry_not_before: None,
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
            trigger_valid: true,
            retry_not_before: None,
        };

        let scheduled2 = ScheduledRule {
            rule: Arc::clone(&rule2),
            trigger: TriggerConfig::Interval { interval_ms: 200 },
            last_execution: None,
            last_cooldown_start: None,
            onchange_state: OnChangeState::default(),
            trigger_valid: true,
            retry_not_before: None,
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
        let scheduler = RuleScheduler::new(live_state, pool, 100, log_dir.path().to_path_buf());
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
            trigger_valid: true,
            retry_not_before: None,
        });

        scheduler
            .execute_watch_triggered(&crate::point_watch_dispatcher::WatchEvent {
                rule_ids: vec![7],
                channel_id: 10,
                point_kind: aether_domain::PointKind::Telemetry,
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

    #[tokio::test]
    async fn sixty_four_contended_history_writes_share_one_total_deadline() {
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
        let held = pool.acquire().await.expect("hold the only DB connection");
        let result = RuleExecutionResult {
            rule_id: 1,
            success: true,
            actions_executed: Vec::new(),
            error: None,
            execution_path: vec!["start".to_string(), "end".to_string()],
            matched_condition: None,
            variable_values: Arc::new(HashMap::new()),
            point_values: Arc::new(HashMap::new()),
            node_details: HashMap::new(),
        };
        let results = (0..64)
            .map(|id| {
                let mut result = result.clone();
                result.rule_id = id;
                (id, result)
            })
            .collect();
        let started = Instant::now();

        persist_rule_execution_batch(&pool, results, Duration::from_millis(20)).await;

        assert!(
            started.elapsed() < Duration::from_millis(200),
            "64 blocked writes must not each consume an independent deadline"
        );
        drop(held);
    }

    #[tokio::test]
    async fn application_rule_port_classifies_missing_rule_as_invalid_data() {
        use aether_domain::RuleId;
        use aether_ports::{AutomationRuleExecutor, PortErrorKind};

        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open in-memory sqlite");
        sqlx::query(
            "CREATE TABLE rules (\
                 id INTEGER PRIMARY KEY,\
                 name TEXT NOT NULL,\
                 description TEXT,\
                 enabled INTEGER NOT NULL,\
                 priority INTEGER NOT NULL,\
                 cooldown_ms INTEGER NOT NULL,\
                 trigger_config TEXT,\
                 nodes_json TEXT NOT NULL\
             )",
        )
        .execute(&pool)
        .await
        .expect("create rules table");
        let log_dir = tempfile::tempdir().expect("temporary rule logs");
        let scheduler = RuleScheduler::new(
            Arc::new(crate::MemoryRuleLiveState::new()),
            pool,
            100,
            log_dir.path().to_path_buf(),
        );

        let error = AutomationRuleExecutor::execute(&scheduler, RuleId::new(404))
            .await
            .expect_err("unknown rule is rejected");

        assert_eq!(error.kind(), PortErrorKind::InvalidData);
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

    #[test]
    fn reloading_preserves_scheduling_state_for_rules_that_still_exist() {
        // Replacing the whole vector reset every cooldown, so editing one rule
        // made every rule immediately eligible to fire again.
        let now = Instant::now();
        let onchange = OnChangeState {
            last_trigger: Some(now),
            last_value: HashMap::from([("inst:1:M:7".to_string(), 42.0)]),
        };

        let previous = vec![ScheduledRule {
            rule: Arc::new(create_test_rule(1, "kept", 60_000)),
            trigger: TriggerConfig::default(),
            last_execution: Some(now),
            last_cooldown_start: Some(now),
            onchange_state: onchange,
            trigger_valid: true,
            retry_not_before: None,
        }];
        let mut loaded = vec![
            ScheduledRule {
                rule: Arc::new(create_test_rule(1, "kept", 60_000)),
                trigger: TriggerConfig::default(),
                last_execution: None,
                last_cooldown_start: None,
                onchange_state: OnChangeState::default(),
                trigger_valid: true,
                retry_not_before: None,
            },
            ScheduledRule {
                rule: Arc::new(create_test_rule(2, "added", 0)),
                trigger: TriggerConfig::default(),
                last_execution: None,
                last_cooldown_start: None,
                onchange_state: OnChangeState::default(),
                trigger_valid: true,
                retry_not_before: None,
            },
        ];

        preserve_schedule_state(&previous, &mut loaded);

        assert_eq!(
            loaded[0].last_execution,
            Some(now),
            "a surviving rule must keep its cooldown across a reload"
        );
        assert_eq!(loaded[0].last_cooldown_start, Some(now));
        assert_eq!(loaded[0].onchange_state.last_trigger, Some(now));
        assert_eq!(
            loaded[0].onchange_state.last_value.get("inst:1:M:7"),
            Some(&42.0),
            "OnChange baselines must survive too, or every point looks changed"
        );
        assert_eq!(
            loaded[1].last_execution, None,
            "a newly added rule starts fresh"
        );
    }

    #[test]
    fn an_absent_trigger_config_fails_closed() {
        let error = resolve_trigger(None).expect_err("trigger_config is mandatory");
        assert!(error.to_string().contains("trigger_config is required"));
    }

    #[test]
    fn a_valid_trigger_config_is_parsed() {
        let trigger = resolve_trigger(Some(r#"{"type":"interval","interval_ms":250}"#))
            .expect("valid trigger");

        let TriggerConfig::Interval { interval_ms } = trigger else {
            panic!("expected the configured interval trigger");
        };
        assert_eq!(interval_ms, 250);
    }

    #[test]
    fn corrupt_and_semantically_invalid_triggers_fail_closed() {
        for invalid in [
            "{not json",
            r#"{"type":"interval","interval_ms":0}"#,
            r#"{"type":"on_change","point_refs":[]}"#,
            r#"{"type":"on_change","point_refs":[{"instance":1,"point_type":"measurement","point":0},{"instance":1,"point_type":"measurement","point":0}]}"#,
            r#"{"type":"on_change","point_refs":[{"instance":1,"point_type":"measurement","point":0}],"value_deadband":{"type":"absolute","threshold":-0.1}}"#,
        ] {
            assert!(
                resolve_trigger(Some(invalid)).is_err(),
                "invalid trigger must never be converted into an executable fallback: {invalid}"
            );
        }

        let refs = (0..=MAX_ONCHANGE_POINT_REFS)
            .map(|point| PointRef {
                instance: 1,
                point_type: PointKind::Measurement,
                point: point as u32,
            })
            .collect::<Vec<_>>();
        assert!(
            TriggerConfig::OnChange {
                point_refs: refs,
                time_deadband_ms: None,
                value_deadband: None,
            }
            .validate()
            .is_err(),
            "oversized subscription lists must fail before scheduler publication"
        );
    }
}
