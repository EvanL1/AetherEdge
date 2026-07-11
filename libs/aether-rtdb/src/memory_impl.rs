//! High-performance in-memory RTDB implementation
//!
//! Uses DashMap for lock-free concurrent access with excellent performance.
//! Perfect for testing and embedded scenarios.

use crate::numfmt::{f64_to_bytes, i64_to_bytes};
use crate::traits::*;
use anyhow::Result;
use bytes::Bytes;
use dashmap::{DashMap, DashSet};
use parking_lot::RwLock;
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::Arc;

/// Normalize Redis-style list indices (supporting negative offsets)
///
/// Converts Redis-compatible (start, stop) range with negative index support
/// into a (start_idx, stop_idx) pair of positive indices for slicing.
/// Returns `(0, 0)` for empty/invalid ranges.
#[inline]
fn normalize_list_indices(len: usize, start: isize, stop: isize) -> (usize, usize) {
    let len = len as isize;
    let start_idx = if start < 0 {
        (len + start).max(0) as usize
    } else {
        start.min(len) as usize
    };
    let stop_idx = if stop < 0 {
        (len + stop + 1).max(0) as usize
    } else {
        (stop + 1).min(len) as usize
    };
    (start_idx, stop_idx)
}

/// In-memory RTDB implementation with concurrent access support
///
/// This is a pure storage abstraction. For routing logic, use the
/// `aether-routing` library which handles M2C routing externally.
pub struct MemoryRtdb {
    kv_store: Arc<DashMap<String, Bytes>>,
    hash_store: Arc<DashMap<String, DashMap<String, Bytes>>>,
    list_store: Arc<DashMap<String, RwLock<VecDeque<Bytes>>>>,
    set_store: Arc<DashMap<String, DashSet<String>>>,
}

impl MemoryRtdb {
    /// Create new in-memory RTDB instance
    pub fn new() -> Self {
        Self {
            kv_store: Arc::new(DashMap::new()),
            hash_store: Arc::new(DashMap::new()),
            list_store: Arc::new(DashMap::new()),
            set_store: Arc::new(DashMap::new()),
        }
    }

    /// Clear all data (useful for testing)
    pub fn clear(&self) {
        self.kv_store.clear();
        self.hash_store.clear();
        self.list_store.clear();
        self.set_store.clear();
    }

    /// Get statistics about stored data
    pub fn stats(&self) -> MemoryStats {
        MemoryStats {
            kv_count: self.kv_store.len(),
            hash_count: self.hash_store.len(),
            list_count: self.list_store.len(),
            set_count: self.set_store.len(),
        }
    }
}

impl Default for MemoryRtdb {
    fn default() -> Self {
        Self::new()
    }
}

/// Statistics about memory RTDB usage
#[derive(Debug, Clone)]
pub struct MemoryStats {
    pub kv_count: usize,
    pub hash_count: usize,
    pub list_count: usize,
    pub set_count: usize,
}

impl Rtdb for MemoryRtdb {
    fn get(&self, key: &str) -> impl Future<Output = Result<Option<Bytes>>> + Send + '_ {
        let result = self.kv_store.get(key).map(|v| v.clone());
        async move { Ok(result) }
    }

    fn set(&self, key: &str, value: Bytes) -> impl Future<Output = Result<()>> + Send + '_ {
        self.kv_store.insert(key.to_string(), value);
        async move { Ok(()) }
    }

    fn del(&self, key: &str) -> impl Future<Output = Result<bool>> + Send + '_ {
        let result = self.kv_store.remove(key).is_some();
        async move { Ok(result) }
    }

    fn exists(&self, key: &str) -> impl Future<Output = Result<bool>> + Send + '_ {
        let result = self.kv_store.contains_key(key);
        async move { Ok(result) }
    }

    fn incrbyfloat(
        &self,
        key: &str,
        increment: f64,
    ) -> impl Future<Output = Result<f64>> + Send + '_ {
        // Use entry API for atomic read-modify-write
        // The RefMut holds the shard lock, preventing concurrent access to this key
        let key_owned = key.to_string();
        let new_value = {
            let mut entry = self
                .kv_store
                .entry(key_owned.clone())
                .or_insert_with(|| Bytes::from("0"));

            // Parse current value (default to 0.0 if invalid, matching Redis behavior)
            let current: f64 = match std::str::from_utf8(entry.as_ref()) {
                Ok(s) => s.parse().unwrap_or_else(|_| {
                    tracing::warn!(
                        key = %key_owned,
                        value = %s,
                        "incrbyfloat: failed to parse value as f64, defaulting to 0.0"
                    );
                    0.0
                }),
                Err(_) => {
                    tracing::warn!(
                        key = %key_owned,
                        "incrbyfloat: value is not valid UTF-8, defaulting to 0.0"
                    );
                    0.0
                },
            };

            // Calculate and store new value atomically
            // Use ryu for zero-allocation f64 formatting
            let new_val = current + increment;
            *entry = f64_to_bytes(new_val);
            new_val
        };

        async move { Ok(new_value) }
    }

    fn hash_set(
        &self,
        key: &str,
        field: &str,
        value: Bytes,
    ) -> impl Future<Output = Result<()>> + Send + '_ {
        self.hash_store
            .entry(key.to_string())
            .or_default()
            .insert(field.to_string(), value);
        async move { Ok(()) }
    }

    fn hash_setnx(
        &self,
        key: &str,
        field: &str,
        value: Bytes,
    ) -> impl Future<Output = Result<bool>> + Send + '_ {
        // Mirror Redis HSETNX semantics: insert only when the field is
        // absent. DashMap entry().or_default() then field-level test under
        // the entry's lock — equivalent to HSETNX's atomic check-and-set.
        let mut entry = self.hash_store.entry(key.to_string()).or_default();
        let _: &mut _ = &mut *entry; // ensure mut is observably used
        let inserted = if entry.contains_key(field) {
            false
        } else {
            entry.insert(field.to_string(), value);
            true
        };
        async move { Ok(inserted) }
    }

    fn hash_get(
        &self,
        key: &str,
        field: &str,
    ) -> impl Future<Output = Result<Option<Bytes>>> + Send + '_ {
        let result = self
            .hash_store
            .get(key)
            .and_then(|hash| hash.get(field).map(|v| v.clone()));
        async move { Ok(result) }
    }

    fn hash_mget(
        &self,
        key: &str,
        fields: &[&str],
    ) -> impl Future<Output = Result<Vec<Option<Bytes>>>> + Send + '_ {
        let result = match self.hash_store.get(key) {
            Some(hash) => fields
                .iter()
                .map(|field| hash.get(*field).map(|v| v.clone()))
                .collect(),
            _ => {
                vec![None; fields.len()]
            },
        };
        async move { Ok(result) }
    }

    fn hash_mset(
        &self,
        key: &str,
        fields: Vec<(String, Bytes)>,
    ) -> impl Future<Output = Result<()>> + Send + '_ {
        let hash = self.hash_store.entry(key.to_string()).or_default();
        for (field, value) in fields {
            hash.insert(field, value);
        }
        async move { Ok(()) }
    }

    fn hash_get_all(
        &self,
        key: &str,
    ) -> impl Future<Output = Result<HashMap<String, Bytes>>> + Send + '_ {
        let result = match self.hash_store.get(key) {
            Some(hash) => {
                // Pre-allocate HashMap with exact capacity
                let mut map = HashMap::with_capacity(hash.len());
                for entry in hash.iter() {
                    map.insert(entry.key().clone(), entry.value().clone());
                }
                map
            },
            _ => HashMap::new(),
        };
        async move { Ok(result) }
    }

    fn hash_del(&self, key: &str, field: &str) -> impl Future<Output = Result<bool>> + Send + '_ {
        let result = match self.hash_store.get(key) {
            Some(hash) => hash.remove(field).is_some(),
            _ => false,
        };
        async move { Ok(result) }
    }

    fn hash_del_many(
        &self,
        key: &str,
        fields: &[String],
    ) -> impl Future<Output = Result<usize>> + Send + '_ {
        let result = match self.hash_store.get(key) {
            Some(hash) => {
                let mut removed = 0;
                for field in fields {
                    if hash.remove(field).is_some() {
                        removed += 1;
                    }
                }
                removed
            },
            _ => 0,
        };
        async move { Ok(result) }
    }

    fn list_lpush(&self, key: &str, value: Bytes) -> impl Future<Output = Result<()>> + Send + '_ {
        self.list_store
            .entry(key.to_string())
            .or_insert_with(|| RwLock::new(VecDeque::new()))
            .write()
            .push_front(value);
        async move { Ok(()) }
    }

    fn list_rpush(&self, key: &str, value: Bytes) -> impl Future<Output = Result<()>> + Send + '_ {
        self.list_store
            .entry(key.to_string())
            .or_insert_with(|| RwLock::new(VecDeque::new()))
            .write()
            .push_back(value);
        async move { Ok(()) }
    }

    fn list_lpop(&self, key: &str) -> impl Future<Output = Result<Option<Bytes>>> + Send + '_ {
        let result = self.list_store.get(key).and_then(|list| {
            let mut list = list.write();
            list.pop_front()
        });
        async move { Ok(result) }
    }

    fn list_rpop(&self, key: &str) -> impl Future<Output = Result<Option<Bytes>>> + Send + '_ {
        let result = self.list_store.get(key).and_then(|list| {
            let mut list = list.write();
            list.pop_back()
        });
        async move { Ok(result) }
    }

    fn list_blpop(
        &self,
        keys: &[&str],
        timeout_seconds: u64,
    ) -> impl Future<Output = Result<Option<(String, Bytes)>>> + Send + '_ {
        // For blocking pop, we need to clone the list_store reference
        let list_store = self.list_store.clone();
        let keys: Vec<String> = keys.iter().copied().map(String::from).collect();

        async move {
            use tokio::time::{Duration, Instant, sleep};

            let start = Instant::now();
            let timeout = Duration::from_secs(timeout_seconds);

            // Poll keys until timeout or data found
            loop {
                // Try to pop from each key in order
                for key in &keys {
                    if let Some(list) = list_store.get(key) {
                        let mut list = list.write();
                        if let Some(value) = list.pop_front() {
                            return Ok(Some((key.clone(), value)));
                        }
                    }
                }

                // Check timeout
                if start.elapsed() >= timeout {
                    return Ok(None);
                }

                // Sleep briefly before next poll (10ms)
                sleep(Duration::from_millis(10)).await;
            }
        }
    }

    fn list_range(
        &self,
        key: &str,
        start: isize,
        stop: isize,
    ) -> impl Future<Output = Result<Vec<Bytes>>> + Send + '_ {
        let result = match self.list_store.get(key) {
            Some(list) => {
                let list = list.read();
                let (start_idx, stop_idx) = normalize_list_indices(list.len(), start, stop);
                if start_idx < stop_idx {
                    let count = stop_idx - start_idx;
                    let mut result = Vec::with_capacity(count);
                    for item in list.iter().skip(start_idx).take(count) {
                        result.push(item.clone());
                    }
                    result
                } else {
                    Vec::new()
                }
            },
            _ => Vec::new(),
        };
        async move { Ok(result) }
    }

    fn list_trim(
        &self,
        key: &str,
        start: isize,
        stop: isize,
    ) -> impl Future<Output = Result<()>> + Send + '_ {
        if let Some(list) = self.list_store.get(key) {
            let mut list = list.write();
            let (start_idx, stop_idx) = normalize_list_indices(list.len(), start, stop);
            if start_idx < stop_idx && stop_idx <= list.len() {
                *list = list
                    .iter()
                    .skip(start_idx)
                    .take(stop_idx - start_idx)
                    .cloned()
                    .collect();
            } else {
                list.clear();
            }
        }
        async move { Ok(()) }
    }

    fn scan_match(&self, pattern: &str) -> impl Future<Output = Result<Vec<String>>> + Send + '_ {
        // Test stub: simple pattern matching on in-memory keys
        tracing::trace!("MemoryRtdb: SCAN MATCH pattern '{}'", pattern);

        // Convert glob pattern to regex (simple implementation for testing)
        let regex_pattern = pattern.replace("*", ".*").replace("?", ".");

        let kv_store = self.kv_store.clone();
        let hash_store = self.hash_store.clone();
        let list_store = self.list_store.clone();

        async move {
            let re = regex::Regex::new(&format!("^{}$", regex_pattern))?;

            let mut matches = Vec::new();

            // Scan KV store
            for entry in kv_store.iter() {
                if re.is_match(entry.key()) {
                    matches.push(entry.key().clone());
                }
            }

            // Scan hash store
            for entry in hash_store.iter() {
                if re.is_match(entry.key()) {
                    matches.push(entry.key().clone());
                }
            }

            // Scan list store
            for entry in list_store.iter() {
                if re.is_match(entry.key()) {
                    matches.push(entry.key().clone());
                }
            }

            matches.sort();
            matches.dedup();
            Ok(matches)
        }
    }

    fn sadd(&self, key: &str, member: &str) -> impl Future<Output = Result<bool>> + Send + '_ {
        let set = self.set_store.entry(key.to_string()).or_default();
        let result = set.insert(member.to_string());
        async move { Ok(result) }
    }

    fn srem(&self, key: &str, member: &str) -> impl Future<Output = Result<bool>> + Send + '_ {
        let result = match self.set_store.get(key) {
            Some(set) => set.remove(member).is_some(),
            _ => false,
        };
        async move { Ok(result) }
    }

    fn smembers(&self, key: &str) -> impl Future<Output = Result<Vec<String>>> + Send + '_ {
        let result = match self.set_store.get(key) {
            Some(set) => {
                // Pre-allocate Vec with exact capacity
                let mut members = Vec::with_capacity(set.len());
                for entry in set.iter() {
                    members.push(entry.key().clone());
                }
                members
            },
            _ => Vec::new(),
        };
        async move { Ok(result) }
    }

    fn hincrby(
        &self,
        key: &str,
        field: &str,
        increment: i64,
    ) -> impl Future<Output = Result<i64>> + Send + '_ {
        // Use nested entry API for atomic read-modify-write
        // Outer lock: ensures hash exists, inner lock: atomic field update
        let key_owned = key.to_string();
        let field_owned = field.to_string();
        let new_value = {
            let hash = self.hash_store.entry(key_owned.clone()).or_default();

            // Get or create field with atomic update
            let mut entry = hash
                .entry(field_owned.clone())
                .or_insert_with(|| Bytes::from("0"));

            // Parse current value (default to 0 if invalid, matching Redis HINCRBY behavior)
            let current: i64 = match std::str::from_utf8(entry.as_ref()) {
                Ok(s) => s.parse().unwrap_or_else(|_| {
                    tracing::warn!(
                        key = %key_owned,
                        field = %field_owned,
                        value = %s,
                        "hincrby: failed to parse value as i64, defaulting to 0"
                    );
                    0
                }),
                Err(_) => {
                    tracing::warn!(
                        key = %key_owned,
                        field = %field_owned,
                        "hincrby: value is not valid UTF-8, defaulting to 0"
                    );
                    0
                },
            };

            // Calculate and store new value atomically
            // Use itoa for zero-allocation i64 formatting
            let new_val = current + increment;
            *entry = i64_to_bytes(new_val);
            new_val
        };

        async move { Ok(new_value) }
    }

    fn pipeline_hash_mset(
        &self,
        operations: crate::traits::HashMsetOps,
    ) -> impl Future<Output = Result<()>> + Send + '_ {
        // For in-memory implementation, just execute each HSET sequentially
        // This is efficient since it's all in-memory with no network overhead
        for (key, fields) in operations {
            if !fields.is_empty() {
                let hash = self.hash_store.entry(key).or_default();
                for (field, value) in fields {
                    hash.insert(field.to_string(), value);
                }
            }
        }
        async move { Ok(()) }
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;
    use tokio::task::JoinSet;

    /// Spawn `$count` concurrent tasks on a fresh `MemoryRtdb`.
    /// Each task receives an `Arc<MemoryRtdb>` and loop index.
    macro_rules! concurrent_test {
        ($count:expr_2021, |$rtdb:ident, $i:ident| $body:expr_2021) => {{
            let rtdb = Arc::new(MemoryRtdb::new());
            let mut tasks = JoinSet::new();
            for $i in 0..$count as usize {
                let $rtdb = rtdb.clone();
                tasks.spawn(async move { $body });
            }
            while tasks.join_next().await.is_some() {}
            rtdb
        }};
    }

    #[tokio::test]
    async fn test_memory_rtdb_kv_operations() {
        let rtdb = MemoryRtdb::new();

        // Test set and get
        rtdb.set("test:key", Bytes::from("value")).await.unwrap();
        let value = rtdb.get("test:key").await.unwrap();
        assert_eq!(value, Some(Bytes::from("value")));

        // Test exists
        assert!(rtdb.exists("test:key").await.unwrap());
        assert!(!rtdb.exists("nonexistent").await.unwrap());

        // Test delete
        assert!(rtdb.del("test:key").await.unwrap());
        assert!(!rtdb.exists("test:key").await.unwrap());
        assert!(!rtdb.del("nonexistent").await.unwrap());
    }

    #[tokio::test]
    async fn test_memory_rtdb_hash_operations() {
        let rtdb = MemoryRtdb::new();

        // Test hash set/get
        rtdb.hash_set("test:hash", "field1", Bytes::from("value1"))
            .await
            .unwrap();
        rtdb.hash_set("test:hash", "field2", Bytes::from("value2"))
            .await
            .unwrap();

        let value = rtdb.hash_get("test:hash", "field1").await.unwrap();
        assert_eq!(value, Some(Bytes::from("value1")));

        // Test hash_get_all
        let all = rtdb.hash_get_all("test:hash").await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all.get("field1"), Some(&Bytes::from("value1")));

        // Test hash_mset
        rtdb.hash_mset(
            "test:hash2",
            vec![
                ("f1".to_string(), Bytes::from("v1")),
                ("f2".to_string(), Bytes::from("v2")),
            ],
        )
        .await
        .unwrap();

        let all2 = rtdb.hash_get_all("test:hash2").await.unwrap();
        assert_eq!(all2.len(), 2);

        // Test hash_del
        assert!(rtdb.hash_del("test:hash", "field1").await.unwrap());
        assert!(!rtdb.hash_del("test:hash", "nonexistent").await.unwrap());
    }

    #[tokio::test]
    async fn test_memory_rtdb_list_operations() {
        let rtdb = MemoryRtdb::new();

        // Test lpush and lpop
        rtdb.list_lpush("test:list", Bytes::from("value1"))
            .await
            .unwrap();
        rtdb.list_lpush("test:list", Bytes::from("value2"))
            .await
            .unwrap();

        let value = rtdb.list_lpop("test:list").await.unwrap();
        assert_eq!(value, Some(Bytes::from("value2")));

        // Test rpush
        rtdb.list_rpush("test:list", Bytes::from("value3"))
            .await
            .unwrap();

        // Test list_range
        let range = rtdb.list_range("test:list", 0, -1).await.unwrap();
        assert_eq!(range.len(), 2);
        assert_eq!(range[0], Bytes::from("value1"));
        assert_eq!(range[1], Bytes::from("value3"));

        // Test list_trim
        rtdb.list_trim("test:list", 0, 0).await.unwrap();
        let range = rtdb.list_range("test:list", 0, -1).await.unwrap();
        assert_eq!(range.len(), 1);
    }

    #[tokio::test]
    async fn test_memory_rtdb_stats() {
        let rtdb = MemoryRtdb::new();

        rtdb.set("key1", Bytes::from("value")).await.unwrap();
        rtdb.hash_set("hash1", "field", Bytes::from("value"))
            .await
            .unwrap();
        rtdb.list_lpush("list1", Bytes::from("value"))
            .await
            .unwrap();

        let stats = rtdb.stats();
        assert_eq!(stats.kv_count, 1);
        assert_eq!(stats.hash_count, 1);
        assert_eq!(stats.list_count, 1);

        rtdb.clear();
        let stats = rtdb.stats();
        assert_eq!(stats.kv_count, 0);
        assert_eq!(stats.hash_count, 0);
        assert_eq!(stats.list_count, 0);
    }

    // ========== Boundary Case Tests ==========

    #[tokio::test]
    async fn test_empty_key_operations() {
        let rtdb = MemoryRtdb::new();

        // Empty key should work
        rtdb.set("", Bytes::from("value")).await.unwrap();
        let value = rtdb.get("").await.unwrap();
        assert_eq!(value, Some(Bytes::from("value")));

        // Empty hash key
        rtdb.hash_set("", "field", Bytes::from("value"))
            .await
            .unwrap();
        let value = rtdb.hash_get("", "field").await.unwrap();
        assert_eq!(value, Some(Bytes::from("value")));
    }

    #[tokio::test]
    async fn test_empty_value_operations() {
        let rtdb = MemoryRtdb::new();

        // Empty value
        rtdb.set("key", Bytes::from("")).await.unwrap();
        let value = rtdb.get("key").await.unwrap();
        assert_eq!(value, Some(Bytes::from("")));

        // Empty hash field value
        rtdb.hash_set("hash", "field", Bytes::from(""))
            .await
            .unwrap();
        let value = rtdb.hash_get("hash", "field").await.unwrap();
        assert_eq!(value, Some(Bytes::from("")));
    }

    #[tokio::test]
    async fn test_nonexistent_key_operations() {
        let rtdb = MemoryRtdb::new();

        // Get nonexistent key
        let value = rtdb.get("nonexistent").await.unwrap();
        assert_eq!(value, None);

        // Delete nonexistent key
        assert!(!rtdb.del("nonexistent").await.unwrap());

        // Hash operations on nonexistent key
        let value = rtdb.hash_get("nonexistent", "field").await.unwrap();
        assert_eq!(value, None);

        let all = rtdb.hash_get_all("nonexistent").await.unwrap();
        assert_eq!(all.len(), 0);

        // List operations on nonexistent key
        let value = rtdb.list_lpop("nonexistent").await.unwrap();
        assert_eq!(value, None);

        let range = rtdb.list_range("nonexistent", 0, -1).await.unwrap();
        assert_eq!(range.len(), 0);
    }

    #[tokio::test]
    async fn test_invalid_utf8_handling() {
        let rtdb = MemoryRtdb::new();

        // Store invalid UTF-8 bytes
        let invalid_utf8 = vec![0xFF, 0xFE, 0xFD];
        rtdb.set("invalid", Bytes::from(invalid_utf8.clone()))
            .await
            .unwrap();

        let value = rtdb.get("invalid").await.unwrap().unwrap();
        assert_eq!(value.to_vec(), invalid_utf8);
    }

    #[tokio::test]
    async fn test_list_range_boundaries() {
        let rtdb = MemoryRtdb::new();

        // Populate list
        for i in 0..5 {
            rtdb.list_rpush("list", Bytes::from(format!("value{}", i)))
                .await
                .unwrap();
        }

        // Negative indices
        let range = rtdb.list_range("list", -2, -1).await.unwrap();
        assert_eq!(range.len(), 2);
        assert_eq!(range[0], Bytes::from("value3"));
        assert_eq!(range[1], Bytes::from("value4"));

        // Out of bounds (should return empty or partial)
        let range = rtdb.list_range("list", 10, 20).await.unwrap();
        assert_eq!(range.len(), 0);

        // Reverse range (start > stop)
        let range = rtdb.list_range("list", 3, 1).await.unwrap();
        assert_eq!(range.len(), 0);
    }

    #[tokio::test]
    async fn test_list_trim_boundaries() {
        let rtdb = MemoryRtdb::new();

        // Populate list
        for i in 0..5 {
            rtdb.list_rpush("list", Bytes::from(format!("value{}", i)))
                .await
                .unwrap();
        }

        // Trim to negative range
        rtdb.list_trim("list", -3, -1).await.unwrap();
        let range = rtdb.list_range("list", 0, -1).await.unwrap();
        assert_eq!(range.len(), 3);

        // Trim out of bounds (should clear)
        rtdb.list_trim("list", 10, 20).await.unwrap();
        let range = rtdb.list_range("list", 0, -1).await.unwrap();
        assert_eq!(range.len(), 0);
    }

    #[tokio::test]
    async fn test_empty_hash_mset() {
        let rtdb = MemoryRtdb::new();

        // Empty hash_mset should work
        rtdb.hash_mset("hash", vec![]).await.unwrap();
        let all = rtdb.hash_get_all("hash").await.unwrap();
        assert_eq!(all.len(), 0);
    }

    #[tokio::test]
    async fn test_large_field_names() {
        let rtdb = MemoryRtdb::new();

        // Very long key
        let long_key = "a".repeat(1000);
        rtdb.set(&long_key, Bytes::from("value")).await.unwrap();
        let value = rtdb.get(&long_key).await.unwrap();
        assert_eq!(value, Some(Bytes::from("value")));

        // Very long hash field
        let long_field = "f".repeat(1000);
        rtdb.hash_set("hash", &long_field, Bytes::from("value"))
            .await
            .unwrap();
        let value = rtdb.hash_get("hash", &long_field).await.unwrap();
        assert_eq!(value, Some(Bytes::from("value")));
    }

    // ========== Concurrent Access Tests ==========

    #[tokio::test]
    async fn test_concurrent_kv_operations() {
        let rtdb = concurrent_test!(100, |r, i| {
            r.set(&format!("key{}", i), Bytes::from(format!("value{}", i)))
                .await
                .unwrap();
        });
        for i in 0..100 {
            let value = rtdb.get(&format!("key{}", i)).await.unwrap();
            assert_eq!(value, Some(Bytes::from(format!("value{}", i))));
        }
    }

    // ========== New Method Tests ==========

    #[tokio::test]
    async fn test_set_operations() {
        let rtdb = MemoryRtdb::new();

        // Test sadd - adding new member
        assert!(rtdb.sadd("test:set", "member1").await.unwrap());
        assert!(rtdb.sadd("test:set", "member2").await.unwrap());
        assert!(rtdb.sadd("test:set", "member3").await.unwrap());

        // Test sadd - adding duplicate member (should return false)
        assert!(!rtdb.sadd("test:set", "member1").await.unwrap());

        // Test smembers
        let members = rtdb.smembers("test:set").await.unwrap();
        assert_eq!(members.len(), 3);
        assert!(members.contains(&"member1".to_string()));
        assert!(members.contains(&"member2".to_string()));
        assert!(members.contains(&"member3".to_string()));

        // Test srem - removing existing member
        assert!(rtdb.srem("test:set", "member2").await.unwrap());
        let members = rtdb.smembers("test:set").await.unwrap();
        assert_eq!(members.len(), 2);
        assert!(!members.contains(&"member2".to_string()));

        // Test srem - removing non-existent member
        assert!(!rtdb.srem("test:set", "nonexistent").await.unwrap());

        // Test smembers on non-existent set
        let empty = rtdb.smembers("nonexistent:set").await.unwrap();
        assert_eq!(empty.len(), 0);
    }

    #[tokio::test]
    async fn test_hincrby_operation() {
        let rtdb = MemoryRtdb::new();

        // Test hincrby on non-existent field (should initialize to increment)
        let result = rtdb.hincrby("test:hash", "counter", 5).await.unwrap();
        assert_eq!(result, 5);

        // Test hincrby on existing field
        let result = rtdb.hincrby("test:hash", "counter", 10).await.unwrap();
        assert_eq!(result, 15);

        // Test negative increment
        let result = rtdb.hincrby("test:hash", "counter", -3).await.unwrap();
        assert_eq!(result, 12);

        // Test increment by zero
        let result = rtdb.hincrby("test:hash", "counter", 0).await.unwrap();
        assert_eq!(result, 12);

        // Test different field in same hash
        let result = rtdb
            .hincrby("test:hash", "other_counter", 100)
            .await
            .unwrap();
        assert_eq!(result, 100);
    }

    #[tokio::test]
    async fn test_stats_includes_set_count() {
        let rtdb = MemoryRtdb::new();

        // Initially all counts should be 0
        let stats = rtdb.stats();
        assert_eq!(stats.set_count, 0);

        // Add some data to different stores
        rtdb.set("key1", Bytes::from("value")).await.unwrap();
        rtdb.hash_set("hash1", "field", Bytes::from("value"))
            .await
            .unwrap();
        rtdb.list_lpush("list1", Bytes::from("value"))
            .await
            .unwrap();
        rtdb.sadd("set1", "member1").await.unwrap();

        let stats = rtdb.stats();
        assert_eq!(stats.kv_count, 1);
        assert_eq!(stats.hash_count, 1);
        assert_eq!(stats.list_count, 1);
        assert_eq!(stats.set_count, 1);

        // Clear and verify all counts reset
        rtdb.clear();
        let stats = rtdb.stats();
        assert_eq!(stats.kv_count, 0);
        assert_eq!(stats.hash_count, 0);
        assert_eq!(stats.list_count, 0);
        assert_eq!(stats.set_count, 0);
    }

    #[tokio::test]
    async fn test_concurrent_set_operations() {
        let rtdb = concurrent_test!(50, |r, i| {
            r.sadd("shared_set", &format!("member{}", i)).await.unwrap();
        });
        let members = rtdb.smembers("shared_set").await.unwrap();
        assert_eq!(members.len(), 50);
    }

    #[tokio::test]
    async fn test_concurrent_hincrby_operations() {
        let rtdb = concurrent_test!(100, |r, _i| {
            r.hincrby("test:hash", "counter", 1).await.unwrap();
        });
        let result = rtdb.hincrby("test:hash", "counter", 0).await.unwrap();
        assert_eq!(result, 100);
    }

    #[tokio::test]
    async fn test_concurrent_hash_operations() {
        let rtdb = concurrent_test!(50, |r, i| {
            r.hash_set(
                "shared_hash",
                &format!("field{}", i),
                Bytes::from(format!("value{}", i)),
            )
            .await
            .unwrap();
        });
        let all = rtdb.hash_get_all("shared_hash").await.unwrap();
        assert_eq!(all.len(), 50);
    }

    #[tokio::test]
    async fn test_concurrent_list_operations() {
        let rtdb = concurrent_test!(100, |r, i| {
            r.list_lpush("shared_list", Bytes::from(format!("value{}", i)))
                .await
                .unwrap();
        });
        let range = rtdb.list_range("shared_list", 0, -1).await.unwrap();
        assert_eq!(range.len(), 100);
    }

    #[tokio::test]
    async fn test_concurrent_mixed_operations() {
        let rtdb = concurrent_test!(50, |r, i| {
            r.set(&format!("key{}", i), Bytes::from(format!("value{}", i)))
                .await
                .unwrap();
            let _ = r.get(&format!("key{}", i)).await;
            r.del(&format!("key{}", i)).await.unwrap();
        });
        for i in 0..50 {
            assert!(!rtdb.exists(&format!("key{}", i)).await.unwrap());
        }
    }

    // ========== Multi-threaded Concurrency Tests ==========
    // These tests use multi_thread runtime to actually test concurrent access

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_concurrent_hincrby_multithread() {
        let rtdb = concurrent_test!(1000, |r, _i| {
            r.hincrby("test:hash", "counter", 1).await.unwrap();
        });
        let result = rtdb.hincrby("test:hash", "counter", 0).await.unwrap();
        assert_eq!(result, 1000, "Concurrent increments should sum correctly");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_concurrent_incrbyfloat_multithread() {
        let rtdb = concurrent_test!(1000, |r, _i| {
            r.incrbyfloat("counter", 1.0).await.unwrap();
        });
        let result = rtdb.incrbyfloat("counter", 0.0).await.unwrap();
        assert!(
            (result - 1000.0).abs() < 0.001,
            "Expected 1000.0, got {}",
            result
        );
    }
}
