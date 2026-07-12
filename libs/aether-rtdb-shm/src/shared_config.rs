//! Business-side SHM configuration and channel-to-slot index.
//!
//! Pure-infra constants and path/time helpers live in `crate::core::config`.
//! This module owns the business-aware pieces: `SharedConfig` (capacity in
//! "instances" / "channels"), `ChannelToSlotIndex` (uses `PointType`), and
//! the legacy `is_shm_available` shim that takes a `SharedConfig`.
//!
//! Pre-existing re-exports (`SHARED_MAGIC`, `DEFAULT_SHM_PATH`,
//! `default_shm_path`, `timestamp_ms`, `DEFAULT_SNAPSHOT_INTERVAL_SECS`)
//! remain available at this module's path for backward compatibility — they
//! now forward to `crate::core::config`.

use aether_domain::PointKind;
use aether_model::PointType;
use aether_shm_bridge::ChannelPointManifest;
use rustc_hash::FxHashMap;
use std::collections::BTreeMap;
use std::path::PathBuf;
use tracing::info;

// ========== Re-exports from core::config (compat shim) ==========

pub use crate::core::config::{
    DEFAULT_SHM_PATH, DEFAULT_SNAPSHOT_INTERVAL_SECS, SHARED_MAGIC, default_shm_path, timestamp_ms,
};

// ========== SharedConfig ==========

/// Configuration for shared memory
#[derive(Debug, Clone)]
pub struct SharedConfig {
    /// Path to shared memory file
    pub path: PathBuf,
    /// Maximum number of instances
    pub max_instances: usize,
    /// Maximum points per instance (measurement + action)
    pub max_points_per_instance: usize,
    /// Maximum number of channels
    pub max_channels: usize,
    /// Maximum points per channel (all types combined)
    pub max_points_per_channel: usize,

    // ========== Snapshot Configuration ==========
    /// Path to snapshot file (None = disabled)
    pub snapshot_path: Option<PathBuf>,
    /// Automatic snapshot interval (None = disabled)
    pub snapshot_interval: Option<std::time::Duration>,
    /// Whether to restore from snapshot on startup
    pub restore_on_start: bool,
}

impl Default for SharedConfig {
    fn default() -> Self {
        Self {
            path: default_shm_path(),
            max_instances: 1024,
            max_points_per_instance: 65536,
            max_channels: 1024,
            max_points_per_channel: 65536,
            // Snapshot defaults
            snapshot_path: None,
            snapshot_interval: None,
            restore_on_start: true,
        }
    }
}

impl SharedConfig {
    /// Create config with custom path
    pub fn with_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.path = path.into();
        self
    }

    /// Create config with custom max_instances
    pub fn with_max_instances(mut self, max_instances: usize) -> Self {
        self.max_instances = max_instances;
        self
    }

    /// Create config with custom max_points_per_instance
    pub fn with_max_points_per_instance(mut self, max_points: usize) -> Self {
        self.max_points_per_instance = max_points;
        self
    }

    /// Create config with custom max_channels
    pub fn with_max_channels(mut self, max_channels: usize) -> Self {
        self.max_channels = max_channels;
        self
    }

    /// Create config with custom max_points_per_channel
    pub fn with_max_points_per_channel(mut self, max_points: usize) -> Self {
        self.max_points_per_channel = max_points;
        self
    }

    /// Get shared memory path
    #[inline]
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Get max slots for unified shared memory
    ///
    /// This is used by the new unified_shm implementation.
    /// Returns the configured value or None if not set.
    #[inline]
    pub fn max_slots(&self) -> Option<u32> {
        // Use max_channels * max_points_per_channel as default
        // This can be overridden by explicit configuration
        Some((self.max_channels * self.max_points_per_channel) as u32)
    }

    /// Create config with explicit max_slots for unified shm
    pub fn with_max_slots(mut self, max_slots: usize) -> Self {
        // Store in max_channels * max_points_per_channel
        // This is a simplification - in practice we might add a dedicated field
        self.max_channels = max_slots;
        self.max_points_per_channel = 1;
        self
    }

    // ========== Snapshot Configuration Methods ==========

    /// Configure snapshot file path
    ///
    /// If set, enables snapshot saving. If None, snapshots are disabled.
    pub fn with_snapshot_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.snapshot_path = Some(path.into());
        self
    }

    /// Configure automatic snapshot interval
    ///
    /// If set, a background task will save snapshots at this interval.
    /// Requires snapshot_path to be set.
    pub fn with_snapshot_interval(mut self, interval: std::time::Duration) -> Self {
        self.snapshot_interval = Some(interval);
        self
    }

    /// Configure whether to restore from snapshot on startup
    ///
    /// Default is true. If false, always starts with fresh empty data.
    pub fn with_restore_on_start(mut self, restore: bool) -> Self {
        self.restore_on_start = restore;
        self
    }

    /// Get snapshot path
    #[inline]
    pub fn snapshot_path(&self) -> Option<&PathBuf> {
        self.snapshot_path.as_ref()
    }

    /// Get snapshot interval
    #[inline]
    pub fn snapshot_interval(&self) -> Option<std::time::Duration> {
        self.snapshot_interval
    }

    /// Check if should restore on start
    #[inline]
    pub fn restore_on_start(&self) -> bool {
        self.restore_on_start
    }

    /// Build snapshot config from environment variables
    ///
    /// Reads:
    /// - SHM_SNAPSHOT_PATH: Path to snapshot file (default: data/shm-snapshot.bin)
    /// - SHM_SNAPSHOT_INTERVAL: Interval in seconds (default: 300 = 5 minutes)
    /// - SHM_RESTORE_ON_START: "true" or "false" (default: true)
    pub fn with_snapshot_from_env(mut self) -> Self {
        use std::env;

        // Snapshot path
        if let Ok(path) = env::var("SHM_SNAPSHOT_PATH") {
            self.snapshot_path = Some(PathBuf::from(path));
        } else {
            // Default path if not explicitly disabled
            self.snapshot_path = Some(PathBuf::from("data/shm-snapshot.bin"));
        }

        // Snapshot interval
        if let Ok(interval_str) = env::var("SHM_SNAPSHOT_INTERVAL") {
            if let Ok(secs) = interval_str.parse::<u64>() {
                self.snapshot_interval = Some(std::time::Duration::from_secs(secs));
            }
        } else {
            // Default: 5 minutes
            self.snapshot_interval = Some(std::time::Duration::from_secs(
                DEFAULT_SNAPSHOT_INTERVAL_SECS,
            ));
        }

        // Restore on start
        if let Ok(restore_str) = env::var("SHM_RESTORE_ON_START") {
            self.restore_on_start = restore_str.to_lowercase() != "false";
        }

        self
    }
}

// ========== ChannelToSlotIndex ==========

/// Legacy direct-index projection of [`ChannelPointManifest`].
///
/// Pre-computed at startup to eliminate runtime C2M routing lookup.
/// Provides O(1) channel-to-slot mapping for the hottest path.
///
/// # Architecture
/// ```text
/// Before (2 lookups):
///   Channel → C2M Route → Instance → SharedMemory Lookup → Slot
///
/// After (1 lookup):
///   Channel → ChannelToSlotIndex → Slot
/// ```
///
/// # Performance
/// - Before: ~90ns (two hash lookups)
/// - After: ~50ns (single hash lookup)
///
/// Removal criterion: replace raw `PointType` lookups in production callers
/// with typed manifest addresses, then delete this projection and its
/// arbitrary-insert test compatibility path.
#[derive(Debug)]
pub struct ChannelToSlotIndex {
    /// (channel_id, point_type, point_id) → slot index (0-based)
    index: FxHashMap<(u32, PointType, u32), usize>,
    /// Statistics
    mapped_count: usize,
    /// Formal typed manifest behind production indexes. `None` is reserved for
    /// the legacy arbitrary-insert test helper.
    formal: Option<ChannelPointManifest>,
}

impl ChannelToSlotIndex {
    /// Look up slot index for a channel point
    ///
    /// # Arguments
    /// * `channel_id` - Channel identifier
    /// * `point_type` - Point type (Telemetry, Signal, Control, Adjustment)
    /// * `point_id` - Point identifier within the channel
    ///
    /// # Returns
    /// Slot index (0-based) for `UnifiedWriter::set_direct`, or None if not mapped
    #[inline]
    pub fn lookup(&self, channel_id: u32, point_type: PointType, point_id: u32) -> Option<usize> {
        self.index.get(&(channel_id, point_type, point_id)).copied()
    }

    /// Get number of mapped channel points
    pub fn len(&self) -> usize {
        self.mapped_count
    }

    /// Check if index is empty
    pub fn is_empty(&self) -> bool {
        self.mapped_count == 0
    }

    /// Iterate all (key, slot) pairs — used by [`ReverseSlotIndex`]
    pub fn iter(&self) -> impl Iterator<Item = (&(u32, PointType, u32), &usize)> {
        self.index.iter()
    }

    /// Create an empty index (test helper — available in all test builds)
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn new_empty() -> Self {
        Self {
            index: FxHashMap::default(),
            mapped_count: 0,
            formal: Some(ChannelPointManifest::default()),
        }
    }

    /// Create an empty index for use in downstream crate tests.
    ///
    /// Unlike `new_empty()` (which is `#[cfg(test)]`), this method is always
    /// compiled so that test code in other crates (e.g. `aether-rules`) can
    /// construct a slot index without a real `UnifiedWriter`.
    ///
    /// The returned index contains no mappings; `lookup()` always returns `None`.
    /// This is intentional for tests that only need the index to exist.
    pub fn empty_for_test() -> Self {
        Self {
            index: FxHashMap::default(),
            mapped_count: 0,
            formal: Some(ChannelPointManifest::default()),
        }
    }

    /// Insert a single mapping (test helper — available in all test builds)
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn insert(&mut self, channel_id: u32, point_type: PointType, point_id: u32, slot: usize) {
        self.index.insert((channel_id, point_type, point_id), slot);
        self.mapped_count = self.index.len();
        self.formal = None;
    }

    /// Build from UnifiedWriter's channel_layouts
    ///
    /// This method creates a ChannelToSlotIndex from the unified shared memory format.
    /// Stores slot indices (0-based) that map directly to `UnifiedWriter::set_direct`.
    ///
    /// # Arguments
    /// * `writer` - UnifiedWriter with registered channel points
    ///
    /// # Returns
    /// ChannelToSlotIndex with pre-computed mappings
    pub fn from_unified_writer(writer: &crate::unified_shm::UnifiedWriter) -> Self {
        let counts = writer
            .channel_layouts()
            .iter()
            .enumerate()
            .filter(|(_, layout)| layout.total_points > 0)
            .map(|(channel_id, layout)| (channel_id as u32, layout.type_counts))
            .collect::<BTreeMap<_, _>>();
        Self::from_manifest(ChannelPointManifest::from_map(counts))
    }

    /// Projects a formal manifest into the raw legacy lookup shape.
    pub(crate) fn from_manifest(formal: ChannelPointManifest) -> Self {
        let index = formal
            .iter_physical_points()
            .map(|(slot, address)| {
                (
                    (
                        address.channel_id().get(),
                        legacy_point_type(address.kind()),
                        address.point_id().get(),
                    ),
                    slot,
                )
            })
            .collect::<FxHashMap<_, _>>();
        let mapped_count = formal.point_count();
        info!(
            "ChannelToSlotIndex compatibility projection: {} direct mappings",
            mapped_count
        );

        Self {
            index,
            mapped_count,
            formal: Some(formal),
        }
    }

    /// Returns the formal manifest when this index came from a production
    /// writer. Arbitrary legacy test inserts deliberately return `None`.
    pub(crate) const fn formal_manifest(&self) -> Option<&ChannelPointManifest> {
        self.formal.as_ref()
    }
}

pub(crate) const fn legacy_point_type(kind: PointKind) -> PointType {
    match kind {
        PointKind::Telemetry => PointType::Telemetry,
        PointKind::Status => PointType::Signal,
        PointKind::Command => PointType::Control,
        PointKind::Action => PointType::Adjustment,
    }
}

// ========== Utility Functions ==========

/// Check if shared memory path is available (parent directory exists).
pub fn is_shm_available(config: &SharedConfig) -> bool {
    crate::core::config::parent_dir_exists(&config.path)
}
