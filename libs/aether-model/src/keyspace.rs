//! Redis KeySpace Configuration
//!
//! This module provides the `KeySpaceConfig` struct for generating Redis keys
//! in a consistent and type-safe manner across all AetherEMS services.

use crate::PointType;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// Cached production configuration (singleton, zero-allocation after first call)
static PRODUCTION_CONFIG: OnceLock<KeySpaceConfig> = OnceLock::new();

/// Keyspace configuration for Redis operations
///
/// **Design Principles:**
/// - Configuration as Data
/// - Test isolation (dedicated keyspace)
/// - Multi-environment support (dev/test/prod)
/// - Single Source of Truth for key naming
///
/// **Usage Example:**
/// ```
/// use aether_model::{KeySpaceConfig, PointType};
///
/// // Production environment
/// let prod_config = KeySpaceConfig::production();
///
/// // Test environment (fully isolated keyspace)
/// let test_config = KeySpaceConfig::test();
///
/// // M2C routing configuration
/// let m2c_config = prod_config.for_m2c();
///
/// // Key generation (type-safe)
/// let key = prod_config.channel_key(1001, PointType::Telemetry);
/// // => "io:1001:T"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeySpaceConfig {
    /// Data storage key prefix (e.g., "io" or "test:io")
    pub data_prefix: String,

    /// Instance key prefix (e.g., "inst" or "test:inst")
    pub inst_prefix: String,

    /// Routing table key (e.g., "route:c2m" or "test:route:c2m")
    pub routing_table: String,

    /// Target key prefix (M2C only, e.g., "io")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_prefix: Option<String>,

    /// Instance name lookup pattern (M2C only, e.g., "inst:*:name")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inst_name_pattern: Option<String>,
}

impl Default for KeySpaceConfig {
    fn default() -> Self {
        Self::production()
    }
}

impl KeySpaceConfig {
    /// Production environment configuration
    ///
    /// Uses standard keyspace naming:
    /// - data_prefix: "io"
    /// - inst_prefix: "inst"
    /// - routing_table: "route:c2m"
    pub fn production() -> Self {
        Self {
            data_prefix: "io".to_string(),
            inst_prefix: "inst".to_string(),
            routing_table: "route:c2m".to_string(),
            target_prefix: None,
            inst_name_pattern: None,
        }
    }

    /// Get cached production configuration (zero-allocation after first call)
    ///
    /// Use this method in hot paths to avoid repeated String allocations.
    /// The configuration is initialized once on first call and cached statically.
    ///
    /// ## Example
    /// ```
    /// use aether_model::KeySpaceConfig;
    ///
    /// // Zero-allocation (uses cached singleton)
    /// let config = KeySpaceConfig::production_cached();
    /// let key = config.channel_key(1001, aether_model::PointType::Telemetry);
    /// ```
    #[inline]
    pub fn production_cached() -> &'static KeySpaceConfig {
        PRODUCTION_CONFIG.get_or_init(Self::production)
    }

    /// Test environment configuration (fully isolated keyspace)
    ///
    /// Adds a "test:" prefix to all keys to prevent test data from polluting production.
    ///
    /// Example:
    /// ```
    /// use aether_model::KeySpaceConfig;
    ///
    /// let test_config = KeySpaceConfig::test();
    /// // data_prefix: "test:io"
    /// // routing_table: "test:route:c2m"
    /// ```
    pub fn test() -> Self {
        Self {
            data_prefix: "test:io".to_string(),
            inst_prefix: "test:inst".to_string(),
            routing_table: "test:route:c2m".to_string(),
            target_prefix: Some("test:io".to_string()),
            inst_name_pattern: Some("test:inst:*:name".to_string()),
        }
    }

    /// M2C (Model to Channel) routing configuration
    ///
    /// Used by automation.set_action_point to route model actions to channels.
    ///
    /// Auto settings:
    /// - target_prefix: points to io data keys
    /// - inst_name_pattern: instance name lookup pattern
    /// - routing_table: switch to m2c routing table
    ///
    /// Example:
    /// ```
    /// use aether_model::KeySpaceConfig;
    ///
    /// let prod_config = KeySpaceConfig::production();
    /// let m2c_config = prod_config.for_m2c();
    /// // routing_table: "route:m2c"
    /// // target_prefix: Some("io")
    /// // inst_name_pattern: Some("inst:*:name")
    /// ```
    pub fn for_m2c(&self) -> Self {
        let target_prefix = self.data_prefix.clone();
        let inst_name_pattern = format!("{}:*:name", self.inst_prefix);
        let routing_table = if self.routing_table.contains("test:") {
            "test:route:m2c".to_string()
        } else {
            "route:m2c".to_string()
        };

        Self {
            data_prefix: self.inst_prefix.clone(), // Not used in M2C
            inst_prefix: self.inst_prefix.clone(),
            routing_table,
            target_prefix: Some(target_prefix),
            inst_name_pattern: Some(inst_name_pattern),
        }
    }

    // ============================================================
    // Redis key generation methods (Single Source of Truth)
    // ============================================================

    /// Build channel data key: io:{channel_id}:{type}
    ///
    /// # Examples
    /// ```
    /// use aether_model::{KeySpaceConfig, PointType};
    ///
    /// let config = KeySpaceConfig::production();
    /// assert_eq!(config.channel_key(1001, PointType::Telemetry), "io:1001:T");
    /// ```
    pub fn channel_key(&self, channel_id: u32, point_type: PointType) -> String {
        format!(
            "{}:{}:{}",
            self.data_prefix,
            channel_id,
            point_type.as_str()
        )
    }

    /// Build channel timestamp key: io:{channel_id}:{type}:ts
    pub fn channel_ts_key(&self, channel_id: u32, point_type: PointType) -> String {
        format!(
            "{}:{}:{}:ts",
            self.data_prefix,
            channel_id,
            point_type.as_str()
        )
    }

    /// Build channel raw value key: io:{channel_id}:{type}:raw
    pub fn channel_raw_key(&self, channel_id: u32, point_type: PointType) -> String {
        format!(
            "{}:{}:{}:raw",
            self.data_prefix,
            channel_id,
            point_type.as_str()
        )
    }

    /// Build instance measurement key: inst:{instance_id}:M
    ///
    /// # Examples
    /// ```
    /// use aether_model::KeySpaceConfig;
    ///
    /// let config = KeySpaceConfig::production();
    /// assert_eq!(config.instance_measurement_key(1), "inst:1:M");
    /// ```
    pub fn instance_measurement_key(&self, instance_id: u32) -> String {
        format!("{}:{}:M", self.inst_prefix, instance_id)
    }

    /// Build instance action key: inst:{instance_id}:A
    pub fn instance_action_key(&self, instance_id: u32) -> String {
        format!("{}:{}:A", self.inst_prefix, instance_id)
    }

    /// Build instance measurement timestamp key: inst:{instance_id}:M:ts
    ///
    /// Sidecar hash storing per-point epoch-ms timestamps, parallel to the
    /// value hash at `instance_measurement_key()`. Fields are point_id strings,
    /// values are i64 epoch-ms encoded as UTF-8 strings (matches `channel_ts_key`).
    pub fn instance_measurement_ts_key(&self, instance_id: u32) -> String {
        format!("{}:{}:M:ts", self.inst_prefix, instance_id)
    }

    /// Build instance action timestamp key: inst:{instance_id}:A:ts
    ///
    /// Sidecar hash storing per-point epoch-ms timestamps, parallel to the
    /// value hash at `instance_action_key()`.
    pub fn instance_action_ts_key(&self, instance_id: u32) -> String {
        format!("{}:{}:A:ts", self.inst_prefix, instance_id)
    }

    /// Build instance name key: inst:{instance_id}:name
    pub fn instance_name_key(&self, instance_id: u32) -> String {
        format!("{}:{}:name", self.inst_prefix, instance_id)
    }

    /// Build channels hash key: io:channels
    ///
    /// Stores all channel ID→name mappings in a single hash for efficient lookup.
    /// - HSET: Set a single channel name
    /// - HDEL: Delete a single channel
    /// - HGETALL: Get all ID→name mappings
    /// - HKEYS: Get all channel IDs
    ///
    /// # Examples
    /// ```
    /// use aether_model::KeySpaceConfig;
    ///
    /// let config = KeySpaceConfig::production();
    /// assert_eq!(config.channels_hash_key(), "io:channels");
    ///
    /// let test_config = KeySpaceConfig::test();
    /// assert_eq!(test_config.channels_hash_key(), "test:io:channels");
    /// ```
    pub fn channels_hash_key(&self) -> String {
        format!("{}:channels", self.data_prefix)
    }

    /// Build channel online status hash key: io:online
    ///
    /// Stores channel online status in a single hash.
    /// - Field: channel_id (string)
    /// - Value: "1" (online) or "0" (offline)
    ///
    /// # Examples
    /// ```
    /// use aether_model::KeySpaceConfig;
    ///
    /// let config = KeySpaceConfig::production();
    /// assert_eq!(config.channel_online_key(), "io:online");
    /// ```
    pub fn channel_online_key(&self) -> String {
        format!("{}:online", self.data_prefix)
    }

    /// Build instance measurement point key: inst:{instance_id}:M:{point_id}
    ///
    /// # Examples
    /// ```
    /// use aether_model::KeySpaceConfig;
    /// let config = KeySpaceConfig::production();
    /// assert_eq!(config.instance_measurement_point_key(1, "101"), "inst:1:M:101");
    /// ```
    pub fn instance_measurement_point_key(&self, instance_id: u32, point_id: &str) -> String {
        format!("{}:{}:M:{}", self.inst_prefix, instance_id, point_id)
    }

    /// Build instance action point key: inst:{instance_id}:A:{point_id}
    ///
    /// # Examples
    /// ```
    /// use aether_model::KeySpaceConfig;
    /// let config = KeySpaceConfig::production();
    /// assert_eq!(config.instance_action_point_key(1, "1"), "inst:1:A:1");
    /// ```
    pub fn instance_action_point_key(&self, instance_id: u32, point_id: &str) -> String {
        format!("{}:{}:A:{}", self.inst_prefix, instance_id, point_id)
    }

    /// Build instance pattern for SCAN/KEYS: inst:{instance_id}:*
    pub fn instance_pattern(&self, instance_id: u32) -> String {
        format!("{}:{}:*", self.inst_prefix, instance_id)
    }

    /// Build instance name index key: inst:name:index
    ///
    /// This is a global hash table for O(1) instance name → ID lookup.
    /// Unlike other instance keys, this is NOT per-instance but a global index.
    ///
    /// # Examples
    /// ```
    /// use aether_model::KeySpaceConfig;
    ///
    /// let config = KeySpaceConfig::production();
    /// assert_eq!(config.instance_name_index_key(), "inst:name:index");
    ///
    /// let test_config = KeySpaceConfig::test();
    /// assert_eq!(test_config.instance_name_index_key(), "test:inst:name:index");
    /// ```
    pub fn instance_name_index_key(&self) -> String {
        format!("{}:name:index", self.inst_prefix)
    }

    /// Build C2M route key: {channel_id}:{type}:{point_id}
    ///
    /// Used as hash field in route:c2m routing table
    pub fn c2m_route_key(&self, channel_id: u32, point_type: PointType, point_id: &str) -> String {
        format!("{}:{}:{}", channel_id, point_type.as_str(), point_id)
    }

    // ============================================================
    // Route key prefix methods (for pattern matching in routing tables)
    // ============================================================

    /// Build measurement route prefix: {instance_id}:M:
    ///
    /// Used for `starts_with` matching in C2M routing table values.
    /// This is an associated function (no `&self`) because route key format
    /// is independent of keyspace configuration.
    ///
    /// # Examples
    /// ```
    /// use aether_model::KeySpaceConfig;
    ///
    /// let prefix = KeySpaceConfig::route_measurement_prefix(10);
    /// assert_eq!(prefix, "10:M:");
    /// assert!("10:M:101".starts_with(&prefix));
    /// ```
    pub fn route_measurement_prefix(instance_id: u32) -> String {
        format!("{}:M:", instance_id)
    }

    /// Build action route prefix: {instance_id}:A:
    ///
    /// Used for `starts_with` matching in M2C routing table keys.
    /// This is an associated function (no `&self`) because route key format
    /// is independent of keyspace configuration.
    ///
    /// # Examples
    /// ```
    /// use aether_model::KeySpaceConfig;
    ///
    /// let prefix = KeySpaceConfig::route_action_prefix(10);
    /// assert_eq!(prefix, "10:A:");
    /// assert!("10:A:1".starts_with(&prefix));
    /// ```
    pub fn route_action_prefix(instance_id: u32) -> String {
        format!("{}:A:", instance_id)
    }

    // ============================================================
    // Product-related keys (automation)
    // ============================================================

    /// Returns the environment prefix ("" for production, "test:" for test)
    ///
    /// Derived from `inst_prefix` — production uses "inst", test uses "test:inst".
    fn env_prefix(&self) -> &str {
        self.inst_prefix.strip_suffix("inst").unwrap_or("")
    }

    /// Build product info key: automation:product:{product_name}
    pub fn product_key(&self, product_name: &str) -> String {
        format!("{}automation:product:{}", self.env_prefix(), product_name)
    }

    /// Product index set key: automation:products (or test:automation:products)
    pub fn product_index_key(&self) -> String {
        format!("{}automation:products", self.env_prefix())
    }

    // ============================================================
    // Rule execution keys
    // ============================================================

    /// Build rule execution state key: rule:{rule_id}:exec
    pub fn rule_exec_key(&self, rule_id: i64) -> String {
        format!("{}rule:{}:exec", self.env_prefix(), rule_id)
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;

    #[test]
    fn test_production_config() {
        let config = KeySpaceConfig::production();
        assert_eq!(config.data_prefix, "io");
        assert_eq!(config.inst_prefix, "inst");
        assert_eq!(config.routing_table, "route:c2m");
        assert_eq!(config.target_prefix, None);
        assert_eq!(config.inst_name_pattern, None);
    }

    #[test]
    fn test_test_config() {
        let config = KeySpaceConfig::test();
        assert_eq!(config.data_prefix, "test:io");
        assert_eq!(config.inst_prefix, "test:inst");
        assert_eq!(config.routing_table, "test:route:c2m");
        assert_eq!(config.target_prefix, Some("test:io".to_string()));
        assert_eq!(
            config.inst_name_pattern,
            Some("test:inst:*:name".to_string())
        );
    }

    #[test]
    fn test_for_m2c() {
        let config = KeySpaceConfig::production().for_m2c();
        assert_eq!(config.routing_table, "route:m2c");
        assert_eq!(config.target_prefix, Some("io".to_string()));
        assert_eq!(config.inst_name_pattern, Some("inst:*:name".to_string()));
    }

    #[test]
    fn test_for_m2c_test_env() {
        let config = KeySpaceConfig::test().for_m2c();
        assert_eq!(config.routing_table, "test:route:m2c");
        assert_eq!(config.target_prefix, Some("test:io".to_string()));
        assert_eq!(
            config.inst_name_pattern,
            Some("test:inst:*:name".to_string())
        );
    }

    #[test]
    fn test_serialization() {
        let config = KeySpaceConfig::test();
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: KeySpaceConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, deserialized);
    }

    #[test]
    fn test_default() {
        let config = KeySpaceConfig::default();
        assert_eq!(config, KeySpaceConfig::production());
    }

    // ============================================================
    // Key generation tests
    // ============================================================

    #[test]
    fn test_channel_key_generation() {
        let config = KeySpaceConfig::production();

        assert_eq!(config.channel_key(1001, PointType::Telemetry), "io:1001:T");
        assert_eq!(config.channel_key(1001, PointType::Signal), "io:1001:S");
        assert_eq!(config.channel_key(1001, PointType::Control), "io:1001:C");
        assert_eq!(config.channel_key(1001, PointType::Adjustment), "io:1001:A");
    }

    #[test]
    fn test_channel_ts_and_raw_keys() {
        let config = KeySpaceConfig::production();

        assert_eq!(
            config.channel_ts_key(1001, PointType::Telemetry),
            "io:1001:T:ts"
        );
        assert_eq!(
            config.channel_raw_key(1001, PointType::Telemetry),
            "io:1001:T:raw"
        );
    }

    #[test]
    fn test_instance_keys() {
        let config = KeySpaceConfig::production();

        assert_eq!(config.instance_measurement_key(1), "inst:1:M");
        assert_eq!(config.instance_action_key(1), "inst:1:A");
        assert_eq!(config.instance_measurement_ts_key(1), "inst:1:M:ts");
        assert_eq!(config.instance_action_ts_key(1), "inst:1:A:ts");
        assert_eq!(config.instance_name_key(1), "inst:1:name");
        assert_eq!(config.instance_pattern(1), "inst:1:*");
        assert_eq!(config.instance_name_index_key(), "inst:name:index");

        // Test environment — prefix must apply to sidecar ts keys too
        let test_config = KeySpaceConfig::test();
        assert_eq!(
            test_config.instance_name_index_key(),
            "test:inst:name:index"
        );
        assert_eq!(
            test_config.instance_measurement_ts_key(1),
            "test:inst:1:M:ts"
        );
        assert_eq!(test_config.instance_action_ts_key(1), "test:inst:1:A:ts");
    }

    #[test]
    fn test_channels_hash_key() {
        let config = KeySpaceConfig::production();
        assert_eq!(config.channels_hash_key(), "io:channels");

        // Test environment
        let test_config = KeySpaceConfig::test();
        assert_eq!(test_config.channels_hash_key(), "test:io:channels");
    }

    #[test]
    fn test_channel_online_key() {
        let config = KeySpaceConfig::production();
        assert_eq!(config.channel_online_key(), "io:online");

        // Test environment
        let test_config = KeySpaceConfig::test();
        assert_eq!(test_config.channel_online_key(), "test:io:online");
    }

    #[test]
    fn test_instance_point_keys() {
        let config = KeySpaceConfig::production();

        assert_eq!(
            config.instance_measurement_point_key(1, "101"),
            "inst:1:M:101"
        );
        assert_eq!(config.instance_action_point_key(1, "1"), "inst:1:A:1");

        // Test environment
        let test_config = KeySpaceConfig::test();
        assert_eq!(
            test_config.instance_measurement_point_key(1, "101"),
            "test:inst:1:M:101"
        );
        assert_eq!(
            test_config.instance_action_point_key(1, "1"),
            "test:inst:1:A:1"
        );
    }

    #[test]
    fn test_routing_keys() {
        let config = KeySpaceConfig::production();

        // C2M route key
        assert_eq!(
            config.c2m_route_key(1001, PointType::Telemetry, "T1"),
            "1001:T:T1"
        );
    }

    #[test]
    fn test_route_prefixes() {
        // Measurement prefix
        assert_eq!(KeySpaceConfig::route_measurement_prefix(10), "10:M:");
        assert_eq!(KeySpaceConfig::route_measurement_prefix(1), "1:M:");

        // Action prefix
        assert_eq!(KeySpaceConfig::route_action_prefix(10), "10:A:");
        assert_eq!(KeySpaceConfig::route_action_prefix(1), "1:A:");

        // Verify starts_with matching works
        let m_prefix = KeySpaceConfig::route_measurement_prefix(10);
        assert!("10:M:101".starts_with(&m_prefix));
        assert!(!"10:A:101".starts_with(&m_prefix));

        let a_prefix = KeySpaceConfig::route_action_prefix(10);
        assert!("10:A:1".starts_with(&a_prefix));
        assert!(!"10:M:1".starts_with(&a_prefix));
    }

    #[test]
    fn test_key_generation_with_test_environment() {
        let config = KeySpaceConfig::test();

        // All keys should have test: prefix
        assert_eq!(
            config.channel_key(1001, PointType::Telemetry),
            "test:io:1001:T"
        );
        assert_eq!(config.instance_measurement_key(1), "test:inst:1:M");
    }

    #[test]
    fn test_key_generation_returns_string() {
        let config = KeySpaceConfig::production();
        let key: String = config.channel_key(1001, PointType::Telemetry);

        // Verify direct String return (no Cow overhead)
        assert_eq!(key, "io:1001:T");
    }

    #[test]
    fn test_product_and_index_keys_respect_env_prefix() {
        let prod = KeySpaceConfig::production();
        assert_eq!(prod.product_index_key(), "automation:products");
        assert_eq!(prod.product_key("sensor"), "automation:product:sensor");
        assert_eq!(prod.rule_exec_key(42), "rule:42:exec");

        let test = KeySpaceConfig::test();
        assert_eq!(test.product_index_key(), "test:automation:products");
        assert_eq!(test.product_key("sensor"), "test:automation:product:sensor");
        assert_eq!(test.rule_exec_key(42), "test:rule:42:exec");
    }
}
