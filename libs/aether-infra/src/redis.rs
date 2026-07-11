//! Redis client module with connection pooling
//!
//! Provides minimal async Redis client with only the methods actually used

use anyhow::{Context, Result};
use bb8::{Pool, PooledConnection};
use bb8_redis::RedisConnectionManager;
use bytes::Bytes;
use redis::AsyncCommands;
use std::sync::Arc;
use std::time::Duration;

// Re-export commonly used types from redis crate
pub use redis::Msg;

/// Redis connection pool configuration
#[derive(Debug, Clone)]
pub struct RedisPoolConfig {
    /// Redis URL (e.g., "redis://localhost:6379")
    pub url: String,
    /// Maximum number of connections in the pool
    pub max_connections: u32,
    /// Minimum number of idle connections
    pub min_idle: Option<u32>,
    /// Connection timeout in seconds
    pub connection_timeout: u64,
    /// Maximum lifetime of a connection in seconds
    pub max_lifetime: Option<u64>,
    /// Idle timeout in seconds
    pub idle_timeout: Option<u64>,
}

impl Default for RedisPoolConfig {
    fn default() -> Self {
        Self {
            url: "redis://localhost:6379".to_string(),
            max_connections: 50,
            min_idle: Some(10),
            connection_timeout: 5,
            max_lifetime: Some(3600), // 1 hour
            idle_timeout: Some(600),  // 10 minutes
        }
    }
}

impl RedisPoolConfig {
    /// Create config from URL with default pool settings
    pub fn from_url(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            ..Default::default()
        }
    }
}

/// Redis asynchronous client with connection pooling
#[derive(Clone)]
pub struct RedisClient {
    pool: Arc<Pool<RedisConnectionManager>>,
    url: String,
}

impl std::fmt::Debug for RedisClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisClient")
            .field("url", &self.url)
            .field("pool_state", &self.pool.state())
            .finish()
    }
}

impl RedisClient {
    /// Build a connection pool from config (shared by with_config and with_config_no_ping)
    async fn build_pool(config: &RedisPoolConfig) -> Result<Arc<Pool<RedisConnectionManager>>> {
        let manager = RedisConnectionManager::new(config.url.as_str())
            .context("Failed to create Redis connection manager")?;

        let mut pool_builder = Pool::builder()
            .max_size(config.max_connections)
            .connection_timeout(Duration::from_secs(config.connection_timeout));

        if let Some(min_idle) = config.min_idle {
            pool_builder = pool_builder.min_idle(Some(min_idle));
        }

        if let Some(max_lifetime) = config.max_lifetime {
            pool_builder = pool_builder.max_lifetime(Some(Duration::from_secs(max_lifetime)));
        }

        if let Some(idle_timeout) = config.idle_timeout {
            pool_builder = pool_builder.idle_timeout(Some(Duration::from_secs(idle_timeout)));
        }

        let pool = pool_builder
            .build(manager)
            .await
            .context("Failed to build Redis connection pool")?;

        Ok(Arc::new(pool))
    }

    /// Create a new client with default configuration
    pub async fn new(url: &str) -> Result<Self> {
        Self::with_config(RedisPoolConfig::from_url(url)).await
    }

    /// Create a new client with custom configuration
    pub async fn with_config(config: RedisPoolConfig) -> Result<Self> {
        let pool = Self::build_pool(&config).await?;

        // Test the connection
        {
            let mut conn = pool
                .get()
                .await
                .context("Failed to get connection from pool for testing")?;
            let _: String = redis::cmd("PING")
                .query_async(&mut *conn)
                .await
                .context("Failed to ping Redis server")?;
        }

        Ok(Self {
            pool,
            url: config.url,
        })
    }

    /// Create a client without performing a PING test (for tests or special cases)
    /// This avoids requiring a live Redis server when the client won't be used.
    pub async fn with_config_no_ping(config: RedisPoolConfig) -> Result<Self> {
        let pool = Self::build_pool(&config).await?;
        Ok(Self {
            pool,
            url: config.url,
        })
    }

    /// Convenience helper to create client without ping from URL
    pub async fn new_unchecked(url: &str) -> Result<Self> {
        Self::with_config_no_ping(RedisPoolConfig::from_url(url)).await
    }

    /// Get a connection from the pool
    ///
    /// This is useful for calling Redis commands or other operations
    /// not provided by the RedisClient API.
    pub async fn get_connection(&self) -> Result<PooledConnection<'_, RedisConnectionManager>> {
        self.pool
            .get()
            .await
            .context("Failed to get connection from pool")
    }

    /// GET operation
    pub async fn get<T: redis::FromRedisValue>(&self, key: &str) -> Result<Option<T>> {
        let mut conn = self.get_connection().await?;
        conn.get(key)
            .await
            .with_context(|| format!("Failed to GET key: {}", key))
    }

    /// SET operation
    pub async fn set<T: redis::ToRedisArgs + Send + Sync>(
        &self,
        key: &str,
        value: T,
    ) -> Result<()> {
        let mut conn = self.get_connection().await?;
        conn.set(key, value)
            .await
            .with_context(|| format!("Failed to SET key: {}", key))
    }

    /// PUBLISH operation
    pub async fn publish(&self, channel: &str, message: &str) -> Result<u32> {
        let mut conn = self.get_connection().await?;
        conn.publish(channel, message)
            .await
            .with_context(|| format!("Failed to PUBLISH to channel: {}", channel))
    }

    /// PING operation - test connection
    pub async fn ping(&self) -> Result<String> {
        let mut conn = self.get_connection().await?;
        redis::cmd("PING")
            .query_async(&mut *conn)
            .await
            .context("Failed to PING Redis server")
    }

    /// BLPOP operation - blocking list pop
    /// Returns Some((key, value)) or None (timeout)
    pub async fn blpop(&self, keys: &[&str], timeout: usize) -> Result<Option<(String, String)>> {
        let mut conn = self.get_connection().await?;
        redis::cmd("BLPOP")
            .arg(keys)
            .arg(timeout)
            .query_async(&mut *conn)
            .await
            .with_context(|| format!("Failed to BLPOP from keys: {:?}", keys))
    }

    /// RPUSH operation - add element to list tail
    pub async fn rpush(&self, key: &str, value: &str) -> Result<u32> {
        let mut conn = self.get_connection().await?;
        conn.rpush(key, value)
            .await
            .with_context(|| format!("Failed to RPUSH to key: {}", key))
    }

    /// LPUSH operation - add element to list head
    pub async fn lpush<T: redis::ToRedisArgs + Send + Sync>(
        &self,
        key: &str,
        value: T,
    ) -> Result<u32> {
        let mut conn = self.get_connection().await?;
        conn.lpush(key, value)
            .await
            .with_context(|| format!("Failed to LPUSH to key: {}", key))
    }

    /// LTRIM operation - trim list to specified range
    pub async fn ltrim(&self, key: &str, start: isize, stop: isize) -> Result<()> {
        let mut conn = self.get_connection().await?;
        redis::cmd("LTRIM")
            .arg(key)
            .arg(start)
            .arg(stop)
            .query_async(&mut *conn)
            .await
            .with_context(|| format!("Failed to LTRIM key: {}", key))
    }

    /// LRANGE operation - get list elements in range
    pub async fn lrange<T: redis::FromRedisValue>(
        &self,
        key: &str,
        start: isize,
        stop: isize,
    ) -> Result<Vec<T>> {
        let mut conn = self.get_connection().await?;
        redis::cmd("LRANGE")
            .arg(key)
            .arg(start)
            .arg(stop)
            .query_async(&mut *conn)
            .await
            .with_context(|| format!("Failed to LRANGE key: {}", key))
    }

    /// INCRBYFLOAT operation - increment a float value and return the new value
    pub async fn incrbyfloat(&self, key: &str, increment: f64) -> Result<f64> {
        let mut conn = self.get_connection().await?;
        redis::cmd("INCRBYFLOAT")
            .arg(key)
            .arg(increment)
            .query_async(&mut *conn)
            .await
            .with_context(|| format!("Failed to INCRBYFLOAT key: {}", key))
    }

    /// Hash operation - set field
    pub async fn hset(&self, key: &str, field: &str, value: String) -> Result<()> {
        let mut conn = self.get_connection().await?;
        redis::cmd("HSET")
            .arg(key)
            .arg(field)
            .arg(value)
            .query_async(&mut *conn)
            .await
            .with_context(|| format!("Failed to HSET field {} in key: {}", field, key))
    }

    /// Hash set if field does not exist (Redis HSETNX).
    ///
    /// Returns Ok(true) if the field was created, Ok(false) if the field
    /// already existed (left untouched). Use for structural initialization
    /// that must not overwrite a concurrently-written value.
    pub async fn hsetnx(&self, key: &str, field: &str, value: String) -> Result<bool> {
        let mut conn = self.get_connection().await?;
        let inserted: i64 = redis::cmd("HSETNX")
            .arg(key)
            .arg(field)
            .arg(value)
            .query_async(&mut *conn)
            .await
            .with_context(|| format!("Failed to HSETNX field {} in key: {}", field, key))?;
        Ok(inserted == 1)
    }

    /// Hash operation - increment field by integer value
    pub async fn hincrby(&self, key: &str, field: &str, increment: i64) -> Result<i64> {
        let mut conn = self.get_connection().await?;
        redis::cmd("HINCRBY")
            .arg(key)
            .arg(field)
            .arg(increment)
            .query_async(&mut *conn)
            .await
            .with_context(|| format!("Failed to HINCRBY field {} in key: {}", field, key))
    }

    /// Hash operation - get field
    pub async fn hget<T: redis::FromRedisValue>(
        &self,
        key: &str,
        field: &str,
    ) -> Result<Option<T>> {
        let mut conn = self.get_connection().await?;
        redis::cmd("HGET")
            .arg(key)
            .arg(field)
            .query_async(&mut *conn)
            .await
            .with_context(|| format!("Failed to HGET field {} from key: {}", field, key))
    }

    /// HGET operation returning raw Bytes (skips UTF-8 validation)
    pub async fn hget_bytes(&self, key: &str, field: &str) -> Result<Option<Bytes>> {
        let mut conn = self.get_connection().await?;
        let result: Option<Vec<u8>> = redis::cmd("HGET")
            .arg(key)
            .arg(field)
            .query_async(&mut *conn)
            .await
            .with_context(|| format!("Failed to HGET field {} from key: {}", field, key))?;
        Ok(result.map(Bytes::from))
    }

    /// Set operation - add member to set
    pub async fn sadd(&self, key: &str, member: &str) -> Result<bool> {
        let mut conn = self.get_connection().await?;
        let added: i32 = redis::cmd("SADD")
            .arg(key)
            .arg(member)
            .query_async(&mut *conn)
            .await
            .with_context(|| format!("Failed to SADD member {} to key: {}", member, key))?;
        Ok(added > 0)
    }

    /// Set operation - remove member from set
    pub async fn srem(&self, key: &str, member: &str) -> Result<bool> {
        let mut conn = self.get_connection().await?;
        let removed: i32 = redis::cmd("SREM")
            .arg(key)
            .arg(member)
            .query_async(&mut *conn)
            .await
            .with_context(|| format!("Failed to SREM member {} from key: {}", member, key))?;
        Ok(removed > 0)
    }

    /// Set operation - retrieve all members
    pub async fn smembers(&self, key: &str) -> Result<Vec<String>> {
        let mut conn = self.get_connection().await?;
        redis::cmd("SMEMBERS")
            .arg(key)
            .query_async(&mut *conn)
            .await
            .with_context(|| format!("Failed to SMEMBERS from key: {}", key))
    }

    /// Return Redis server time in milliseconds
    pub async fn time_millis(&self) -> Result<i64> {
        let mut conn = self.get_connection().await?;
        let (seconds, microseconds): (i64, i64) = redis::cmd("TIME")
            .query_async(&mut *conn)
            .await
            .with_context(|| "Failed to fetch Redis TIME command")?;
        Ok(seconds
            .saturating_mul(1000)
            .saturating_add(microseconds / 1000))
    }

    /// Hash operation - batch set multiple fields (alias for hmset compatibility)
    pub async fn hmset(&self, key: &str, fields: &[(String, String)]) -> Result<()> {
        if fields.is_empty() {
            return Ok(());
        }

        let mut conn = self.get_connection().await?;
        let mut cmd = redis::cmd("HSET");
        cmd.arg(key);
        for (field, value) in fields {
            cmd.arg(field).arg(value);
        }
        cmd.query_async(&mut *conn)
            .await
            .with_context(|| format!("Failed to HMSET in key: {}", key))
    }

    /// Hash operation - get multiple fields
    pub async fn hmget<T: redis::FromRedisValue>(
        &self,
        key: &str,
        fields: &[&str],
    ) -> Result<Vec<Option<T>>> {
        let mut conn = self.get_connection().await?;
        let mut cmd = redis::cmd("HMGET");
        cmd.arg(key);
        for field in fields {
            cmd.arg(*field);
        }
        cmd.query_async(&mut *conn)
            .await
            .with_context(|| format!("Failed to HMGET fields {:?} from key: {}", fields, key))
    }

    /// Hash operation - get all fields
    pub async fn hgetall<T: redis::FromRedisValue>(&self, key: &str) -> Result<T> {
        let mut conn = self.get_connection().await?;
        redis::cmd("HGETALL")
            .arg(key)
            .query_async(&mut *conn)
            .await
            .with_context(|| format!("Failed to HGETALL from key: {}", key))
    }

    /// Use SCAN for production-safe key iteration
    pub async fn scan_match(&self, pattern: &str) -> Result<Vec<String>> {
        let mut conn = self.get_connection().await?;
        let mut keys = Vec::new();
        let mut cursor = 0u64;

        loop {
            let (new_cursor, batch): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(pattern)
                .arg("COUNT")
                .arg(100)
                .query_async(&mut *conn)
                .await
                .with_context(|| format!("Failed to SCAN with pattern: {}", pattern))?;

            keys.extend(batch);
            cursor = new_cursor;

            if cursor == 0 {
                break;
            }
        }

        Ok(keys)
    }

    /// Delete one or more keys
    pub async fn del(&self, keys: &[&str]) -> Result<u32> {
        let mut conn = self.get_connection().await?;
        let mut cmd = redis::cmd("DEL");
        for key in keys {
            cmd.arg(*key);
        }
        cmd.query_async(&mut *conn)
            .await
            .with_context(|| format!("Failed to DEL keys: {:?}", keys))
    }

    /// Call Redis function (for Lua functions)
    pub async fn fcall<T: redis::FromRedisValue>(
        &self,
        function: &str,
        keys: &[&str],
        args: &[&str],
    ) -> Result<T> {
        let mut conn = self.get_connection().await?;
        let mut cmd = redis::cmd("FCALL");
        cmd.arg(function).arg(keys.len());
        for key in keys {
            cmd.arg(*key);
        }
        for arg in args {
            cmd.arg(*arg);
        }
        cmd.query_async(&mut *conn)
            .await
            .with_context(|| format!("Failed to FCALL function: {}", function))
    }

    /// Set a timeout on key (Redis EXPIRE)
    ///
    /// Returns true if the timeout was set, false if key does not exist.
    pub async fn expire(&self, key: &str, seconds: i64) -> Result<bool> {
        let mut conn = self.get_connection().await?;
        redis::cmd("EXPIRE")
            .arg(key)
            .arg(seconds)
            .query_async(&mut *conn)
            .await
            .with_context(|| format!("Failed to EXPIRE key: {}", key))
    }

    /// Check if key exists
    pub async fn exists(&self, key: &str) -> Result<bool> {
        let mut conn = self.get_connection().await?;
        let result: i32 = redis::cmd("EXISTS")
            .arg(key)
            .query_async(&mut *conn)
            .await
            .with_context(|| format!("Failed to EXISTS key: {}", key))?;
        Ok(result > 0)
    }

    /// Pop value from left of list
    pub async fn lpop<T: redis::FromRedisValue>(&self, key: &str) -> Result<Option<T>> {
        let mut conn = self.get_connection().await?;
        redis::cmd("LPOP")
            .arg(key)
            .query_async(&mut *conn)
            .await
            .with_context(|| format!("Failed to LPOP key: {}", key))
    }

    /// Pop value from right of list
    pub async fn rpop<T: redis::FromRedisValue>(&self, key: &str) -> Result<Option<T>> {
        let mut conn = self.get_connection().await?;
        redis::cmd("RPOP")
            .arg(key)
            .query_async(&mut *conn)
            .await
            .with_context(|| format!("Failed to RPOP key: {}", key))
    }

    /// Delete hash field
    pub async fn hdel(&self, key: &str, field: &str) -> Result<bool> {
        let mut conn = self.get_connection().await?;
        let result: i32 = redis::cmd("HDEL")
            .arg(key)
            .arg(field)
            .query_async(&mut *conn)
            .await
            .with_context(|| format!("Failed to HDEL field {} from key: {}", field, key))?;
        Ok(result > 0)
    }

    /// Delete multiple hash fields at once (Redis HDEL with multiple fields)
    ///
    /// This is more efficient than multiple individual hdel calls as it uses
    /// a single Redis command to delete all specified fields.
    ///
    /// Returns the number of fields that were removed.
    pub async fn hdel_many(&self, key: &str, fields: &[String]) -> Result<usize> {
        if fields.is_empty() {
            return Ok(0);
        }

        let mut conn = self.get_connection().await?;
        let mut cmd = redis::cmd("HDEL");
        cmd.arg(key);
        for field in fields {
            cmd.arg(field);
        }
        let result: i64 = cmd
            .query_async(&mut *conn)
            .await
            .with_context(|| format!("Failed to HDEL multiple fields from key: {}", key))?;
        Ok(result.max(0) as usize)
    }

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
    pub async fn pipeline_hmset(
        &self,
        operations: &[(String, Vec<(String, String)>)],
    ) -> Result<()> {
        if operations.is_empty() {
            return Ok(());
        }

        let mut conn = self.get_connection().await?;
        let mut pipe = redis::pipe();

        for (key, fields) in operations {
            if !fields.is_empty() {
                let mut cmd = redis::cmd("HSET");
                cmd.arg(key.as_str());
                for (field, value) in fields {
                    cmd.arg(field.as_str()).arg(value.as_str());
                }
                pipe.add_command(cmd);
            }
        }

        pipe.query_async::<()>(&mut *conn)
            .await
            .with_context(|| "Failed to execute pipeline HMSET")?;

        Ok(())
    }

    /// Atomic variant of pipeline_hmset (MULTI/EXEC).
    ///
    /// Wraps the batch in a Redis transaction so all HMSETs commit
    /// together or not at all. Use where downstream readers expect
    /// cross-key consistency (value + ts pair); rare partial-flush
    /// races on plain pipelines cannot leave Redis in a torn state.
    pub async fn pipeline_hmset_atomic(
        &self,
        operations: &[(String, Vec<(String, String)>)],
    ) -> Result<()> {
        if operations.is_empty() {
            return Ok(());
        }

        let mut conn = self.get_connection().await?;
        let mut pipe = redis::pipe();
        pipe.atomic();

        for (key, fields) in operations {
            if !fields.is_empty() {
                let mut cmd = redis::cmd("HSET");
                cmd.arg(key.as_str());
                for (field, value) in fields {
                    cmd.arg(field.as_str()).arg(value.as_str());
                }
                pipe.add_command(cmd);
            }
        }

        pipe.query_async::<()>(&mut *conn)
            .await
            .with_context(|| "Failed to execute atomic pipeline HMSET")?;

        Ok(())
    }

    /// Get pool statistics
    pub fn pool_state(&self) -> bb8::State {
        self.pool.state()
    }

    /// Flush the current database (delete all keys)
    ///
    /// **WARNING**: This will delete ALL keys in the current database!
    /// Only use this for testing or when you're absolutely sure.
    pub async fn flushdb(&self) -> Result<()> {
        let mut conn = self.get_connection().await?;
        redis::cmd("FLUSHDB")
            .query_async(&mut *conn)
            .await
            .with_context(|| "Failed to FLUSHDB")
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore] // Requires Redis server
    async fn test_connection_pool() {
        let config = RedisPoolConfig {
            url: "redis://localhost:6379".to_string(),
            max_connections: 10,
            min_idle: Some(2),
            ..Default::default()
        };

        let client = RedisClient::with_config(config).await.unwrap();

        // Test basic operations
        client.set("test_key", "test_value").await.unwrap();
        let value: Option<String> = client.get("test_key").await.unwrap();
        assert_eq!(value, Some("test_value".to_string()));

        // Test pool state
        let state = client.pool_state();
        assert!(state.connections <= 10);
    }

    #[tokio::test]
    #[ignore] // Requires Redis server
    async fn test_concurrent_operations() {
        let client = RedisClient::new("redis://localhost:6379").await.unwrap();

        // Spawn multiple concurrent operations
        let mut handles = vec![];
        for i in 0..20 {
            let client_clone = client.clone();
            let handle = tokio::spawn(async move {
                let key = format!("concurrent_key_{}", i);
                client_clone.set(&key, i.to_string()).await.unwrap();
                let value: Option<String> = client_clone.get(&key).await.unwrap();
                assert_eq!(value, Some(i.to_string()));
            });
            handles.push(handle);
        }

        // Wait for all operations to complete
        for handle in handles {
            handle.await.unwrap();
        }

        // Check pool didn't exceed limits
        let state = client.pool_state();
        assert!(state.connections <= 20);
    }
}
