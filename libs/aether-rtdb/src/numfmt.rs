//! High-performance number formatting utilities
//!
//! Provides zero-allocation number-to-string conversion for hot paths.
//! Uses `itoa` for integers and `ryu` for floats, avoiding heap allocations.
//!
//! # Performance
//!
//! | Method | Time | Allocations |
//! |--------|------|-------------|
//! | `u32::to_string()` | ~30ns | 1 heap |
//! | `itoa::Buffer` | ~8ns | 0 (stack) |
//! | `f64::to_string()` | ~50ns | 1 heap |
//! | `ryu::Buffer` | ~15ns | 0 (stack) |
//!
//! # Usage
//!
//! ```rust
//! use aether_rtdb::numfmt::{u32_to_bytes, f64_to_bytes, PointIdCache};
//!
//! // Single conversion (stack buffer, no heap)
//! let bytes = u32_to_bytes(12345);  // Bytes from stack buffer
//!
//! // Cached point IDs (for batch operations)
//! let mut cache = PointIdCache::new();
//! let arc_str = cache.get(42);  // Returns Arc<str>, O(1) clone
//! ```

use bytes::Bytes;
use std::sync::Arc;

/// Convert u32 to Bytes using stack buffer (zero heap allocation during formatting)
///
/// The resulting Bytes owns a copy of the formatted string.
/// Use this for one-off conversions in hot paths.
#[inline]
pub fn u32_to_bytes(n: u32) -> Bytes {
    let mut buffer = itoa::Buffer::new();
    let s = buffer.format(n);
    Bytes::copy_from_slice(s.as_bytes())
}

/// Convert i64 to Bytes using stack buffer
#[inline]
pub fn i64_to_bytes(n: i64) -> Bytes {
    let mut buffer = itoa::Buffer::new();
    let s = buffer.format(n);
    Bytes::copy_from_slice(s.as_bytes())
}

/// Convert f64 to Bytes using stack buffer (zero heap allocation during formatting)
///
/// Uses `ryu` for fast, accurate floating-point formatting.
#[inline]
pub fn f64_to_bytes(n: f64) -> Bytes {
    let mut buffer = ryu::Buffer::new();
    let s = buffer.format(n);
    Bytes::copy_from_slice(s.as_bytes())
}

/// Convert u32 to `Arc<str>` (single allocation, O(1) clone)
///
/// Use this when the same point_id will be used multiple times (e.g., 3-layer writes).
#[inline]
pub fn u32_to_arc_str(n: u32) -> Arc<str> {
    let mut buffer = itoa::Buffer::new();
    Arc::from(buffer.format(n))
}

/// Thread-local point ID string cache
///
/// Caches `Arc<str>` representations of point IDs for batch operations.
/// Using thread-local storage avoids synchronization overhead.
///
/// # Design
///
/// Point IDs in typical industrial control systems are usually:
/// - Contiguous (0-255, 0-1000)
/// - Reused across many writes
///
/// This cache uses a simple Vec for O(1) lookup when point_id < capacity,
/// with fallback to fresh allocation for larger IDs.
///
/// # Example
///
/// ```rust
/// use aether_rtdb::numfmt::PointIdCache;
/// use std::sync::Arc;
///
/// let mut cache = PointIdCache::new();
///
/// // First call: allocates and caches
/// let s1 = cache.get(42);
///
/// // Subsequent calls: returns cached Arc (O(1) clone)
/// let s2 = cache.get(42);
/// assert!(Arc::ptr_eq(&s1, &s2));
/// ```
pub struct PointIdCache {
    /// Pre-allocated cache for common point IDs (0-1023)
    /// Index is point_id, value is `Option<Arc<str>>`
    cache: Vec<Option<Arc<str>>>,
}

impl PointIdCache {
    /// Default capacity for point ID cache (covers most industrial scenarios)
    const DEFAULT_CAPACITY: usize = 1024;

    /// Create a new point ID cache
    pub fn new() -> Self {
        Self {
            cache: vec![None; Self::DEFAULT_CAPACITY],
        }
    }

    /// Create with custom capacity
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            cache: vec![None; capacity],
        }
    }

    /// Get or create `Arc<str>` for point ID
    ///
    /// Returns cached value if available, otherwise creates and caches.
    /// For IDs >= capacity, creates fresh Arc without caching.
    #[inline]
    pub fn get(&mut self, point_id: u32) -> Arc<str> {
        let idx = point_id as usize;
        if idx < self.cache.len() {
            if let Some(ref cached) = self.cache[idx] {
                return Arc::clone(cached);
            }
            let arc = u32_to_arc_str(point_id);
            self.cache[idx] = Some(Arc::clone(&arc));
            arc
        } else {
            // ID exceeds cache capacity - allocate without caching
            u32_to_arc_str(point_id)
        }
    }

    /// Get cached value without allocation (returns None if not cached)
    #[inline]
    pub fn get_cached(&self, point_id: u32) -> Option<Arc<str>> {
        let idx = point_id as usize;
        if idx < self.cache.len() {
            self.cache[idx].as_ref().map(Arc::clone)
        } else {
            None
        }
    }

    /// Clear all cached values
    pub fn clear(&mut self) {
        self.cache.iter_mut().for_each(|v| *v = None);
    }
}

impl Default for PointIdCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Global point ID string pool for common IDs (0-255)
///
/// Pre-computed at initialization time, zero-cost access thereafter.
/// These cover the vast majority of point IDs in typical installations.
pub mod precomputed {
    use super::*;
    use std::sync::LazyLock;

    /// Pre-computed `Arc<str>` for point IDs 0-255
    static POINT_ID_POOL: LazyLock<[Arc<str>; 256]> = LazyLock::new(|| {
        std::array::from_fn(|i| {
            let mut buffer = itoa::Buffer::new();
            Arc::from(buffer.format(i))
        })
    });

    /// Get pre-computed `Arc<str>` for point ID (0-255 only)
    ///
    /// Returns None for IDs >= 256.
    #[inline]
    pub fn get_point_id_str(point_id: u32) -> Option<Arc<str>> {
        if point_id < 256 {
            Some(Arc::clone(&POINT_ID_POOL[point_id as usize]))
        } else {
            None
        }
    }

    /// Get `Arc<str>` for point ID with fallback to dynamic allocation
    ///
    /// Uses pre-computed pool for 0-255, allocates for larger IDs.
    #[inline]
    pub fn get_point_id_str_or_alloc(point_id: u32) -> Arc<str> {
        if point_id < 256 {
            Arc::clone(&POINT_ID_POOL[point_id as usize])
        } else {
            u32_to_arc_str(point_id)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_u32_to_bytes() {
        assert_eq!(&u32_to_bytes(0)[..], b"0");
        assert_eq!(&u32_to_bytes(42)[..], b"42");
        assert_eq!(&u32_to_bytes(12345)[..], b"12345");
        assert_eq!(&u32_to_bytes(u32::MAX)[..], b"4294967295");
    }

    #[test]
    fn test_i64_to_bytes() {
        assert_eq!(&i64_to_bytes(0)[..], b"0");
        assert_eq!(&i64_to_bytes(-42)[..], b"-42");
        assert_eq!(&i64_to_bytes(1234567890123)[..], b"1234567890123");
    }

    #[test]
    fn test_f64_to_bytes() {
        assert_eq!(&f64_to_bytes(0.0)[..], b"0.0");
        assert_eq!(&f64_to_bytes(42.5)[..], b"42.5");
        assert_eq!(&f64_to_bytes(-123.456)[..], b"-123.456");
    }

    #[test]
    fn test_u32_to_arc_str() {
        let s = u32_to_arc_str(42);
        assert_eq!(&*s, "42");
    }

    #[test]
    fn test_point_id_cache() {
        let mut cache = PointIdCache::new();

        // First access - creates and caches
        let s1 = cache.get(42);
        assert_eq!(&*s1, "42");

        // Second access - returns cached
        let s2 = cache.get(42);
        assert!(Arc::ptr_eq(&s1, &s2));

        // Different ID
        let s3 = cache.get(100);
        assert_eq!(&*s3, "100");
        assert!(!Arc::ptr_eq(&s1, &s3));

        // ID beyond capacity (still works, just not cached)
        let s4 = cache.get(10000);
        assert_eq!(&*s4, "10000");
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // unwrap() is acceptable in tests
    fn test_precomputed_pool() {
        // Within pool
        let s1 = precomputed::get_point_id_str(42).unwrap();
        let s2 = precomputed::get_point_id_str(42).unwrap();
        assert!(Arc::ptr_eq(&s1, &s2));
        assert_eq!(&*s1, "42");

        // Outside pool
        assert!(precomputed::get_point_id_str(256).is_none());

        // Fallback allocation
        let s3 = precomputed::get_point_id_str_or_alloc(1000);
        assert_eq!(&*s3, "1000");
    }
}
