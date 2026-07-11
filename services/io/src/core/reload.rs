//! Hot reload implementation for io
//!
//! This module implements the unified `ReloadableService` trait for io,
//! enabling zero-downtime configuration updates from SQLite database.

use common::{ChannelReloadResult, ReloadableService};
use std::sync::Arc;
use tracing::{error, info, warn};

use crate::core::channels::channel_manager::ChannelManager;
use crate::core::config::ChannelConfig;

/// Channel change severity classification
///
/// Determines how configuration changes should be handled:
/// - `MetadataOnly`: Safe to update without restart (name, description)
/// - `NonCritical`: May require reconnect (timeout, retry parameters)
/// - `Critical`: Requires full channel recreation (host, port, protocol)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChannelChangeType {
    /// Metadata changes only (name, description) - no restart needed
    MetadataOnly = 0,
    /// Non-critical changes (timeout, retry) - may require reconnect
    NonCritical = 1,
    /// Critical changes (host, port, protocol) - requires full restart
    Critical = 2,
}

impl ChannelChangeType {
    /// Analyze changes between old and new configuration
    pub fn analyze(old: &ChannelConfig, new: &ChannelConfig) -> Self {
        // Protocol change is always critical
        if old.protocol() != new.protocol() {
            return ChannelChangeType::Critical;
        }

        // Check for critical parameter changes (host, port)
        if let (Some(old_host), Some(new_host)) =
            (old.parameters.get("host"), new.parameters.get("host"))
            && old_host != new_host
        {
            return ChannelChangeType::Critical;
        }

        if let (Some(old_port), Some(new_port)) =
            (old.parameters.get("port"), new.parameters.get("port"))
            && old_port != new_port
        {
            return ChannelChangeType::Critical;
        }

        // Check for non-critical parameter changes (timeout, retry)
        let non_critical_params = ["timeout_ms", "retry_count", "retry_delay_ms"];
        for param in &non_critical_params {
            if let (Some(old_val), Some(new_val)) =
                (old.parameters.get(*param), new.parameters.get(*param))
                && old_val != new_val
            {
                return ChannelChangeType::NonCritical;
            }
        }

        // Check metadata changes (name, description)
        if old.name() != new.name() || old.core.description != new.core.description {
            return ChannelChangeType::MetadataOnly;
        }

        // No changes detected
        ChannelChangeType::MetadataOnly
    }
}

impl ReloadableService for ChannelManager {
    type ChangeType = ChannelChangeType;
    type Config = ChannelConfig;
    type ReloadResult = ChannelReloadResult;

    /// Reload configuration from SQLite database
    async fn reload_from_database(
        &self,
        pool: &sqlx::SqlitePool,
    ) -> anyhow::Result<Self::ReloadResult> {
        let start_time = std::time::Instant::now();
        info!("Starting hot reload from SQLite database");

        // 1. Load all channels from SQLite
        let db_channels: Vec<(i64, String, String, bool)> =
            sqlx::query_as("SELECT channel_id, name, protocol, enabled FROM channels")
                .fetch_all(pool)
                .await?;

        // 2. Get runtime channel IDs
        let runtime_ids: std::collections::HashSet<u32> =
            self.get_channel_ids().into_iter().collect();

        let db_ids: std::collections::HashSet<u32> =
            db_channels.iter().map(|(id, _, _, _)| *id as u32).collect();

        // 3. Determine changes
        let to_add: Vec<u32> = db_ids.difference(&runtime_ids).copied().collect();
        let to_remove: Vec<u32> = runtime_ids.difference(&db_ids).copied().collect();
        let to_update: Vec<u32> = db_ids.intersection(&runtime_ids).copied().collect();

        let mut added = Vec::new();
        let mut updated = Vec::new();
        let mut removed = Vec::new();
        let mut errors = Vec::new();

        // 4. Remove channels that are no longer in SQLite
        for id in &to_remove {
            match self.remove_channel(*id).await {
                Ok(_) => {
                    removed.push(*id);
                    info!("Removed channel {} (not in SQLite)", id);
                },
                Err(e) => {
                    errors.push(format!("Failed to remove channel {}: {}", id, e));
                    error!("Failed to remove channel {}: {}", id, e);
                },
            }
        }

        // 5. Add new channels from SQLite
        for id in &to_add {
            match Self::load_channel_from_db(pool, *id).await {
                Ok(channel_config) => {
                    // Only create and connect if enabled
                    if channel_config.is_enabled() {
                        match self.create_channel(Arc::new(channel_config)).await {
                            Ok(entry) => {
                                // Try to connect using ChannelEntry's direct method
                                if let Err(e) = entry.connect().await {
                                    warn!("Channel {} added but failed to connect: {}", id, e);
                                    errors.push(format!("Channel {} connection failed: {}", id, e));
                                } else {
                                    added.push(*id);
                                    info!("Added and started channel {} from SQLite", id);
                                }
                            },
                            Err(e) => {
                                errors.push(format!("Failed to add channel {}: {}", id, e));
                                error!("Failed to add channel {}: {}", id, e);
                            },
                        }
                    } else {
                        // Channel is disabled, don't create runtime instance
                        added.push(*id);
                        info!("Added channel {} from SQLite (disabled, not started)", id);
                    }
                },
                Err(e) => {
                    errors.push(format!("Failed to load channel {} from DB: {}", id, e));
                    error!("Failed to load channel {} from DB: {}", id, e);
                },
            }
        }

        // 6. Update existing channels
        for id in &to_update {
            match Self::load_channel_from_db(pool, *id).await {
                Ok(new_config) => {
                    // Remove old channel if exists (may be disabled)
                    if let Err(e) = self.remove_channel(*id).await {
                        warn!(
                            "Channel {} not in runtime during update (may be disabled): {}",
                            id, e
                        );
                    }

                    // Only create and connect if enabled
                    if new_config.is_enabled() {
                        match self.create_channel(Arc::new(new_config)).await {
                            Ok(entry) => {
                                if let Err(e) = entry.connect().await {
                                    warn!("Channel {} updated but failed to connect: {}", id, e);
                                    errors.push(format!("Channel {} connection failed: {}", id, e));
                                } else {
                                    updated.push(*id);
                                    info!("Updated and started channel {} from SQLite", id);
                                }
                            },
                            Err(e) => {
                                errors.push(format!("Failed to update channel {}: {}", id, e));
                                error!("Failed to update channel {}: {}", id, e);
                            },
                        }
                    } else {
                        // Channel is disabled, don't create runtime instance
                        updated.push(*id);
                        info!("Updated channel {} from SQLite (disabled, not started)", id);
                    }
                },
                Err(e) => {
                    errors.push(format!("Failed to load channel {} for update: {}", id, e));
                    error!("Failed to load channel {} for update: {}", id, e);
                },
            }
        }

        // 7. Refresh routing cache from SQLite
        match Self::reload_routing_cache(pool, &self.routing_cache).await {
            Ok((c2m_count, m2c_count, c2c_count)) => {
                info!(
                    "Routing cache refreshed: {} C2M, {} M2C, {} C2C mappings",
                    c2m_count, m2c_count, c2c_count
                );
            },
            Err(e) => {
                // Routing cache refresh failure should not block channel reload
                warn!("Failed to refresh routing cache (continuing anyway): {}", e);
                errors.push(format!("Routing cache refresh failed: {}", e));
            },
        }

        let duration_ms = start_time.elapsed().as_millis() as u64;
        let total_count = db_channels.len();

        info!(
            "Hot reload completed: {} added, {} updated, {} removed, {} errors ({}ms)",
            added.len(),
            updated.len(),
            removed.len(),
            errors.len(),
            duration_ms
        );

        Ok(ChannelReloadResult {
            total_count,
            added,
            updated,
            removed,
            errors,
            duration_ms,
        })
    }

    /// Analyze changes between old and new configuration
    fn analyze_changes(
        &self,
        old_config: &Self::Config,
        new_config: &Self::Config,
    ) -> Self::ChangeType {
        ChannelChangeType::analyze(old_config, new_config)
    }

    /// Perform hot reload of a channel
    async fn perform_hot_reload(&self, config: Self::Config) -> anyhow::Result<String> {
        let channel_id = config.id();
        info!("Performing hot reload for channel {}", channel_id);

        // Remove old channel
        self.remove_channel(channel_id).await?;

        // Create new channel with updated config
        let entry = self.create_channel(Arc::new(config)).await?;

        // Connect using ChannelEntry's direct method
        entry.connect().await?;

        Ok("running".to_string())
    }

    /// Rollback to previous configuration
    async fn rollback(&self, previous_config: Self::Config) -> anyhow::Result<String> {
        let channel_id = previous_config.id();
        warn!(
            "Rolling back channel {} to previous configuration",
            channel_id
        );

        // Remove current channel
        let _ = self.remove_channel(channel_id).await;

        // Restore previous configuration
        let entry = self.create_channel(Arc::new(previous_config)).await?;

        // Connect using ChannelEntry's direct method
        entry.connect().await?;

        Ok("restored".to_string())
    }
}

impl ChannelManager {
    /// Load channel configuration from SQLite database
    pub async fn load_channel_from_db(
        pool: &sqlx::SqlitePool,
        channel_id: u32,
    ) -> anyhow::Result<ChannelConfig> {
        // Load basic channel info
        let (name, protocol, enabled): (String, String, bool) =
            sqlx::query_as("SELECT name, protocol, enabled FROM channels WHERE channel_id = ?")
                .bind(channel_id as i64)
                .fetch_one(pool)
                .await?;

        // Load description, parameters, and logging from config JSON
        let (description, parameters, logging): (
            Option<String>,
            std::collections::HashMap<String, serde_json::Value>,
            crate::core::config::ChannelLoggingConfig,
        ) = {
            let config_str: Option<String> =
                sqlx::query_scalar("SELECT config FROM channels WHERE channel_id = ?")
                    .bind(channel_id as i64)
                    .fetch_optional(pool)
                    .await?;

            config_str
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .map(|mut config_value| {
                    // Extract description (still need to clone string, but that's minimal)
                    let desc = config_value
                        .get("description")
                        .and_then(|d| d.as_str())
                        .map(|s| s.to_string());

                    // Extract from "parameters" field - use remove to move ownership (avoid clone)
                    let params = match config_value.get_mut("parameters").map(std::mem::take) {
                        Some(serde_json::Value::Object(map)) => map.into_iter().collect(),
                        _ => std::collections::HashMap::new(),
                    };

                    // Extract logging config from JSON
                    let log_cfg = config_value
                        .get("logging")
                        .and_then(|v| {
                            serde_json::from_value::<crate::core::config::ChannelLoggingConfig>(
                                v.clone(),
                            )
                            .ok()
                        })
                        .unwrap_or_default();

                    (desc, params, log_cfg)
                })
                .unwrap_or_else(|| {
                    (
                        None,
                        std::collections::HashMap::new(),
                        crate::core::config::ChannelLoggingConfig::default(),
                    )
                })
        };

        Ok(ChannelConfig {
            core: crate::core::config::ChannelCore {
                id: channel_id,
                name,
                description,
                protocol,
                enabled,
            },
            parameters,
            logging,
        })
    }

    /// Reload routing cache from SQLite database
    ///
    /// Performs atomic update of the in-memory routing cache with fresh data from SQLite.
    /// Returns (c2m_count, m2c_count, c2c_count) on success.
    ///
    /// This is a public method that can be called from API handlers to refresh routing cache
    /// independently of channel reload operations.
    pub async fn reload_routing_cache(
        sqlite_pool: &sqlx::SqlitePool,
        routing_cache: &Arc<aether_routing::RoutingCache>,
    ) -> anyhow::Result<(usize, usize, usize)> {
        use tracing::debug;

        // 1. Load routing data from SQLite via shared library
        let maps = aether_routing::load_routing_maps(sqlite_pool).await?;

        let c2m_count = maps.c2m.len();
        let m2c_count = maps.m2c.len();
        let c2c_count = maps.c2c.len();

        debug!(
            "Loaded routing maps: {} C2M, {} M2C, {} C2C",
            c2m_count, m2c_count, c2c_count
        );

        // 2. Atomic update of cache (thread-safe, lock-free swap)
        routing_cache.update(maps.c2m, maps.m2c, maps.c2c);

        debug!("Routing cache updated successfully");

        Ok((c2m_count, m2c_count, c2c_count))
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;

    #[test]
    fn test_change_analysis_metadata_only() {
        let old_config = create_test_config(1001, "Old Name", "modbus_tcp", "192.168.1.100", 502);
        let new_config = create_test_config(1001, "New Name", "modbus_tcp", "192.168.1.100", 502);

        let change_type = ChannelChangeType::analyze(&old_config, &new_config);
        assert_eq!(change_type, ChannelChangeType::MetadataOnly);
    }

    #[test]
    fn test_change_analysis_critical_host() {
        let old_config = create_test_config(1001, "Channel 1", "modbus_tcp", "192.168.1.100", 502);
        let new_config = create_test_config(1001, "Channel 1", "modbus_tcp", "192.168.1.101", 502);

        let change_type = ChannelChangeType::analyze(&old_config, &new_config);
        assert_eq!(change_type, ChannelChangeType::Critical);
    }

    #[test]
    fn test_change_analysis_critical_port() {
        let old_config = create_test_config(1001, "Channel 1", "modbus_tcp", "192.168.1.100", 502);
        let new_config = create_test_config(1001, "Channel 1", "modbus_tcp", "192.168.1.100", 503);

        let change_type = ChannelChangeType::analyze(&old_config, &new_config);
        assert_eq!(change_type, ChannelChangeType::Critical);
    }

    #[test]
    fn test_change_analysis_critical_protocol() {
        let old_config = create_test_config(1001, "Channel 1", "modbus_tcp", "192.168.1.100", 502);
        let new_config = create_test_config(1001, "Channel 1", "modbus_rtu", "192.168.1.100", 502);

        let change_type = ChannelChangeType::analyze(&old_config, &new_config);
        assert_eq!(change_type, ChannelChangeType::Critical);
    }

    fn create_test_config(
        id: u32,
        name: &str,
        protocol: &str,
        host: &str,
        port: u16,
    ) -> ChannelConfig {
        let mut parameters = std::collections::HashMap::new();
        parameters.insert("host".to_string(), serde_json::json!(host));
        parameters.insert("port".to_string(), serde_json::json!(port));

        ChannelConfig {
            core: crate::core::config::ChannelCore {
                id,
                name: name.to_string(),
                description: None,
                protocol: protocol.to_string(),
                enabled: true,
            },
            parameters,
            logging: crate::core::config::ChannelLoggingConfig::default(),
        }
    }
}
