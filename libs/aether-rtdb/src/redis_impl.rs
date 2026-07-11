//! Redis implementation of RTDB traits

use crate::traits::*;
use aether_infra::redis::RedisClient;
use anyhow::{Context, Result};
use bytes::Bytes;
use std::collections::HashMap;
use std::sync::Arc;

/// Redis-backed RTDB implementation
///
/// This is a pure storage abstraction. For routing logic, use the
/// `aether-routing` library which handles M2C routing externally.
pub struct RedisRtdb {
    client: Arc<RedisClient>,
}

impl RedisRtdb {
    /// Create new Redis RTDB from URL
    pub async fn new(url: &str) -> Result<Self> {
        Ok(Self {
            client: Arc::new(RedisClient::new(url).await?),
        })
    }

    /// Create from existing RedisClient
    pub fn from_client(client: Arc<RedisClient>) -> Self {
        Self { client }
    }

    /// Get reference to underlying Redis client
    ///
    /// This is useful for calling Redis commands directly
    /// that are not part of the Rtdb trait.
    pub fn client(&self) -> &Arc<RedisClient> {
        &self.client
    }
}

impl Rtdb for RedisRtdb {
    async fn get<'a>(&'a self, key: &'a str) -> Result<Option<Bytes>> {
        let value: Option<String> = self.client.get(key).await.map_err(|e| anyhow::anyhow!(e))?;
        Ok(value.map(Bytes::from))
    }

    async fn set<'a>(&'a self, key: &'a str, value: Bytes) -> Result<()> {
        let s = std::str::from_utf8(value.as_ref()).with_context(|| {
            format!(
                "non-UTF-8 value rejected for SET {} (see Rtdb trait docs)",
                key
            )
        })?;
        self.client
            .set(key, s)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }

    async fn del<'a>(&'a self, key: &'a str) -> Result<bool> {
        let count = self
            .client
            .del(&[key])
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        Ok(count > 0)
    }

    async fn exists<'a>(&'a self, key: &'a str) -> Result<bool> {
        self.client
            .exists(key)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }

    async fn incrbyfloat<'a>(&'a self, key: &'a str, increment: f64) -> Result<f64> {
        self.client
            .incrbyfloat(key, increment)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }

    async fn hash_set<'a>(&'a self, key: &'a str, field: &'a str, value: Bytes) -> Result<()> {
        let s = std::str::from_utf8(value.as_ref())
            .with_context(|| {
                format!(
                    "non-UTF-8 value rejected for HSET {}:{} (see Rtdb trait docs)",
                    key, field
                )
            })?
            .to_owned();
        self.client
            .hset(key, field, s)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }

    async fn hash_setnx<'a>(&'a self, key: &'a str, field: &'a str, value: Bytes) -> Result<bool> {
        let s = std::str::from_utf8(value.as_ref())
            .with_context(|| {
                format!(
                    "non-UTF-8 value rejected for HSETNX {}:{} (see Rtdb trait docs)",
                    key, field
                )
            })?
            .to_owned();
        self.client
            .hsetnx(key, field, s)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }

    async fn hash_get<'a>(&'a self, key: &'a str, field: &'a str) -> Result<Option<Bytes>> {
        let value: Option<String> = self
            .client
            .hget(key, field)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        Ok(value.map(Bytes::from))
    }

    async fn hash_mget<'a>(
        &'a self,
        key: &'a str,
        fields: &'a [&'a str],
    ) -> Result<Vec<Option<Bytes>>> {
        let values: Vec<Option<String>> = self
            .client
            .hmget(key, fields)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        Ok(values.into_iter().map(|v| v.map(Bytes::from)).collect())
    }

    async fn hash_mset<'a>(&'a self, key: &'a str, fields: Vec<(String, Bytes)>) -> Result<()> {
        let string_fields: Result<Vec<(String, String)>> = fields
            .into_iter()
            .map(|(k, v)| {
                let s = std::str::from_utf8(v.as_ref())
                    .with_context(|| {
                        format!(
                            "non-UTF-8 value rejected for HMSET {}:{} (see Rtdb trait docs)",
                            key, k
                        )
                    })?
                    .to_owned();
                Ok((k, s))
            })
            .collect();
        self.client
            .hmset(key, &string_fields?)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }

    async fn hash_get_all<'a>(&'a self, key: &'a str) -> Result<HashMap<String, Bytes>> {
        let data: HashMap<String, String> = self
            .client
            .hgetall(key)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        Ok(data.into_iter().map(|(k, v)| (k, Bytes::from(v))).collect())
    }

    async fn hash_del<'a>(&'a self, key: &'a str, field: &'a str) -> Result<bool> {
        self.client
            .hdel(key, field)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }

    async fn hash_del_many<'a>(&'a self, key: &'a str, fields: &'a [String]) -> Result<usize> {
        self.client
            .hdel_many(key, fields)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }

    async fn list_lpush<'a>(&'a self, key: &'a str, value: Bytes) -> Result<()> {
        let s = std::str::from_utf8(value.as_ref()).context("UTF-8 conversion failed")?;
        self.client
            .lpush(key, s)
            .await
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!(e))
    }

    async fn list_rpush<'a>(&'a self, key: &'a str, value: Bytes) -> Result<()> {
        let s = std::str::from_utf8(value.as_ref()).context("UTF-8 conversion failed")?;
        self.client
            .rpush(key, s)
            .await
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!(e))
    }

    async fn list_lpop<'a>(&'a self, key: &'a str) -> Result<Option<Bytes>> {
        let value: Option<String> = self
            .client
            .lpop(key)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        Ok(value.map(Bytes::from))
    }

    async fn list_rpop<'a>(&'a self, key: &'a str) -> Result<Option<Bytes>> {
        let value: Option<String> = self
            .client
            .rpop(key)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        Ok(value.map(Bytes::from))
    }

    async fn list_blpop<'a>(
        &'a self,
        keys: &'a [&'a str],
        timeout_seconds: u64,
    ) -> Result<Option<(String, Bytes)>> {
        let result: Option<(String, String)> = self
            .client
            .blpop(keys, timeout_seconds as usize)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        Ok(result.map(|(k, v)| (k, Bytes::from(v))))
    }

    async fn list_range<'a>(
        &'a self,
        key: &'a str,
        start: isize,
        stop: isize,
    ) -> Result<Vec<Bytes>> {
        let values: Vec<String> = self
            .client
            .lrange(key, start, stop)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        Ok(values.into_iter().map(Bytes::from).collect())
    }

    async fn list_trim<'a>(&'a self, key: &'a str, start: isize, stop: isize) -> Result<()> {
        self.client
            .ltrim(key, start, stop)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }

    async fn expire<'a>(&'a self, key: &'a str, seconds: i64) -> Result<bool> {
        self.client
            .expire(key, seconds)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }

    async fn scan_match<'a>(&'a self, pattern: &'a str) -> Result<Vec<String>> {
        self.client
            .scan_match(pattern)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }

    async fn sadd<'a>(&'a self, key: &'a str, member: &'a str) -> Result<bool> {
        self.client
            .sadd(key, member)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }

    async fn srem<'a>(&'a self, key: &'a str, member: &'a str) -> Result<bool> {
        self.client
            .srem(key, member)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }

    async fn smembers<'a>(&'a self, key: &'a str) -> Result<Vec<String>> {
        self.client
            .smembers(key)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }

    async fn hincrby<'a>(&'a self, key: &'a str, field: &'a str, increment: i64) -> Result<i64> {
        self.client
            .hincrby(key, field, increment)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }

    async fn pipeline_hash_mset(&self, operations: crate::traits::HashMsetOps) -> Result<()> {
        let string_operations = convert_pipeline_ops(operations)?;
        if string_operations.is_empty() {
            return Ok(());
        }
        self.client
            .pipeline_hmset(&string_operations)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }

    async fn pipeline_hash_mset_atomic(
        &self,
        operations: crate::traits::HashMsetOps,
    ) -> Result<()> {
        let string_operations = convert_pipeline_ops(operations)?;
        if string_operations.is_empty() {
            return Ok(());
        }
        self.client
            .pipeline_hmset_atomic(&string_operations)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }
}

type StringHmsetOps = Vec<(String, Vec<(String, String)>)>;

/// Convert Arc<str> + Bytes to (String, String) for the Redis client.
/// Extracted so both atomic and non-atomic pipeline paths share the
/// UTF-8 validation logic without code duplication.
fn convert_pipeline_ops(operations: crate::traits::HashMsetOps) -> Result<StringHmsetOps> {
    operations
        .into_iter()
        .map(|(key, fields)| {
            let string_fields: Result<Vec<_>> = fields
                .into_iter()
                .map(|(f, v)| {
                    let s = std::str::from_utf8(v.as_ref())
                        .with_context(|| {
                            format!(
                                "non-UTF-8 value rejected for pipeline HSET {}:{} (see Rtdb trait docs)",
                                key, f
                            )
                        })?
                        .to_owned();
                    Ok((f.to_string(), s))
                })
                .collect();
            Ok((key, string_fields?))
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "Requires Redis connection"]
    async fn test_redis_rtdb_basic_operations() {
        let rtdb = RedisRtdb::new("redis://localhost:6379")
            .await
            .expect("Failed to connect to Redis");

        // Test KV operations
        rtdb.set("test:key", Bytes::from("value"))
            .await
            .expect("Failed to set");
        let value = rtdb.get("test:key").await.expect("Failed to get");
        assert_eq!(value, Some(Bytes::from("value")));

        // Test exists
        assert!(rtdb.exists("test:key").await.unwrap());

        // Test delete
        assert!(rtdb.del("test:key").await.unwrap());
        assert!(!rtdb.exists("test:key").await.unwrap());
    }

    #[tokio::test]
    #[ignore = "Requires Redis connection"]
    async fn test_redis_rtdb_hash_operations() {
        let rtdb = RedisRtdb::new("redis://localhost:6379")
            .await
            .expect("Failed to connect to Redis");

        // Test hash operations
        rtdb.hash_set("test:hash", "field1", Bytes::from("value1"))
            .await
            .unwrap();
        rtdb.hash_set("test:hash", "field2", Bytes::from("value2"))
            .await
            .unwrap();

        let value = rtdb.hash_get("test:hash", "field1").await.unwrap();
        assert_eq!(value, Some(Bytes::from("value1")));

        let all = rtdb.hash_get_all("test:hash").await.unwrap();
        assert_eq!(all.len(), 2);

        // Cleanup
        rtdb.del("test:hash").await.unwrap();
    }

    #[tokio::test]
    #[ignore = "Requires Redis connection"]
    async fn test_redis_rtdb_list_operations() {
        let rtdb = RedisRtdb::new("redis://localhost:6379")
            .await
            .expect("Failed to connect to Redis");

        // Test list operations
        rtdb.list_lpush("test:list", Bytes::from("value1"))
            .await
            .unwrap();
        rtdb.list_rpush("test:list", Bytes::from("value2"))
            .await
            .unwrap();

        let range = rtdb.list_range("test:list", 0, -1).await.unwrap();
        assert_eq!(range.len(), 2);

        let value = rtdb.list_lpop("test:list").await.unwrap();
        assert_eq!(value, Some(Bytes::from("value1")));

        // Cleanup
        rtdb.del("test:list").await.unwrap();
    }

    #[tokio::test]
    #[ignore = "Requires Redis connection"]
    async fn test_redis_rtdb_hash_mset() {
        let rtdb = RedisRtdb::new("redis://localhost:6379")
            .await
            .expect("Failed to connect to Redis");

        // Test hash_mset
        rtdb.hash_mset(
            "test:hash_batch",
            vec![
                ("f1".to_string(), Bytes::from("v1")),
                ("f2".to_string(), Bytes::from("v2")),
                ("f3".to_string(), Bytes::from("v3")),
            ],
        )
        .await
        .unwrap();

        let all = rtdb.hash_get_all("test:hash_batch").await.unwrap();
        assert_eq!(all.len(), 3);

        // Cleanup
        rtdb.del("test:hash_batch").await.unwrap();
    }

    #[tokio::test]
    #[ignore = "Requires Redis connection"]
    async fn test_redis_rtdb_list_trim() {
        let rtdb = RedisRtdb::new("redis://localhost:6379")
            .await
            .expect("Failed to connect to Redis");

        // Populate list
        for i in 0..10 {
            rtdb.list_rpush("test:trim_list", Bytes::from(format!("value{}", i)))
                .await
                .unwrap();
        }

        // Trim to keep only first 5
        rtdb.list_trim("test:trim_list", 0, 4).await.unwrap();

        let range = rtdb.list_range("test:trim_list", 0, -1).await.unwrap();
        assert_eq!(range.len(), 5);

        // Cleanup
        rtdb.del("test:trim_list").await.unwrap();
    }
}
