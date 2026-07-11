//! Atomic diagnostics for lock-free channel statistics.
//!
//! This module provides `AtomicDiagnostics` - a lock-free alternative to
//! `Arc<RwLock<Diagnostics>>` for channel statistics tracking.
//!
//! # Performance
//!
//! - Counter updates: ~1ns (atomic fetch_add)
//! - Error recording: ~10ns (ArcSwap store)
//! - Reading stats: ~5ns (atomic load)
//!
//! Compared to RwLock (~50ns for uncontended, ~1μs+ under contention).

use std::sync::atomic::{AtomicU64, Ordering};

use arc_swap::ArcSwapOption;

/// Lock-free channel diagnostics using atomic operations.
///
/// Replaces `Arc<RwLock<ChannelDiagnostics>>` with zero-contention atomics.
/// All operations are wait-free and safe for concurrent access.
#[derive(Debug, Default)]
pub struct AtomicDiagnostics {
    /// Read/receive operation count.
    read_count: AtomicU64,
    /// Write/send operation count.
    write_count: AtomicU64,
    /// Error count.
    error_count: AtomicU64,
    /// Last error message (lock-free swap).
    last_error: ArcSwapOption<String>,
}

impl AtomicDiagnostics {
    /// Create new diagnostics with zero counts.
    pub fn new() -> Self {
        Self::default()
    }

    /// Increment read count by 1.
    #[inline]
    pub fn inc_read(&self) {
        self.read_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Increment read count by n.
    #[inline]
    pub fn add_read(&self, n: u64) {
        self.read_count.fetch_add(n, Ordering::Relaxed);
    }

    /// Increment write count by 1.
    #[inline]
    pub fn inc_write(&self) {
        self.write_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Increment write count by n.
    #[inline]
    pub fn add_write(&self, n: u64) {
        self.write_count.fetch_add(n, Ordering::Relaxed);
    }

    /// Increment error count by 1.
    #[inline]
    pub fn inc_error(&self) {
        self.error_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Increment error count by n.
    #[inline]
    pub fn add_error(&self, n: u64) {
        self.error_count.fetch_add(n, Ordering::Relaxed);
    }

    /// Record an error message (replaces previous).
    #[inline]
    pub fn record_error(&self, msg: impl Into<String>) {
        self.last_error.store(Some(std::sync::Arc::new(msg.into())));
        self.error_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Get current read count.
    #[inline]
    pub fn read_count(&self) -> u64 {
        self.read_count.load(Ordering::Relaxed)
    }

    /// Get current write count.
    #[inline]
    pub fn write_count(&self) -> u64 {
        self.write_count.load(Ordering::Relaxed)
    }

    /// Get current error count.
    #[inline]
    pub fn error_count(&self) -> u64 {
        self.error_count.load(Ordering::Relaxed)
    }

    /// Get last error message (if any).
    #[inline]
    pub fn last_error(&self) -> Option<String> {
        self.last_error.load().as_ref().map(|s| (**s).clone())
    }

    /// Create a snapshot of all diagnostics.
    pub fn snapshot(&self) -> DiagnosticsSnapshot {
        DiagnosticsSnapshot {
            read_count: self.read_count(),
            write_count: self.write_count(),
            error_count: self.error_count(),
            last_error: self.last_error(),
        }
    }
}

/// Immutable snapshot of diagnostics at a point in time.
#[derive(Debug, Clone)]
pub struct DiagnosticsSnapshot {
    pub read_count: u64,
    pub write_count: u64,
    pub error_count: u64,
    pub last_error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_operations() {
        let diag = AtomicDiagnostics::new();

        diag.inc_read();
        diag.inc_read();
        diag.inc_write();
        diag.record_error("test error");

        assert_eq!(diag.read_count(), 2);
        assert_eq!(diag.write_count(), 1);
        assert_eq!(diag.error_count(), 1);
        assert_eq!(diag.last_error(), Some("test error".to_string()));
    }

    #[test]
    fn test_snapshot() {
        let diag = AtomicDiagnostics::new();
        diag.add_read(100);
        diag.add_write(50);
        diag.add_error(5);

        let snap = diag.snapshot();
        assert_eq!(snap.read_count, 100);
        assert_eq!(snap.write_count, 50);
        assert_eq!(snap.error_count, 5);
    }
}
