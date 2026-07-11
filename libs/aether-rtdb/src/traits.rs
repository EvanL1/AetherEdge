//! Trait definitions for RTDB abstraction

use anyhow::Result;
use bytes::Bytes;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

/// Batched hash-mset operations: Vec of (key, Vec of (field, value))
/// Field names use `Arc<str>` to avoid heap allocation on the hot path.
pub type HashMsetOps = Vec<(String, Vec<(Arc<str>, Bytes)>)>;

/// Unified RTDB Storage Trait
///
/// Provides complete storage interface for AetherEMS, combining:
/// - Basic key-value operations
/// - Structured data (Hash, List, Set)
/// - Pipeline operations for batch writes
///
/// Implementations:
/// - `RedisRtdb`: Production Redis backend
/// - `MemoryRtdb`: In-memory backend for testing
///
/// # Value contract (IMPORTANT)
///
/// The `Bytes` type appears in many signatures (`set`, `hash_set`, `hash_mset`,
/// `pipeline_hash_mset`) but the **production Redis backend requires valid
/// UTF-8** for the Redis protocol path used here (`SET`/`HSET` go through
/// `fred`'s string API). Callers must pass UTF-8 bytes — typically
/// `Bytes::from(string_data)` or `f64_to_bytes(value)` which emit ASCII.
///
/// Non-UTF-8 input is rejected at runtime with a clear `UTF-8 conversion failed`
/// error rather than silently corrupting the value or panicking. The
/// in-memory backend (used in tests) accepts any bytes, so writing a test
/// that passes binary data will pass against `MemoryRtdb` but fail against
/// `RedisRtdb` — this is the contract.
///
/// A future cleanup should narrow the signatures to `&str`/`String` to make
/// this contract a compile-time check; tracking issue: see review round 4
/// finding #10.
///
/// Note: All async methods use explicit lifetime `'a` to enable zero-copy parameter passing.
/// The returned Future borrows both `&self` and parameters for the same lifetime,
/// allowing implementations to use borrowed data directly without cloning.
pub trait Rtdb: Send + Sync + 'static {
    // ========== Basic Key-Value Operations ==========

    /// Get value by key
    fn get<'a>(&'a self, key: &'a str) -> impl Future<Output = Result<Option<Bytes>>> + Send + 'a;

    /// Set value for key
    fn set<'a>(
        &'a self,
        key: &'a str,
        value: Bytes,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    /// Delete key
    fn del<'a>(&'a self, key: &'a str) -> impl Future<Output = Result<bool>> + Send + 'a;

    /// Check if key exists
    fn exists<'a>(&'a self, key: &'a str) -> impl Future<Output = Result<bool>> + Send + 'a;

    /// Increment key by float value (Redis INCRBYFLOAT)
    ///
    /// Returns the new value after incrementing.
    ///
    /// # Behavior
    ///
    /// - If the key does not exist, it is initialized to 0.0 before incrementing.
    /// - If the current value cannot be parsed as f64, it is treated as 0.0.
    ///
    /// # Implementation Notes
    ///
    /// - **RedisRtdb**: Delegates to Redis INCRBYFLOAT, which returns an error if the value is not a valid float.
    /// - **MemoryRtdb**: Silently defaults to 0.0 on parse failure (logs at trace level).
    ///
    /// For test consistency, ensure stored values are always valid numeric strings.
    fn incrbyfloat<'a>(
        &'a self,
        key: &'a str,
        increment: f64,
    ) -> impl Future<Output = Result<f64>> + Send + 'a;

    // ========== Hash Operations ==========

    /// Set hash field
    fn hash_set<'a>(
        &'a self,
        key: &'a str,
        field: &'a str,
        value: Bytes,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    /// Set hash field only if it does not already exist (Redis HSETNX).
    ///
    /// Returns Ok(true) when the field was inserted, Ok(false) when it
    /// already existed (and was left untouched).
    ///
    /// Use this for structural initialization that must not clobber a
    /// concurrently-written real-time value. Example: automation bootstrap
    /// inserts `inst:{id}:M:{point} = "0"` for every defined point on
    /// startup, but io may already have flushed a fresh reading via
    /// ShmRedisSync. Plain HSET would overwrite that reading with 0.
    fn hash_setnx<'a>(
        &'a self,
        key: &'a str,
        field: &'a str,
        value: Bytes,
    ) -> impl Future<Output = Result<bool>> + Send + 'a;

    /// Get hash field
    fn hash_get<'a>(
        &'a self,
        key: &'a str,
        field: &'a str,
    ) -> impl Future<Output = Result<Option<Bytes>>> + Send + 'a;

    /// Get multiple hash fields (Redis HMGET)
    ///
    /// Returns a vector of values corresponding to the requested fields.
    /// Non-existent fields are returned as None.
    fn hash_mget<'a>(
        &'a self,
        key: &'a str,
        fields: &'a [&'a str],
    ) -> impl Future<Output = Result<Vec<Option<Bytes>>>> + Send + 'a;

    /// Set multiple hash fields
    fn hash_mset<'a>(
        &'a self,
        key: &'a str,
        fields: Vec<(String, Bytes)>,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    /// Get all hash fields
    fn hash_get_all<'a>(
        &'a self,
        key: &'a str,
    ) -> impl Future<Output = Result<HashMap<String, Bytes>>> + Send + 'a;

    /// Set hash field with f64 value directly
    ///
    /// This is an optimized version of `hash_set` for numeric values.
    /// The default implementation converts f64 to string and calls `hash_set`.
    /// Concrete implementations may override this for better performance
    /// (e.g., using `ryu` for faster float-to-string conversion).
    fn hash_set_f64<'a>(
        &'a self,
        key: &'a str,
        field: &'a str,
        value: f64,
    ) -> impl Future<Output = Result<()>> + Send + 'a {
        async move {
            self.hash_set(key, field, Bytes::from(value.to_string()))
                .await
        }
    }

    /// Delete hash field
    fn hash_del<'a>(
        &'a self,
        key: &'a str,
        field: &'a str,
    ) -> impl Future<Output = Result<bool>> + Send + 'a;

    /// Delete multiple hash fields at once (Redis HDEL with multiple fields)
    ///
    /// This is more efficient than multiple individual hash_del calls as it uses
    /// a single Redis command to delete all specified fields.
    ///
    /// Returns the number of fields that were removed.
    fn hash_del_many<'a>(
        &'a self,
        key: &'a str,
        fields: &'a [String],
    ) -> impl Future<Output = Result<usize>> + Send + 'a;

    /// Delete multiple hash fields using string slices (convenience wrapper)
    ///
    /// This is a convenience method that avoids the need to convert `&[&str]` to `Vec<String>`.
    ///
    /// # Example
    ///
    /// ```ignore
    /// rtdb.hash_del_many_str("my_hash", &["field1", "field2", "field3"]).await?;
    /// ```
    fn hash_del_many_str<'a>(
        &'a self,
        key: &'a str,
        fields: &'a [&'a str],
    ) -> impl Future<Output = Result<usize>> + Send + 'a {
        let key = key.to_string();
        let fields: Vec<String> = fields.iter().copied().map(String::from).collect();
        async move { self.hash_del_many(&key, &fields).await }
    }

    /// Increment hash field by value (Redis HINCRBY)
    ///
    /// Returns the new value after incrementing.
    ///
    /// # Behavior
    ///
    /// - If the hash or field does not exist, it is initialized to 0 before incrementing.
    /// - If the current value cannot be parsed as i64, it is treated as 0.
    ///
    /// # Implementation Notes
    ///
    /// - **RedisRtdb**: Delegates to Redis HINCRBY, which returns an error if the value is not a valid integer.
    /// - **MemoryRtdb**: Silently defaults to 0 on parse failure (logs at trace level).
    ///
    /// For test consistency, ensure stored values are always valid numeric strings.
    fn hincrby<'a>(
        &'a self,
        key: &'a str,
        field: &'a str,
        increment: i64,
    ) -> impl Future<Output = Result<i64>> + Send + 'a;

    // ========== List Operations ==========

    /// Push value to left of list
    fn list_lpush<'a>(
        &'a self,
        key: &'a str,
        value: Bytes,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    /// Push value to right of list
    fn list_rpush<'a>(
        &'a self,
        key: &'a str,
        value: Bytes,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    /// Pop value from left of list
    fn list_lpop<'a>(
        &'a self,
        key: &'a str,
    ) -> impl Future<Output = Result<Option<Bytes>>> + Send + 'a;

    /// Pop value from right of list (Redis RPOP)
    ///
    /// Returns the popped value if the list is not empty, None otherwise.
    fn list_rpop<'a>(
        &'a self,
        key: &'a str,
    ) -> impl Future<Output = Result<Option<Bytes>>> + Send + 'a;

    /// Block and pop value from multiple lists (Redis BLPOP)
    ///
    /// Blocks until a value is available in one of the specified lists,
    /// or until the timeout expires.
    ///
    /// # Arguments
    /// * `keys` - List of keys to wait on
    /// * `timeout_seconds` - Timeout in seconds (0 = block indefinitely)
    ///
    /// # Returns
    /// * `Some((key, value))` - The key that had data and the popped value
    /// * `None` - Timeout expired without data
    fn list_blpop<'a>(
        &'a self,
        keys: &'a [&'a str],
        timeout_seconds: u64,
    ) -> impl Future<Output = Result<Option<(String, Bytes)>>> + Send + 'a;

    /// Get list range
    fn list_range<'a>(
        &'a self,
        key: &'a str,
        start: isize,
        stop: isize,
    ) -> impl Future<Output = Result<Vec<Bytes>>> + Send + 'a;

    /// Trim list to range
    fn list_trim<'a>(
        &'a self,
        key: &'a str,
        start: isize,
        stop: isize,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    // ========== Set Operations ==========

    /// Add member to set (Redis SADD)
    ///
    /// Returns true if the member was added, false if it already existed.
    fn sadd<'a>(
        &'a self,
        key: &'a str,
        member: &'a str,
    ) -> impl Future<Output = Result<bool>> + Send + 'a;

    /// Remove member from set (Redis SREM)
    ///
    /// Returns true if the member was removed, false if it didn't exist.
    fn srem<'a>(
        &'a self,
        key: &'a str,
        member: &'a str,
    ) -> impl Future<Output = Result<bool>> + Send + 'a;

    /// Get all members of a set (Redis SMEMBERS)
    ///
    /// Returns a vector of all members in the set.
    fn smembers<'a>(
        &'a self,
        key: &'a str,
    ) -> impl Future<Output = Result<Vec<String>>> + Send + 'a;

    // ========== Key Scanning Operations ==========

    /// Scan keys matching a pattern (Redis SCAN with MATCH)
    ///
    /// Returns a list of keys matching the glob pattern.
    /// In test implementations (MemoryRtdb), this searches in-memory keys.
    fn scan_match<'a>(
        &'a self,
        pattern: &'a str,
    ) -> impl Future<Output = Result<Vec<String>>> + Send + 'a;

    // ========== Key Expiry Operations ==========

    /// Set a timeout on key (Redis EXPIRE)
    ///
    /// Returns true if the timeout was set, false if key does not exist.
    /// Default implementation is a no-op that returns false (for test backends).
    fn expire<'a>(
        &'a self,
        key: &'a str,
        seconds: i64,
    ) -> impl Future<Output = Result<bool>> + Send + 'a {
        let _ = (key, seconds);
        async move { Ok(false) }
    }

    // ========== Pipeline Operations ==========

    /// Execute multiple HMSET operations in a single pipeline (pure Redis, no Lua)
    ///
    /// This batches multiple hash write operations into a single network round-trip,
    /// significantly reducing latency for bulk writes.
    ///
    /// # Arguments
    /// * `operations` - Vector of (key, fields) tuples, where fields is Vec<(field, value)>
    ///
    /// # Returns
    /// * `Ok(())` on success
    /// * `Err` if any operation fails
    fn pipeline_hash_mset(
        &self,
        operations: HashMsetOps,
    ) -> impl Future<Output = Result<()>> + Send + '_;

    /// Execute multiple HMSET operations atomically (MULTI/EXEC).
    ///
    /// Unlike `pipeline_hash_mset` which just queues commands on the
    /// connection (any partial-flush would leave Redis in a torn state),
    /// this variant wraps the batch in MULTI/EXEC so all writes commit
    /// together or not at all. Use for cases where readers depend on
    /// cross-key consistency, e.g. `inst:{id}:M` + `inst:{id}:M:ts`
    /// where a torn write produces new values paired with old timestamps.
    ///
    /// Default implementation delegates to `pipeline_hash_mset` for
    /// backends where the operation is already atomic (e.g. in-memory
    /// Mutex-guarded backends).
    fn pipeline_hash_mset_atomic(
        &self,
        operations: HashMsetOps,
    ) -> impl Future<Output = Result<()>> + Send + '_ {
        self.pipeline_hash_mset(operations)
    }
}
