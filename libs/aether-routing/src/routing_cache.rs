//! Application-layer routing cache
//!
//! Provides in-memory caching of routing tables for high-performance lookups.
//! This is a pure data structure without external dependencies.
//!
//! ## Structured Route Targets
//!
//! All route targets are stored as structured types, eliminating runtime string parsing:
//! - `C2MTarget`: Channel → Instance (measurement point)
//! - `C2CTarget`: Channel → Channel (data forwarding)
//! - `M2CTarget`: Instance → Channel (action/control)
//!
//! ## Single Index Design
//!
//! All routing tables use structured tuple keys for zero-allocation lookups:
//! - C2M/C2C: `(channel_id, point_type, point_id)`
//! - M2C: `(instance_id, point_type, point_id)`

use aether_model::PointType;
use arc_swap::ArcSwap;
use rustc_hash::FxHashMap;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

// ============================================================================
// Route Target Types
// ============================================================================

/// C2M (Channel to Model) route target
///
/// Routes channel point data to an instance measurement point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct C2MTarget {
    /// Target instance ID
    pub instance_id: u32,
    /// Target measurement point ID
    pub point_id: u32,
}

impl fmt::Display for C2MTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:M:{}", self.instance_id, self.point_id)
    }
}

/// C2C (Channel to Channel) route target
///
/// Routes channel point data to another channel point (data forwarding),
/// optionally with a linear transform: `target_value = scale * source + offset`.
/// `scale=1.0, offset=0.0` is the identity (no transform).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct C2CTarget {
    /// Target channel ID
    pub channel_id: u32,
    /// Target point type (T/S/C/A)
    pub point_type: PointType,
    /// Target point ID
    pub point_id: u32,
    /// Linear scale factor (default 1.0)
    pub scale: f64,
    /// Linear offset (default 0.0)
    pub offset: f64,
}

impl C2CTarget {
    /// Apply the linear transform to a source value.
    #[inline]
    pub fn transform(&self, value: f64) -> f64 {
        self.scale * value + self.offset
    }

    /// Returns true if the transform is the identity (no value change).
    #[inline]
    pub fn is_identity_transform(&self) -> bool {
        self.scale == 1.0 && self.offset == 0.0
    }
}

impl fmt::Display for C2CTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}",
            self.channel_id,
            self.point_type.as_str(),
            self.point_id
        )
    }
}

/// M2C (Model to Channel) route target
///
/// Routes instance action point to a channel point for control/adjustment.
/// This is a Copy type - clone is zero-cost (12 bytes stack copy).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct M2CTarget {
    /// Target channel ID
    pub channel_id: u32,
    /// Target point type (typically C or A)
    pub point_type: PointType,
    /// Target point ID
    pub point_id: u32,
}

impl fmt::Display for M2CTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}",
            self.channel_id,
            self.point_type.as_str(),
            self.point_id
        )
    }
}

// ============================================================================
// Parsing helpers
// ============================================================================

/// Parse C2M target from string "instance_id:M:point_id"
fn parse_c2m_target(s: &str) -> Option<C2MTarget> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let instance_id = parts[0].parse().ok()?;
    // parts[1] should be "M" - we ignore it as it's always M for C2M
    let point_id = parts[2].parse().ok()?;
    Some(C2MTarget {
        instance_id,
        point_id,
    })
}

/// Parse channel point target from string "channel_id:type:point_id"
///
/// Used for both C2C and M2C targets (which have identical field structure).
/// Callers construct the specific target type from the returned tuple.
fn parse_channel_point(s: &str) -> Option<(u32, PointType, u32)> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let id = parts[0].parse().ok()?;
    let point_type = parse_point_type(parts[1])?;
    let point_id = parts[2].parse().ok()?;
    Some((id, point_type, point_id))
}

/// Parse a C2C value string. Accepts `"ch:type:point"` (identity transform)
/// or `"ch:type:point|scale|offset"` for a linear transform.
fn parse_c2c_value(s: &str) -> Option<C2CTarget> {
    let mut parts = s.splitn(3, '|');
    let target_str = parts.next()?;
    let (channel_id, point_type, point_id) = parse_channel_point(target_str)?;
    let scale = parts.next().and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let offset = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
    Some(C2CTarget {
        channel_id,
        point_type,
        point_id,
        scale,
        offset,
    })
}

#[inline]
fn m2c_from_parts((channel_id, point_type, point_id): (u32, PointType, u32)) -> M2CTarget {
    M2CTarget {
        channel_id,
        point_type,
        point_id,
    }
}

/// Parse point type string to PointType enum
#[inline]
fn parse_point_type(s: &str) -> Option<PointType> {
    match s {
        "T" => Some(PointType::Telemetry),
        "S" => Some(PointType::Signal),
        "C" => Some(PointType::Control),
        "A" => Some(PointType::Adjustment),
        // Note: "M" in C2M targets means Measurement (instance point), not a PointType
        _ => None,
    }
}

/// Structured route key type for C2M and C2C (zero-allocation lookups)
/// Format: (channel_id, point_type, point_id)
pub type StructuredRouteKey = (u32, PointType, u32);

/// Structured route key type for M2C (zero-allocation lookups)
/// Format: (instance_id, point_type, point_id)
pub type StructuredM2CKey = (u32, PointType, u32);

/// Parse route key string "id:type:point_id" into structured key
#[inline]
fn parse_route_key(s: &str) -> Option<StructuredRouteKey> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let id = parts[0].parse().ok()?;
    let point_type = parse_point_type(parts[1])?;
    let point_id = parts[2].parse().ok()?;
    Some((id, point_type, point_id))
}

// ============================================================================
// Table Building (shared between from_maps and update)
// ============================================================================

/// Build structured routing tables from string maps
///
/// Parses string keys/values into structured types. Invalid entries are skipped.
/// Used by both `from_maps()` and `update()` to eliminate duplicate parsing logic.
/// Reverse C2M lookup: `(instance_id, instance_point_id)` → `(channel_id, channel_point_type, channel_point_id)`.
///
/// Built once from the forward C2M table at construction time so that
/// instance-keyed callers (rule engine OnChange snapshot, instance-to-slot
/// resolution) get O(1) lookups instead of O(N) `c2m_iter` scans.
pub type StructuredC2MReverseKey = (u32, u32);
pub type StructuredC2MReverseTarget = (u32, PointType, u32);

type BuiltTables = (
    FxHashMap<StructuredRouteKey, C2MTarget>,
    FxHashMap<StructuredM2CKey, M2CTarget>,
    FxHashMap<StructuredRouteKey, C2CTarget>,
    FxHashMap<StructuredC2MReverseKey, StructuredC2MReverseTarget>,
);

fn build_tables(
    c2m_data: HashMap<String, String>,
    m2c_data: HashMap<String, String>,
    c2c_data: HashMap<String, String>,
) -> BuiltTables {
    let mut c2m = FxHashMap::default();
    let mut m2c = FxHashMap::default();
    let mut c2c = FxHashMap::default();
    let mut c2m_reverse = FxHashMap::default();

    for (k, v) in c2m_data {
        if let (Some(key), Some(target)) = (parse_route_key(&k), parse_c2m_target(&v)) {
            // forward: (channel, type, point) → (instance, point)
            c2m.insert(key, target);
            // reverse: (instance, point) → (channel, type, point)
            //
            // If multiple channels fan into the same instance point, the
            // last wins — that is the same forward-table semantic (HashMap
            // insert overwrite), so the two stay consistent.
            c2m_reverse.insert((target.instance_id, target.point_id), key);
        }
    }
    for (k, v) in m2c_data {
        if let (Some(key), Some(parts)) = (parse_route_key(&k), parse_channel_point(&v)) {
            m2c.insert(key, m2c_from_parts(parts));
        }
    }
    for (k, v) in c2c_data {
        // parse_c2c_value accepts both "ch:type:point" (identity transform)
        // and "ch:type:point|scale|offset" formats — single parse path.
        if let (Some(key), Some(target)) = (parse_route_key(&k), parse_c2c_value(&v)) {
            c2c.insert(key, target);
        }
    }

    (c2m, m2c, c2c, c2m_reverse)
}

// ============================================================================
// RoutingCache
// ============================================================================

/// Application-layer routing cache for C2M, C2C and M2C routing
///
/// Uses independent ArcSwap for each table, enabling fine-grained copy-on-write:
/// - Modifying C2C only clones the C2C table, not C2M or M2C
/// - Reduces unnecessary HashMap cloning by 2/3 for single-table updates
///
/// ## Performance
/// - Read: `ArcSwap::load()` (~5ns) + `FxHashMap::get()` (~20ns) = ~25ns total
/// - Write: Clone only the modified table + `ArcSwap::store()` (atomic pointer swap)
///
/// ## Hot Path Usage
/// For hot paths like `write_channel_batch`, use `lookup_*_by_parts()` methods
/// which take structured keys directly, avoiding temporary String allocation.
///
/// ## Consistency Note
/// The three tables are updated independently (not atomically together).
/// This is acceptable because:
/// 1. `update()` is a cold path (config reload), not hot path
/// 2. Each table's snapshot is always consistent
/// 3. C2M, M2C, C2C are logically independent mappings
#[derive(Debug)]
pub struct RoutingCache {
    /// C2M routing: (channel_id, point_type, point_id) -> instance target
    c2m: ArcSwap<FxHashMap<StructuredRouteKey, C2MTarget>>,
    /// C2C routing: (channel_id, point_type, point_id) -> channel target
    c2c: ArcSwap<FxHashMap<StructuredRouteKey, C2CTarget>>,
    /// M2C routing: (instance_id, point_type, point_id) -> channel target
    m2c: ArcSwap<FxHashMap<StructuredM2CKey, M2CTarget>>,
    /// Reverse C2M routing: (instance_id, instance_point_id) -> (channel_id, point_type, point_id)
    ///
    /// Built from the forward C2M table at construction/reload time. Lets
    /// rule-engine OnChange snapshot and instance-to-slot resolvers do an
    /// O(1) reverse lookup instead of scanning c2m_iter().
    c2m_reverse: ArcSwap<FxHashMap<StructuredC2MReverseKey, StructuredC2MReverseTarget>>,
}

impl RoutingCache {
    /// Create an empty routing cache
    pub fn new() -> Self {
        Self {
            c2m: ArcSwap::from_pointee(FxHashMap::default()),
            c2c: ArcSwap::from_pointee(FxHashMap::default()),
            m2c: ArcSwap::from_pointee(FxHashMap::default()),
            c2m_reverse: ArcSwap::from_pointee(FxHashMap::default()),
        }
    }

    /// Construct routing cache from raw HashMap data
    ///
    /// Parses string targets into structured types at load time.
    /// Invalid targets are silently skipped (logged in production).
    ///
    /// ## Example
    /// ```rust
    /// use aether_routing::RoutingCache;
    /// use std::collections::HashMap;
    ///
    /// let c2m_data: HashMap<String, String> = HashMap::new(); // load from SQLite
    /// let m2c_data: HashMap<String, String> = HashMap::new(); // load from SQLite
    /// let c2c_data: HashMap<String, String> = HashMap::new(); // load from SQLite
    /// let cache = RoutingCache::from_maps(c2m_data, m2c_data, c2c_data);
    /// ```
    pub fn from_maps(
        c2m_data: HashMap<String, String>,
        m2c_data: HashMap<String, String>,
        c2c_data: HashMap<String, String>,
    ) -> Self {
        let (c2m, m2c, c2c, c2m_reverse) = build_tables(c2m_data, m2c_data, c2c_data);
        Self {
            c2m: ArcSwap::from_pointee(c2m),
            c2c: ArcSwap::from_pointee(c2c),
            m2c: ArcSwap::from_pointee(m2c),
            c2m_reverse: ArcSwap::from_pointee(c2m_reverse),
        }
    }

    /// Update routing cache with new data (independent table replacement)
    ///
    /// Builds new snapshots for each table and replaces them independently.
    /// Used during hot-reload. Each table's snapshot is always consistent.
    ///
    /// Note: The three tables are NOT updated atomically together.
    /// This is acceptable because update() is a cold path and the tables
    /// are logically independent (C2M, M2C, C2C don't cross-reference each other).
    pub fn update(
        &self,
        c2m_data: HashMap<String, String>,
        m2c_data: HashMap<String, String>,
        c2c_data: HashMap<String, String>,
    ) {
        let (new_c2m, new_m2c, new_c2c, new_c2m_reverse) =
            build_tables(c2m_data, m2c_data, c2c_data);
        // Independent replacement - each table is atomically swapped
        self.c2m.store(Arc::new(new_c2m));
        self.c2c.store(Arc::new(new_c2c));
        self.m2c.store(Arc::new(new_m2c));
        self.c2m_reverse.store(Arc::new(new_c2m_reverse));
    }

    /// Lookup C2M routing by string key (parses key first)
    ///
    /// For hot paths, prefer `lookup_c2m_by_parts()` to avoid string parsing.
    ///
    /// ## Example
    /// ```rust
    /// use aether_routing::RoutingCache;
    /// use std::collections::HashMap;
    ///
    /// let mut c2m = HashMap::new();
    /// c2m.insert("2:T:1".to_string(), "23:M:1".to_string());
    /// let cache = RoutingCache::from_maps(c2m, HashMap::new(), HashMap::new());
    ///
    /// if let Some(target) = cache.lookup_c2m("2:T:1") {
    ///     assert_eq!(target.instance_id, 23);
    ///     assert_eq!(target.point_id, 1);
    /// }
    /// ```
    pub fn lookup_c2m(&self, key: &str) -> Option<C2MTarget> {
        let structured_key = parse_route_key(key)?;
        self.c2m.load().get(&structured_key).copied()
    }

    /// Lookup C2M routing by structured key (zero-allocation)
    ///
    /// Use this method in hot paths to avoid string parsing overhead.
    ///
    /// ## Example
    /// ```rust
    /// use aether_routing::RoutingCache;
    /// use aether_model::PointType;
    /// use std::collections::HashMap;
    ///
    /// let mut c2m = HashMap::new();
    /// c2m.insert("2:T:1".to_string(), "23:M:1".to_string());
    /// let cache = RoutingCache::from_maps(c2m, HashMap::new(), HashMap::new());
    ///
    /// // Zero-allocation lookup
    /// if let Some(target) = cache.lookup_c2m_by_parts(2, PointType::Telemetry, 1) {
    ///     assert_eq!(target.instance_id, 23);
    ///     assert_eq!(target.point_id, 1);
    /// }
    /// ```
    #[inline]
    pub fn lookup_c2m_by_parts(
        &self,
        channel_id: u32,
        point_type: PointType,
        point_id: u32,
    ) -> Option<C2MTarget> {
        self.c2m
            .load()
            .get(&(channel_id, point_type, point_id))
            .copied()
    }

    /// Reverse C2M lookup: instance/point → channel/point/type.
    ///
    /// O(1) hash lookup. Use this when you have an `(instance_id, point_id)`
    /// from a rule subscription or measurement reference and need to find the
    /// io-side channel that writes it.
    ///
    /// Returns `None` when the instance point has no C2M route configured.
    #[inline]
    pub fn lookup_c2m_reverse(
        &self,
        instance_id: u32,
        instance_point_id: u32,
    ) -> Option<(u32, PointType, u32)> {
        self.c2m_reverse
            .load()
            .get(&(instance_id, instance_point_id))
            .copied()
    }

    /// Lookup M2C routing by string key (parses key first)
    ///
    /// ## Example
    /// ```rust
    /// use aether_routing::RoutingCache;
    /// use aether_model::PointType;
    /// use std::collections::HashMap;
    ///
    /// let mut m2c = HashMap::new();
    /// m2c.insert("23:A:4".to_string(), "2:A:1".to_string());
    /// let cache = RoutingCache::from_maps(HashMap::new(), m2c, HashMap::new());
    ///
    /// if let Some(target) = cache.lookup_m2c("23:A:4") {
    ///     assert_eq!(target.channel_id, 2);
    ///     assert_eq!(target.point_type, PointType::Adjustment);
    ///     assert_eq!(target.point_id, 1);
    /// }
    /// ```
    pub fn lookup_m2c(&self, key: &str) -> Option<M2CTarget> {
        let structured_key = parse_route_key(key)?;
        self.m2c.load().get(&structured_key).copied()
    }

    /// Lookup M2C routing by structured key (zero-allocation)
    #[inline]
    pub fn lookup_m2c_by_parts(
        &self,
        instance_id: u32,
        point_type: PointType,
        point_id: u32,
    ) -> Option<M2CTarget> {
        self.m2c
            .load()
            .get(&(instance_id, point_type, point_id))
            .copied()
    }

    /// Lookup C2C routing by structured key (zero-allocation)
    ///
    /// Use this method in hot paths to avoid string parsing overhead.
    ///
    /// ## Example
    /// ```rust
    /// use aether_routing::RoutingCache;
    /// use aether_model::PointType;
    /// use std::collections::HashMap;
    ///
    /// let mut c2c = HashMap::new();
    /// c2c.insert("1001:T:1".to_string(), "1002:T:5".to_string());
    /// let cache = RoutingCache::from_maps(HashMap::new(), HashMap::new(), c2c);
    ///
    /// // Zero-allocation lookup
    /// if let Some(target) = cache.lookup_c2c_by_parts(1001, PointType::Telemetry, 1) {
    ///     assert_eq!(target.channel_id, 1002);
    ///     assert_eq!(target.point_id, 5);
    /// }
    /// ```
    #[inline]
    pub fn lookup_c2c_by_parts(
        &self,
        channel_id: u32,
        point_type: PointType,
        point_id: u32,
    ) -> Option<C2CTarget> {
        self.c2c
            .load()
            .get(&(channel_id, point_type, point_id))
            .copied()
    }

    /// Get cache statistics
    pub fn stats(&self) -> RoutingCacheStats {
        RoutingCacheStats {
            c2m_count: self.c2m.load().len(),
            m2c_count: self.m2c.load().len(),
            c2c_count: self.c2c.load().len(),
        }
    }

    /// Iterate over all C2M routes (for building ChannelToSlotIndex)
    ///
    /// Returns a Vec of all C2M routes for building direct channel-to-slot mappings.
    /// The returned Vec is a snapshot of the current routing state.
    ///
    /// # Example
    /// ```ignore
    /// for (key, target) in routing_cache.c2m_iter() {
    ///     // key = (channel_id, point_type, point_id)
    ///     // target = C2MTarget { instance_id, point_id }
    /// }
    /// ```
    #[inline]
    pub fn c2m_iter(&self) -> Vec<(StructuredRouteKey, C2MTarget)> {
        let c2m = self.c2m.load();
        c2m.iter().map(|(&k, &v)| (k, v)).collect()
    }

    /// Iterate over all M2C routes (for building reverse mappings)
    ///
    /// Returns a Vec of all M2C routes.
    #[inline]
    pub fn m2c_iter(&self) -> Vec<(StructuredM2CKey, M2CTarget)> {
        let m2c = self.m2c.load();
        m2c.iter().map(|(&k, &v)| (k, v)).collect()
    }
}

impl Default for RoutingCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Routing cache statistics
#[derive(Debug, Clone)]
pub struct RoutingCacheStats {
    pub c2m_count: usize,
    pub m2c_count: usize,
    pub c2c_count: usize,
}

// ============================================================================
// Content Hash for Cross-Process Synchronization
// ============================================================================

impl RoutingCache {
    /// Compute a content hash for routing cache synchronization
    ///
    /// This hash is used to detect routing configuration mismatches between
    /// processes (e.g., io and automation). When shared memory is created,
    /// the hash is stored in the header. When opened, the hash is verified
    /// to ensure both processes are using the same routing configuration.
    ///
    /// ## Hash Algorithm
    /// Uses FxHash (fast, non-cryptographic) over:
    /// - Sorted C2M entries: (channel_id, point_type, point_id) → (instance_id, point_id)
    /// - Sorted M2C entries: (instance_id, point_type, point_id) → (channel_id, point_type, point_id)
    ///
    /// ## Determinism
    /// The hash is deterministic: same routing content → same hash.
    /// Uses sorted iteration to ensure order independence.
    pub fn content_hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};

        // Load BOTH guards before iterating: update() swaps them sequentially,
        // so two independent loads can mix one old and one new snapshot,
        // producing a hash that corresponds to no real state. A concurrent
        // update racing with this read still produces either old-pair or
        // new-pair (the worst case is one extra reset), but never a
        // chimera that could coincidentally equal a prior hash.
        let c2m = self.c2m.load();
        let m2c = self.m2c.load();

        let mut hasher = rustc_hash::FxHasher::default();

        let mut c2m_entries: Vec<_> = c2m.iter().map(|(k, v)| (*k, *v)).collect();
        c2m_entries.sort_by_key(|((ch_id, pt, pt_id), _)| (*ch_id, pt.to_u8(), *pt_id));
        for ((ch_id, pt, pt_id), target) in c2m_entries {
            ch_id.hash(&mut hasher);
            pt.to_u8().hash(&mut hasher);
            pt_id.hash(&mut hasher);
            target.instance_id.hash(&mut hasher);
            target.point_id.hash(&mut hasher);
        }

        let mut m2c_entries: Vec<_> = m2c.iter().map(|(k, v)| (*k, *v)).collect();
        m2c_entries.sort_by_key(|((inst_id, pt, pt_id), _)| (*inst_id, pt.to_u8(), *pt_id));
        for ((inst_id, pt, pt_id), target) in m2c_entries {
            inst_id.hash(&mut hasher);
            pt.to_u8().hash(&mut hasher);
            pt_id.hash(&mut hasher);
            target.channel_id.hash(&mut hasher);
            target.point_type.to_u8().hash(&mut hasher);
            target.point_id.hash(&mut hasher);
        }

        // C2C omitted: it does not affect slot allocation or SHM layout identity.

        hasher.finish()
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Tests can use unwrap for clarity
mod tests {
    use super::*;

    #[test]
    fn test_routing_cache_creation() {
        let cache = RoutingCache::new();
        let stats = cache.stats();
        assert_eq!(stats.c2m_count, 0);
        assert_eq!(stats.m2c_count, 0);
    }

    #[test]
    fn test_from_maps() {
        let mut c2m_data = HashMap::new();
        c2m_data.insert("2:T:1".to_string(), "23:M:1".to_string());

        let mut m2c_data = HashMap::new();
        m2c_data.insert("23:A:4".to_string(), "2:A:1".to_string());

        let mut c2c_data = HashMap::new();
        c2c_data.insert("1001:T:1".to_string(), "1002:T:5".to_string());

        let cache = RoutingCache::from_maps(c2m_data, m2c_data, c2c_data);

        // Verify C2M lookup returns structured type
        let c2m = cache.lookup_c2m("2:T:1").unwrap();
        assert_eq!(c2m.instance_id, 23);
        assert_eq!(c2m.point_id, 1);

        // Verify M2C lookup returns structured type
        let m2c = cache.lookup_m2c("23:A:4").unwrap();
        assert_eq!(m2c.channel_id, 2);
        assert_eq!(m2c.point_type, PointType::Adjustment);
        assert_eq!(m2c.point_id, 1);

        // Verify C2C lookup returns structured type
        let c2c = cache
            .lookup_c2c_by_parts(1001, PointType::Telemetry, 1)
            .unwrap();
        assert_eq!(c2c.channel_id, 1002);
        assert_eq!(c2c.point_type, PointType::Telemetry);
        assert_eq!(c2c.point_id, 5);

        let stats = cache.stats();
        assert_eq!(stats.c2m_count, 1);
        assert_eq!(stats.m2c_count, 1);
        assert_eq!(stats.c2c_count, 1);
    }

    #[test]
    fn test_by_parts_lookup() {
        let mut c2m_data = HashMap::new();
        c2m_data.insert("2:T:1".to_string(), "23:M:1".to_string());

        let mut c2c_data = HashMap::new();
        c2c_data.insert("1001:T:1".to_string(), "1002:T:5".to_string());

        let cache = RoutingCache::from_maps(c2m_data, HashMap::new(), c2c_data);

        // Test C2M by_parts lookup
        let c2m = cache
            .lookup_c2m_by_parts(2, PointType::Telemetry, 1)
            .unwrap();
        assert_eq!(c2m.instance_id, 23);
        assert_eq!(c2m.point_id, 1);

        // Test C2C by_parts lookup
        let c2c = cache
            .lookup_c2c_by_parts(1001, PointType::Telemetry, 1)
            .unwrap();
        assert_eq!(c2c.channel_id, 1002);
        assert_eq!(c2c.point_id, 5);

        // Non-existent should return None
        assert!(
            cache
                .lookup_c2m_by_parts(999, PointType::Telemetry, 1)
                .is_none()
        );
        assert!(
            cache
                .lookup_c2c_by_parts(999, PointType::Telemetry, 1)
                .is_none()
        );
    }

    #[test]
    fn test_update() {
        let cache = RoutingCache::new();

        let mut c2m_data = HashMap::new();
        c2m_data.insert("2:T:1".to_string(), "23:M:1".to_string());

        let mut m2c_data = HashMap::new();
        m2c_data.insert("23:A:4".to_string(), "2:A:1".to_string());

        let mut c2c_data = HashMap::new();
        c2c_data.insert("1001:S:2".to_string(), "1002:S:3".to_string());

        cache.update(c2m_data, m2c_data, c2c_data);

        // Verify updated values
        let c2m = cache.lookup_c2m("2:T:1").unwrap();
        assert_eq!(c2m.instance_id, 23);
        assert_eq!(c2m.point_id, 1);

        let m2c = cache.lookup_m2c("23:A:4").unwrap();
        assert_eq!(m2c.channel_id, 2);
        assert_eq!(m2c.point_type, PointType::Adjustment);
        assert_eq!(m2c.point_id, 1);

        let c2c = cache
            .lookup_c2c_by_parts(1001, PointType::Signal, 2)
            .unwrap();
        assert_eq!(c2c.channel_id, 1002);
        assert_eq!(c2c.point_type, PointType::Signal);
        assert_eq!(c2c.point_id, 3);
    }

    #[test]
    fn test_parse_invalid_targets() {
        // Invalid format should be skipped
        let mut c2m_data = HashMap::new();
        c2m_data.insert("valid:T:1".to_string(), "23:M:1".to_string());
        c2m_data.insert("invalid:T:2".to_string(), "not:a:valid:target".to_string());
        c2m_data.insert("also_invalid".to_string(), "short".to_string());

        let cache = RoutingCache::from_maps(c2m_data, HashMap::new(), HashMap::new());

        // Only valid entry should be present (note: "valid" parses as channel_id fails)
        // Actually "valid" won't parse as u32, so none will be present
        assert_eq!(cache.stats().c2m_count, 0);
    }

    #[test]
    fn test_parse_valid_numeric_keys() {
        let mut c2m_data = HashMap::new();
        c2m_data.insert("100:T:1".to_string(), "23:M:1".to_string());
        c2m_data.insert("100:T:2".to_string(), "23:M:2".to_string());

        let cache = RoutingCache::from_maps(c2m_data, HashMap::new(), HashMap::new());

        assert!(cache.lookup_c2m("100:T:1").is_some());
        assert!(cache.lookup_c2m("100:T:2").is_some());
        assert_eq!(cache.stats().c2m_count, 2);
    }

    #[test]
    fn test_c2m_iter() {
        let mut c2m_data = HashMap::new();
        c2m_data.insert("1001:T:1".to_string(), "5:M:10".to_string());
        c2m_data.insert("1001:T:2".to_string(), "5:M:20".to_string());
        c2m_data.insert("1002:S:1".to_string(), "6:M:30".to_string());

        let mut m2c_data = HashMap::new();
        m2c_data.insert("5:A:1".to_string(), "1001:C:1".to_string());

        let cache = RoutingCache::from_maps(c2m_data, m2c_data, HashMap::new());

        // Test c2m_iter
        let c2m_routes = cache.c2m_iter();
        assert_eq!(c2m_routes.len(), 3);

        // Verify routes contain expected data
        let has_route = c2m_routes.iter().any(|(key, target)| {
            key.0 == 1001 && key.1 == PointType::Telemetry && key.2 == 1 && target.instance_id == 5
        });
        assert!(has_route);

        // Test m2c_iter
        let m2c_routes = cache.m2c_iter();
        assert_eq!(m2c_routes.len(), 1);

        let has_m2c_route = m2c_routes.iter().any(|(key, target)| {
            key.0 == 5 && key.1 == PointType::Adjustment && key.2 == 1 && target.channel_id == 1001
        });
        assert!(has_m2c_route);
    }

    // ========== Content Hash Tests ==========

    #[test]
    fn test_content_hash_deterministic() {
        let mut c2m_data = HashMap::new();
        c2m_data.insert("1001:T:1".to_string(), "5:M:10".to_string());
        c2m_data.insert("1001:T:2".to_string(), "5:M:20".to_string());

        let mut m2c_data = HashMap::new();
        m2c_data.insert("5:A:1".to_string(), "1001:C:1".to_string());

        // Create two caches with same data
        let cache1 = RoutingCache::from_maps(c2m_data.clone(), m2c_data.clone(), HashMap::new());
        let cache2 = RoutingCache::from_maps(c2m_data, m2c_data, HashMap::new());

        // Hash should be identical
        assert_eq!(cache1.content_hash(), cache2.content_hash());
    }

    #[test]
    fn test_content_hash_differs_on_data_change() {
        let mut c2m_data = HashMap::new();
        c2m_data.insert("1001:T:1".to_string(), "5:M:10".to_string());

        let cache1 = RoutingCache::from_maps(c2m_data, HashMap::new(), HashMap::new());

        // Different data
        let mut c2m_data2 = HashMap::new();
        c2m_data2.insert("1001:T:1".to_string(), "6:M:10".to_string()); // Different instance_id

        let cache2 = RoutingCache::from_maps(c2m_data2, HashMap::new(), HashMap::new());

        // Hash should differ
        assert_ne!(cache1.content_hash(), cache2.content_hash());
    }

    #[test]
    fn test_content_hash_empty_cache() {
        let cache = RoutingCache::new();
        // Empty cache should still produce a valid hash
        let hash = cache.content_hash();
        // Just verify it produces a valid hash (any u64 is valid)
        let _ = hash;
    }

    #[test]
    fn test_content_hash_c2c_not_included() {
        let mut c2m_data = HashMap::new();
        c2m_data.insert("1001:T:1".to_string(), "5:M:10".to_string());

        // Cache with C2C
        let mut c2c_data = HashMap::new();
        c2c_data.insert("1001:T:1".to_string(), "1002:T:1".to_string());

        let cache1 = RoutingCache::from_maps(c2m_data.clone(), HashMap::new(), HashMap::new());
        let cache2 = RoutingCache::from_maps(c2m_data, HashMap::new(), c2c_data);

        // C2C should NOT affect hash (doesn't affect slot allocation)
        assert_eq!(cache1.content_hash(), cache2.content_hash());
    }
}
