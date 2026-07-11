//! State storage for stateful functions
//!
//! Functions like `integrate()` and `moving_avg()` need to persist state
//! between evaluations (last timestamp, window values, etc.).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::future::Future;
use tokio::sync::RwLock;

use crate::error::Result;

/// State storage trait for stateful functions
///
/// Implementations may use in-memory or local embedded persistence. Network
/// stores do not belong in the deterministic edge runtime.
pub trait StateStore: Send + Sync {
    /// Get state for a key
    fn get(&self, key: &str) -> impl Future<Output = Result<Option<Vec<u8>>>> + Send;

    /// Set state for a key
    fn set(&self, key: &str, value: &[u8]) -> impl Future<Output = Result<()>> + Send;

    /// Delete state for a key
    fn delete(&self, key: &str) -> impl Future<Output = Result<()>> + Send;
}

/// In-memory state store for testing and simple use cases
#[derive(Default)]
pub struct MemoryStateStore {
    data: RwLock<HashMap<String, Vec<u8>>>,
}

impl MemoryStateStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl StateStore for MemoryStateStore {
    fn get(&self, key: &str) -> impl Future<Output = Result<Option<Vec<u8>>>> + Send {
        let key = key.to_string();
        async move {
            let data = self.data.read().await;
            Ok(data.get(&key).cloned())
        }
    }

    fn set(&self, key: &str, value: &[u8]) -> impl Future<Output = Result<()>> + Send {
        let key = key.to_string();
        let value = value.to_vec();
        async move {
            let mut data = self.data.write().await;
            data.insert(key, value);
            Ok(())
        }
    }

    fn delete(&self, key: &str) -> impl Future<Output = Result<()>> + Send {
        let key = key.to_string();
        async move {
            let mut data = self.data.write().await;
            data.remove(&key);
            Ok(())
        }
    }
}

// === State data structures for built-in functions ===

/// Integrate function state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrateState {
    /// Last timestamp (Unix seconds, f64 for precision)
    pub last_ts: f64,
    /// Accumulated value
    pub accumulated: f64,
}

/// Moving average function state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MovingAvgState {
    /// Circular buffer of recent values
    pub values: Vec<f64>,
    /// Next write position in buffer
    pub position: usize,
    /// Number of values stored (may be less than buffer size initially)
    pub count: usize,
}

impl MovingAvgState {
    pub fn new(window_size: usize) -> Self {
        // Defense in depth: API layer (`BuiltinFunctions::moving_avg`) rejects window=0
        // with `CalcError::Function`, but if a future caller skips that path the empty
        // buffer would still panic on index in `add()`. Clamp protects all callers.
        let window_size = window_size.max(1);
        Self {
            values: vec![0.0; window_size],
            position: 0,
            count: 0,
        }
    }

    /// Add a value and return the new moving average.
    ///
    /// Resilient to corrupted persisted state: if `values` is empty (which `new`
    /// prevents but a stale checkpoint could carry), or `position` is out of bounds,
    /// the buffer is rebuilt rather than panicking on index.
    pub fn add(&mut self, value: f64) -> f64 {
        if self.values.is_empty() {
            self.values = vec![0.0];
            self.position = 0;
            self.count = 0;
        }
        self.position %= self.values.len();
        let Some(slot) = self.values.get_mut(self.position) else {
            return self.average();
        };
        *slot = value;
        self.position = (self.position + 1) % self.values.len();
        if self.count < self.values.len() {
            self.count += 1;
        }
        self.average()
    }

    /// Get current average.
    ///
    /// Tolerates corrupted state where `count` exceeds `values.len()` by clamping.
    pub fn average(&self) -> f64 {
        if self.count == 0 || self.values.is_empty() {
            return 0.0;
        }
        let take = self.count.min(self.values.len());
        let sum: f64 = self.values.iter().take(take).sum();
        sum / take as f64
    }
}

/// Rate of change function state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateOfChangeState {
    /// Last timestamp (Unix seconds)
    pub last_ts: f64,
    /// Last value
    pub last_value: f64,
}

/// Period delta function state
///
/// Tracks the snapshot value at the start of each period (daily, weekly, monthly, quarterly)
/// to calculate the delta (change) within the current period.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeriodDeltaState {
    /// Snapshot value at period start (cumulative counter reading)
    pub snapshot: f64,
    /// Period start timestamp (Unix seconds)
    pub period_start_ts: i64,
}

/// Helper function to create state key
///
/// Format: `calc:state:{context}:{func}:{var}`
pub fn state_key(context: &str, func: &str, var: &str) -> String {
    format!("calc:state:{}:{}:{}", context, func, var)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moving_avg_window_zero_does_not_panic() {
        let mut s = MovingAvgState::new(0);
        assert_eq!(s.add(1.0), 1.0);
        assert_eq!(s.add(2.0), 2.0);
    }

    #[test]
    fn moving_avg_corrupted_position_recovers() {
        // Persisted state from a previous run could end up with position > values.len()
        // (e.g. version skew or a corrupt local checkpoint). add() must self-heal, not panic.
        let mut s = MovingAvgState {
            values: vec![0.0, 0.0, 0.0],
            position: 999,
            count: 0,
        };
        assert_eq!(s.add(1.0), 1.0);
        assert!(s.position < s.values.len());
    }

    #[test]
    fn moving_avg_empty_values_recovers() {
        // Stale blob with empty buffer would panic at values[position]; add() rebuilds.
        let mut s = MovingAvgState {
            values: vec![],
            position: 0,
            count: 5,
        };
        assert_eq!(s.add(7.0), 7.0);
        assert_eq!(s.values.len(), 1);
    }

    #[test]
    fn moving_avg_count_exceeds_capacity_clamps() {
        // Corrupted count > values.len() must not panic in average() and must clamp.
        let s = MovingAvgState {
            values: vec![1.0, 2.0],
            position: 0,
            count: 999,
        };
        assert_eq!(s.average(), 1.5);
    }
}
