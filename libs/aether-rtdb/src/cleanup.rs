//! Redis Data Cleanup Trait and Implementation
//!
//! Provides a generic framework for cleaning up invalid Redis keys based on
//! database configuration. Services implement the `CleanupProvider` trait
//! to define their specific cleanup logic.

use std::collections::HashSet;
use std::future::Future;

use anyhow::Result;
use tracing::{debug, info, warn};

use crate::traits::Rtdb;

/// Provider trait for Redis cleanup operations
///
/// Services implement this trait to define how to:
/// - Get valid entity IDs from their database
/// - Extract entity IDs from Redis keys
/// - Identify system keys that should be preserved
pub trait CleanupProvider: Send + Sync {
    /// Get the set of valid entity IDs from the database
    ///
    /// # Returns
    /// A HashSet of valid ID strings (e.g., channel IDs, instance names)
    ///
    /// # Example
    /// ```ignore
    /// fn get_valid_ids(&self) -> impl Future<Output = Result<HashSet<String>>> + Send {
    ///     async move {
    ///         let ids = sqlx::query_as::<_, (u16,)>("SELECT id FROM channels")
    ///             .fetch_all(&self.db)
    ///             .await?
    ///             .into_iter()
    ///             .map(|(id,)| id.to_string())
    ///             .collect();
    ///         Ok(ids)
    ///     }
    /// }
    /// ```
    fn get_valid_ids(&self) -> impl Future<Output = Result<HashSet<String>>> + Send;

    /// Get the Redis key pattern to scan
    ///
    /// # Returns
    /// A pattern string for Redis KEYS command (e.g., "io:*", "automation:*")
    fn key_pattern(&self) -> &str;

    /// Extract entity ID from a Redis key
    ///
    /// # Arguments
    /// * `key` - Redis key string
    ///
    /// # Returns
    /// Some(id) if the key represents an entity, None for system keys
    ///
    /// # Example
    /// ```ignore
    /// fn extract_id(&self, key: &str) -> Option<String> {
    ///     // io:1:T -> Some("1")
    ///     // io:stats:* -> None
    ///     let parts: Vec<&str> = key.split(':').collect();
    ///     if parts.len() >= 2 && parts[0] == "io" {
    ///         parts[1].parse::<u16>().ok().map(|id| id.to_string())
    ///     } else {
    ///         None
    ///     }
    /// }
    /// ```
    fn extract_id(&self, key: &str) -> Option<String>;

    /// Check if a key is a system key that should be preserved
    ///
    /// # Arguments
    /// * `key` - Redis key string
    ///
    /// # Returns
    /// true if the key should be preserved, false otherwise
    ///
    /// # Default Implementation
    /// Returns false (no system keys). Override this method if your
    /// service has system-level keys that should not be deleted.
    fn is_system_key(&self, _key: &str) -> bool {
        false
    }

    /// Get the service name for logging
    fn service_name(&self) -> &str;

    /// Get valid point IDs for a specific entity (optional, for point-level cleanup)
    ///
    /// # Arguments
    /// * `entity_id` - Entity ID (channel_id or instance_id)
    /// * `key` - Full Redis key to determine point type (e.g., "io:1:T", "inst:1:M")
    ///
    /// # Returns
    /// * `None` - Point-level cleanup not supported (default)
    /// * `Some(set)` - Set of valid point IDs for this entity and key
    ///
    /// # Example
    /// ```ignore
    /// fn get_valid_point_ids_for_entity(
    ///     &self,
    ///     entity_id: &str,
    ///     key: &str,
    /// ) -> impl Future<Output = Result<Option<HashSet<String>>>> + Send {
    ///     let entity_id = entity_id.to_string();
    ///     let key = key.to_string();
    ///     async move {
    ///         // For io:1:T, query telemetry_points WHERE channel_id = 1
    ///         // Return Some(set) with valid point_ids
    ///         Ok(None)
    ///     }
    /// }
    /// ```
    fn get_valid_point_ids_for_entity(
        &self,
        _entity_id: &str,
        _key: &str,
    ) -> impl Future<Output = Result<Option<HashSet<String>>>> + Send {
        async { Ok(None) } // Default: no point-level cleanup
    }
}

/// Clean up invalid Redis keys based on database configuration
///
/// This function performs the following steps:
/// 1. Get valid entity IDs from database via provider
/// 2. Scan Redis keys matching the pattern
/// 3. Delete keys for entities not in the valid set
/// 4. Preserve system keys
///
/// # Arguments
/// * `provider` - Cleanup provider implementation
/// * `redis` - Redis client with KeyValueStore trait
///
/// # Returns
/// Number of keys deleted, or error if cleanup fails
///
/// # Example
/// ```ignore
/// let provider = IoCleanupProvider { db: pool };
/// let deleted = cleanup_invalid_keys(&provider, &redis).await?;
/// info!("Cleaned up {} invalid keys", deleted);
/// ```
pub async fn cleanup_invalid_keys<P, R>(provider: &P, redis: &R) -> Result<usize>
where
    P: CleanupProvider,
    R: Rtdb,
{
    // Check if cleanup is disabled via environment variable
    if std::env::var("SKIP_REDIS_CLEANUP")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false)
    {
        info!("{}: Cleanup skip (env)", provider.service_name());
        return Ok(0);
    }

    info!("{}: Cleanup starting", provider.service_name());

    // 1. Get valid entity IDs from database
    let valid_ids = provider.get_valid_ids().await?;
    info!("{}: {} valid IDs", provider.service_name(), valid_ids.len());
    debug!("{}: Valid IDs: {:?}", provider.service_name(), valid_ids);

    // 2. Scan Redis keys matching pattern
    let pattern = provider.key_pattern();
    let keys: Vec<String> = redis.scan_match(pattern).await?;
    info!(
        "{}: {} keys ({})",
        provider.service_name(),
        keys.len(),
        pattern
    );

    // 3. Analyze and delete invalid keys
    let mut deleted_count = 0;
    let mut preserved_system_keys = 0;
    let mut preserved_valid_keys = 0;
    let mut deleted_point_count = 0;

    for key in keys {
        // Check if it's a system key
        if provider.is_system_key(&key) {
            preserved_system_keys += 1;
            debug!("{}: Preserved system key: {}", provider.service_name(), key);
            continue;
        }

        // Extract entity ID and check validity
        match provider.extract_id(&key) {
            Some(id) => {
                if valid_ids.contains(&id) {
                    // Valid entity, keep the key
                    preserved_valid_keys += 1;

                    // Check for point-level cleanup
                    if let Some(valid_point_ids) =
                        provider.get_valid_point_ids_for_entity(&id, &key).await?
                    {
                        // Get all fields in the hash
                        let all_fields = redis.hash_get_all(&key).await?;

                        // Delete invalid point IDs
                        for field in all_fields.keys() {
                            if !valid_point_ids.contains(field) {
                                redis.hash_del(&key, field).await?;
                                deleted_point_count += 1;
                                debug!(
                                    "{}: Deleted invalid point field: {} in key {} (point '{}' not in config)",
                                    provider.service_name(),
                                    field,
                                    key,
                                    field
                                );
                            }
                        }
                    }
                } else {
                    // Invalid entity, delete the key
                    redis.del(&key).await?;
                    deleted_count += 1;
                    debug!(
                        "{}: Deleted invalid key: {} (entity '{}' not in config)",
                        provider.service_name(),
                        key,
                        id
                    );
                }
            },
            None => {
                // Could not extract ID, treat as system key
                preserved_system_keys += 1;
                debug!(
                    "{}: Preserved non-entity key: {}",
                    provider.service_name(),
                    key
                );
            },
        }
    }

    info!(
        "{}: Cleanup done (del:{}/{} keep:{}/{})",
        provider.service_name(),
        deleted_count,
        deleted_point_count,
        preserved_valid_keys,
        preserved_system_keys
    );

    if deleted_count > 0 || deleted_point_count > 0 {
        let mut messages = Vec::new();
        if deleted_count > 0 {
            messages.push(format!("{} keys", deleted_count));
        }
        if deleted_point_count > 0 {
            messages.push(format!("{} point fields", deleted_point_count));
        }
        warn!(
            "{}: Removed {}",
            provider.service_name(),
            messages.join(", ")
        );
    }

    Ok(deleted_count)
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;
    use crate::memory_impl::MemoryRtdb;
    use bytes::Bytes;
    use serial_test::serial;

    // Mock provider for testing
    struct MockProvider {
        valid_ids: HashSet<String>,
        system_keys: Vec<String>,
    }

    impl CleanupProvider for MockProvider {
        fn get_valid_ids(&self) -> impl Future<Output = Result<HashSet<String>>> + Send {
            let valid_ids = self.valid_ids.clone();
            async move { Ok(valid_ids) }
        }

        fn key_pattern(&self) -> &str {
            "test:*"
        }

        fn extract_id(&self, key: &str) -> Option<String> {
            let parts: Vec<&str> = key.split(':').collect();
            if parts.len() >= 2 && parts[0] == "test" {
                Some(parts[1].to_string())
            } else {
                None
            }
        }

        fn is_system_key(&self, key: &str) -> bool {
            self.system_keys.contains(&key.to_string())
        }

        fn service_name(&self) -> &str {
            "test"
        }
    }

    /// Helper to create MemoryRtdb with initial keys populated
    async fn create_rtdb_with_keys(keys: &[&str]) -> MemoryRtdb {
        let rtdb = MemoryRtdb::new();
        for key in keys {
            // Set a dummy value to mark the key as existing
            rtdb.set(key, Bytes::from("test_value")).await.unwrap();
        }
        rtdb
    }

    #[tokio::test]
    #[serial]
    async fn test_cleanup_removes_invalid_keys() {
        // Ensure clean environment (prevent pollution from parallel tests)
        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::remove_var("SKIP_REDIS_CLEANUP") };

        let mut valid_ids = HashSet::new();
        valid_ids.insert("1".to_string());
        valid_ids.insert("2".to_string());

        let provider = MockProvider {
            valid_ids,
            system_keys: vec!["test:stats:count".to_string()],
        };

        let rtdb = create_rtdb_with_keys(&[
            "test:1:data",
            "test:2:data",
            "test:999:data",    // Invalid - should be deleted
            "test:stats:count", // System key - should be preserved
        ])
        .await;

        let deleted = cleanup_invalid_keys(&provider, &rtdb).await.unwrap();

        assert_eq!(deleted, 1); // Only test:999:data should be deleted
        assert!(rtdb.exists("test:1:data").await.unwrap());
        assert!(rtdb.exists("test:2:data").await.unwrap());
        assert!(!rtdb.exists("test:999:data").await.unwrap());
        assert!(rtdb.exists("test:stats:count").await.unwrap());
    }

    #[tokio::test]
    #[serial]
    async fn test_cleanup_with_environment_variable() {
        // Ensure clean environment before setting test variable
        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::remove_var("SKIP_REDIS_CLEANUP") };
        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::set_var("SKIP_REDIS_CLEANUP", "true") };

        let provider = MockProvider {
            valid_ids: HashSet::new(),
            system_keys: vec![],
        };

        let rtdb = MemoryRtdb::new();

        let deleted = cleanup_invalid_keys(&provider, &rtdb).await.unwrap();
        assert_eq!(deleted, 0);

        // Clean up after test
        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::remove_var("SKIP_REDIS_CLEANUP") };
    }
}
