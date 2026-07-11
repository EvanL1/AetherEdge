//! Built-in functions for expression evaluation
//!
//! Provides stateful functions: integrate, moving_avg, rate_of_change
//! And stateless functions: scale, clamp, abs, min, max

use crate::error::{CalcError, Result};
use crate::state::{
    IntegrateState, MovingAvgState, PeriodDeltaState, RateOfChangeState, StateStore, state_key,
};
use chrono::{Datelike, Local, TimeZone, Utc};
use std::sync::Arc;
use tracing::debug;

/// Built-in function executor
///
/// Handles execution of stateful and stateless built-in functions.
///
/// # Type Parameters
/// * `S` - State store implementation (defaults to MemoryStateStore)
pub struct BuiltinFunctions<S: StateStore> {
    /// State store for stateful functions
    state_store: Arc<S>,
    /// Context identifier (e.g., rule_id, instance_id)
    context: String,
}

impl<S: StateStore> BuiltinFunctions<S> {
    pub fn new(state_store: Arc<S>, context: impl Into<String>) -> Self {
        Self {
            state_store,
            context: context.into(),
        }
    }

    /// Execute integrate function
    ///
    /// Calculates time integral: accumulated += value * dt
    /// Returns accumulated value (e.g., kWh from W)
    ///
    /// # Arguments
    /// * `var_name` - Variable name for state tracking
    /// * `value` - Current value to integrate
    /// * `unit_factor` - Conversion factor (default 1.0, use 1/3600 for W→Wh)
    pub async fn integrate(&self, var_name: &str, value: f64, unit_factor: f64) -> Result<f64> {
        let key = state_key(&self.context, "integrate", var_name);
        let now = Utc::now().timestamp() as f64;

        // Load existing state
        let state = if let Some(data) = self.state_store.get(&key).await? {
            serde_json::from_slice::<IntegrateState>(&data)
                .map_err(|e| CalcError::state(format!("Failed to deserialize state: {}", e)))?
        } else {
            // First call - initialize with current time, no accumulation yet
            let initial = IntegrateState {
                last_ts: now,
                accumulated: 0.0,
            };
            let data = serde_json::to_vec(&initial)
                .map_err(|e| CalcError::state(format!("Failed to serialize state: {}", e)))?;
            self.state_store.set(&key, &data).await?;
            return Ok(0.0); // First call returns 0
        };

        // Calculate dt (time delta in seconds)
        let dt = now - state.last_ts;
        if dt <= 0.0 {
            return Ok(state.accumulated);
        }

        // Integrate: accumulated += value * dt * unit_factor
        let delta = value * dt * unit_factor;
        let new_accumulated = state.accumulated + delta;

        debug!(
            var = var_name,
            value = value,
            dt = dt,
            delta = delta,
            accumulated = new_accumulated,
            "integrate"
        );

        // Save new state
        let new_state = IntegrateState {
            last_ts: now,
            accumulated: new_accumulated,
        };
        let data = serde_json::to_vec(&new_state)
            .map_err(|e| CalcError::state(format!("Failed to serialize state: {}", e)))?;
        self.state_store.set(&key, &data).await?;

        Ok(new_accumulated)
    }

    /// Execute moving average function
    ///
    /// Calculates moving average over a sliding window
    ///
    /// # Arguments
    /// * `var_name` - Variable name for state tracking
    /// * `value` - Current value to add
    /// * `window` - Window size (number of samples)
    pub async fn moving_avg(&self, var_name: &str, value: f64, window: usize) -> Result<f64> {
        if window == 0 {
            return Err(CalcError::function(
                "moving_avg: window must be >= 1 (got 0)",
            ));
        }
        let key = state_key(&self.context, "moving_avg", var_name);

        // Load or create state
        let mut state = if let Some(data) = self.state_store.get(&key).await? {
            let s: MovingAvgState = serde_json::from_slice(&data)
                .map_err(|e| CalcError::state(format!("Failed to deserialize state: {}", e)))?;
            // Handle window size change
            if s.values.len() != window {
                MovingAvgState::new(window)
            } else {
                s
            }
        } else {
            MovingAvgState::new(window)
        };

        // Add value and calculate average
        let avg = state.add(value);

        debug!(
            var = var_name,
            value = value,
            window = window,
            avg = avg,
            "moving_avg"
        );

        // Save state
        let data = serde_json::to_vec(&state)
            .map_err(|e| CalcError::state(format!("Failed to serialize state: {}", e)))?;
        self.state_store.set(&key, &data).await?;

        Ok(avg)
    }

    /// Execute rate of change function
    ///
    /// Calculates dv/dt (change rate per second)
    ///
    /// # Arguments
    /// * `var_name` - Variable name for state tracking
    /// * `value` - Current value
    pub async fn rate_of_change(&self, var_name: &str, value: f64) -> Result<f64> {
        let key = state_key(&self.context, "rate", var_name);
        let now = Utc::now().timestamp() as f64;

        // Load existing state
        let state = if let Some(data) = self.state_store.get(&key).await? {
            serde_json::from_slice::<RateOfChangeState>(&data)
                .map_err(|e| CalcError::state(format!("Failed to deserialize state: {}", e)))?
        } else {
            // First call - store current and return NaN. There is no defensible
            // numeric answer with only one sample: returning 0.0 would mean
            // "rate is zero" (a real measurement) and trigger downstream rules
            // like "rate < threshold". NaN propagates through validate_value
            // → action_skipped, so dependent writes are correctly suppressed
            // until two samples exist.
            let initial = RateOfChangeState {
                last_ts: now,
                last_value: value,
            };
            let data = serde_json::to_vec(&initial)
                .map_err(|e| CalcError::state(format!("Failed to serialize state: {}", e)))?;
            self.state_store.set(&key, &data).await?;
            return Ok(f64::NAN);
        };

        // Calculate rate. Same dt==0 case: with no time elapsed, "rate" is
        // ill-defined — return NaN rather than 0.0 (which would falsely
        // assert "no change") so downstream rules skip the write.
        let dt = now - state.last_ts;
        let rate = if dt > 0.0 {
            (value - state.last_value) / dt
        } else {
            f64::NAN
        };

        debug!(
            var = var_name,
            value = value,
            last_value = state.last_value,
            dt = dt,
            rate = rate,
            "rate_of_change"
        );

        // Save new state
        let new_state = RateOfChangeState {
            last_ts: now,
            last_value: value,
        };
        let data = serde_json::to_vec(&new_state)
            .map_err(|e| CalcError::state(format!("Failed to serialize state: {}", e)))?;
        self.state_store.set(&key, &data).await?;

        Ok(rate)
    }

    /// Reset all states for this context
    pub async fn reset_states(&self) -> Result<()> {
        // This is a simplified implementation
        // In production, you'd want to iterate and delete all keys with the context prefix
        Ok(())
    }

    /// Execute period delta function
    ///
    /// Calculates the change (delta) of a cumulative value within a time period.
    /// Useful for calculating energy consumption: daily kWh from total kWh counter.
    ///
    /// Period types:
    /// - "daily": Resets at midnight local time
    /// - "weekly": Resets at Monday midnight local time
    /// - "monthly": Resets at 1st of month midnight
    /// - "quarterly": Resets at Q1/Q2/Q3/Q4 start
    ///
    /// # Arguments
    /// * `var_name` - Variable name for state tracking
    /// * `value` - Current cumulative value (e.g., total kWh)
    /// * `period` - Period type: "daily", "weekly", "monthly", "quarterly"
    ///
    /// # Returns
    /// Delta value (current - snapshot). On the first call (no period baseline
    /// yet) returns NaN so downstream `validate_value` skips writes — a 0.0
    /// would falsely report "no consumption this period" before the first
    /// snapshot is even captured.
    ///
    /// # Counter Reset Handling
    /// If value < snapshot (counter reset), snapshot is updated to current value
    /// and delta is 0.0 for that call.
    pub async fn period_delta(&self, var_name: &str, value: f64, period: &str) -> Result<f64> {
        let key = state_key(
            &self.context,
            "period_delta",
            &format!("{}_{}", var_name, period),
        );
        let now = Utc::now();

        // Load existing state or create initial
        let mut state = if let Some(data) = self.state_store.get(&key).await? {
            serde_json::from_slice::<PeriodDeltaState>(&data)
                .map_err(|e| CalcError::state(format!("Failed to deserialize state: {}", e)))?
        } else {
            // First call - initialize with current value and period start.
            // Return NaN, not 0.0: with no prior snapshot we cannot honestly
            // report "delta == 0 for this period". 0.0 would fool downstream
            // dashboards/rules into treating "no baseline yet" as "no
            // consumption". NaN flows through validate_value → action_skipped.
            let initial = PeriodDeltaState {
                snapshot: value,
                period_start_ts: Self::get_period_start(now, period),
            };
            let data = serde_json::to_vec(&initial)
                .map_err(|e| CalcError::state(format!("Failed to serialize state: {}", e)))?;
            self.state_store.set(&key, &data).await?;
            return Ok(f64::NAN);
        };

        // Check if period has rotated
        let current_period_start = Self::get_period_start(now, period);
        if current_period_start > state.period_start_ts {
            // New period - take new snapshot
            debug!(
                var = var_name,
                period = period,
                old_snapshot = state.snapshot,
                new_snapshot = value,
                "period_delta: period rotated"
            );
            state.snapshot = value;
            state.period_start_ts = current_period_start;
        }

        // Handle counter reset (value decreased, likely meter reset)
        if value < state.snapshot {
            debug!(
                var = var_name,
                value = value,
                snapshot = state.snapshot,
                "period_delta: counter reset detected"
            );
            state.snapshot = value;
        }

        // Calculate delta
        let delta = value - state.snapshot;

        // Save updated state
        let data = serde_json::to_vec(&state)
            .map_err(|e| CalcError::state(format!("Failed to serialize state: {}", e)))?;
        self.state_store.set(&key, &data).await?;

        debug!(
            var = var_name,
            period = period,
            value = value,
            snapshot = state.snapshot,
            delta = delta,
            "period_delta"
        );

        Ok(delta)
    }

    /// Get the start timestamp of the current period in local timezone
    ///
    /// Returns Unix timestamp (seconds) for the start of:
    /// - daily: midnight today
    /// - weekly: Monday midnight of current week
    /// - monthly: 1st of current month midnight
    /// - quarterly: 1st of current quarter (Jan/Apr/Jul/Oct) midnight
    fn get_period_start(now: chrono::DateTime<Utc>, period: &str) -> i64 {
        let local = now.with_timezone(&Local);

        match period {
            "daily" => {
                // Midnight of today in local timezone
                local
                    .date_naive()
                    .and_hms_opt(0, 0, 0)
                    .and_then(|dt| Local.from_local_datetime(&dt).single())
                    .map(|dt| dt.timestamp())
                    .unwrap_or_else(|| now.timestamp())
            },
            "weekly" => {
                // Monday midnight of current week
                let days_since_monday = local.weekday().num_days_from_monday() as i64;
                let monday = local - chrono::Duration::days(days_since_monday);
                monday
                    .date_naive()
                    .and_hms_opt(0, 0, 0)
                    .and_then(|dt| Local.from_local_datetime(&dt).single())
                    .map(|dt| dt.timestamp())
                    .unwrap_or_else(|| now.timestamp())
            },
            "monthly" => {
                // 1st of current month midnight
                local
                    .with_day(1)
                    .and_then(|dt| dt.date_naive().and_hms_opt(0, 0, 0))
                    .and_then(|dt| Local.from_local_datetime(&dt).single())
                    .map(|dt| dt.timestamp())
                    .unwrap_or_else(|| now.timestamp())
            },
            "quarterly" => {
                // 1st of current quarter (Jan=1, Apr=4, Jul=7, Oct=10)
                let quarter_month = ((local.month() - 1) / 3) * 3 + 1;
                local
                    .with_month(quarter_month)
                    .and_then(|dt| dt.with_day(1))
                    .and_then(|dt| dt.date_naive().and_hms_opt(0, 0, 0))
                    .and_then(|dt| Local.from_local_datetime(&dt).single())
                    .map(|dt| dt.timestamp())
                    .unwrap_or_else(|| now.timestamp())
            },
            _ => {
                // Unknown period - use current timestamp (effectively no period tracking)
                now.timestamp()
            },
        }
    }
}

// === Stateless functions (pure, no state needed) ===

/// Scale a value by a factor
pub fn scale(value: f64, factor: f64) -> f64 {
    value * factor
}

/// Clamp a value to a range
pub fn clamp(value: f64, min: f64, max: f64) -> f64 {
    if min.is_nan() || max.is_nan() {
        return value;
    }
    let (lo, hi) = if min <= max { (min, max) } else { (max, min) };
    if value < lo {
        lo
    } else if value > hi {
        hi
    } else {
        value
    }
}

/// Absolute value
pub fn abs(value: f64) -> f64 {
    value.abs()
}

/// Minimum of two values
pub fn min(a: f64, b: f64) -> f64 {
    a.min(b)
}

/// Maximum of two values
pub fn max(a: f64, b: f64) -> f64 {
    a.max(b)
}

/// Round to specified decimal places
pub fn round(value: f64, decimals: i32) -> f64 {
    let factor = 10_f64.powi(decimals);
    (value * factor).round() / factor
}

/// Sign function: returns -1, 0, or 1
pub fn sign(value: f64) -> f64 {
    if value > 0.0 {
        1.0
    } else if value < 0.0 {
        -1.0
    } else {
        0.0
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
#[allow(clippy::approx_constant)]
mod tests {
    use super::*;
    use crate::state::MemoryStateStore;
    use std::sync::Arc;

    #[test]
    fn test_scale() {
        assert_eq!(scale(100.0, 0.5), 50.0);
        assert_eq!(scale(100.0, 2.0), 200.0);
    }

    #[test]
    fn test_clamp() {
        assert_eq!(clamp(50.0, 0.0, 100.0), 50.0);
        assert_eq!(clamp(-10.0, 0.0, 100.0), 0.0);
        assert_eq!(clamp(150.0, 0.0, 100.0), 100.0);
        assert_eq!(clamp(50.0, 100.0, 0.0), 50.0);
    }

    #[test]
    fn test_round() {
        assert_eq!(round(3.14159, 2), 3.14);
        assert_eq!(round(3.145, 2), 3.15);
        assert_eq!(round(3.14159, 0), 3.0);
    }

    #[test]
    fn test_moving_avg_state() {
        let mut state = MovingAvgState::new(3);
        assert_eq!(state.add(10.0), 10.0); // [10], avg=10
        assert_eq!(state.add(20.0), 15.0); // [10,20], avg=15
        assert_eq!(state.add(30.0), 20.0); // [10,20,30], avg=20
        assert_eq!(state.add(40.0), 30.0); // [40,20,30], avg=30 (overwrites 10)
    }

    #[tokio::test]
    async fn test_integrate_basic() {
        let store = Arc::new(MemoryStateStore::new());
        let funcs = BuiltinFunctions::new(store, "test");

        // First call initializes, returns 0
        let result = funcs.integrate("power", 1000.0, 1.0).await.unwrap();
        assert_eq!(result, 0.0);
    }

    #[tokio::test]
    async fn test_moving_avg_async() {
        let store = Arc::new(MemoryStateStore::new());
        let funcs = BuiltinFunctions::new(store, "test");

        let _ = funcs.moving_avg("temp", 10.0, 3).await.unwrap();
        let _ = funcs.moving_avg("temp", 20.0, 3).await.unwrap();
        let avg = funcs.moving_avg("temp", 30.0, 3).await.unwrap();
        assert_eq!(avg, 20.0); // (10+20+30)/3
    }

    #[tokio::test]
    async fn test_moving_avg_window_zero_returns_err() {
        let store = Arc::new(MemoryStateStore::new());
        let funcs = BuiltinFunctions::new(store, "test");

        // window=0 must surface as a CalcError, not silently succeed and not panic.
        let err = funcs.moving_avg("temp", 10.0, 0).await.unwrap_err();
        assert!(matches!(err, CalcError::Function(_)));
    }

    #[tokio::test]
    async fn test_rate_of_change_first_call_returns_nan() {
        let store = Arc::new(MemoryStateStore::new());
        let funcs = BuiltinFunctions::new(store, "test");

        // First call has no baseline → NaN, not 0.0. Locks the contract:
        // no two-sample comparison is possible yet, so any caller that
        // forwards this into a SCADA write must be filtered out by
        // validate_value's NaN-rejection rather than fooled by a fake zero.
        let rate = funcs.rate_of_change("voltage", 100.0).await.unwrap();
        assert!(
            rate.is_nan(),
            "rate_of_change first call must return NaN sentinel, got {}",
            rate
        );
    }

    #[tokio::test]
    async fn test_period_delta_first_call_returns_nan() {
        let store = Arc::new(MemoryStateStore::new());
        let funcs = BuiltinFunctions::new(store, "test");

        // First call captures the snapshot but cannot compute a meaningful
        // delta yet — must return NaN, not 0.0.
        let delta = funcs
            .period_delta("kwh_meter", 12345.0, "daily")
            .await
            .unwrap();
        assert!(
            delta.is_nan(),
            "period_delta first call must return NaN sentinel, got {}",
            delta
        );

        // Subsequent call within the same period: now there's a baseline,
        // delta is well-defined (and finite).
        let delta2 = funcs
            .period_delta("kwh_meter", 12350.0, "daily")
            .await
            .unwrap();
        assert!(
            delta2.is_finite(),
            "second call must return finite delta, got {}",
            delta2
        );
        assert!(
            delta2 >= 0.0,
            "delta should be non-negative for a counter increase"
        );
    }
}
