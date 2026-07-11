# Seqlock Zero-Cost Optimization Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Eliminate all `mfence`/`lock` instructions from PointSlot seqlock hot path, achieving ~60ns end-to-end latency on x86.

**Architecture:** Split `flags: AtomicU64` (packed seq+dirty) into `seq: AtomicU32` + `dirty: AtomicU32`. Replace `fetch_add`/`fence(SeqCst)` with `store(Release)`/`fence(Release)` — on x86 these compile to plain `mov` with zero hardware barrier overhead. On ARM64 they compile to `stlr`/`dmb ish` (still correct, slightly faster than current).

**Tech Stack:** Rust `std::sync::atomic`, `#[repr(C, align(32))]`

**Key Invariant:** Single-writer assumption per PointSlot — comsrv writes telemetry slots, modsrv writes control slots. Never concurrent.

---

## Task 1: Write failing tests for split seq/dirty fields

**Files:**
- Modify: `libs/aether-rtdb-shm/tests/test_seqlock_dirty.rs`

**Step 1: Write tests that validate split-field behavior**

Replace the entire file with tests that exercise `seq_raw()` (new accessor) instead of `flags_raw()`:

```rust
//! Tests for seqlock with split seq/dirty fields.
//!
//! Validates that seq counter and dirty flag are independent
//! (no shared AtomicU64, no fetch_add/fetch_or interference).

#[test]
fn test_seq_increments_by_2_per_write() {
    use aether_rtdb_shm::PointSlot;

    let slot = PointSlot::new();
    assert_eq!(slot.seq_raw(), 0);

    slot.set(1.0, 1.0, 100);
    assert_eq!(slot.seq_raw(), 2, "1 write = seq 2");

    slot.set(2.0, 2.0, 200);
    assert_eq!(slot.seq_raw(), 4, "2 writes = seq 4");
}

#[test]
fn test_seq_always_even_after_write() {
    use aether_rtdb_shm::PointSlot;

    let slot = PointSlot::new();

    for i in 0..100u64 {
        slot.set(i as f64, i as f64 * 10.0, 1000 + i);
        let seq = slot.seq_raw();
        assert_eq!(seq % 2, 0, "seq should be even after write #{i}, got {seq}");
    }
}

#[test]
fn test_dirty_independent_of_seq() {
    use aether_rtdb_shm::PointSlot;

    let slot = PointSlot::new();
    assert!(!slot.is_dirty());

    slot.set(1.0, 1.0, 100);
    assert!(slot.is_dirty());
    assert_eq!(slot.seq_raw(), 2);

    // clear_dirty must NOT affect seq
    slot.clear_dirty();
    assert!(!slot.is_dirty());
    assert_eq!(slot.seq_raw(), 2, "clear_dirty must not change seq");
}

#[test]
fn test_consistent_read_after_split() {
    use aether_rtdb_shm::PointSlot;

    let slot = PointSlot::new();

    for i in 0..50u64 {
        slot.set(i as f64, i as f64 * 2.0, 1000 + i);
    }

    let (value, raw, ts) = slot.load_consistent();
    assert!((value - 49.0).abs() < 0.001);
    assert!((raw - 98.0).abs() < 0.001);
    assert_eq!(ts, 1049);
    assert_eq!(slot.seq_raw(), 100); // 50 writes × 2
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p aether-rtdb-shm --test test_seqlock_dirty 2>&1`
Expected: FAIL — `seq_raw` method does not exist yet.

**Step 3: Commit**

```bash
git add libs/aether-rtdb-shm/tests/test_seqlock_dirty.rs
git commit -m "test(shm): RED — seqlock tests for split seq/dirty fields"
```

---

## Task 2: Split `flags` into `seq` + `dirty` in PointSlot struct

**Files:**
- Modify: `libs/aether-rtdb-shm/src/vec_impl.rs:66-103`

**Step 1: Replace the struct definition and constructor**

Replace `PointSlot` struct, constants, and `new()`:

```rust
/// Single-point shared memory slot with seqlock consistency.
///
/// Layout (32 bytes total, 32-byte aligned):
/// ```text
/// offset 0:  value_bits  (AtomicU64)   8 bytes
/// offset 8:  timestamp   (AtomicU64)   8 bytes
/// offset 16: raw_bits    (AtomicU64)   8 bytes
/// offset 24: seq         (AtomicU32)   4 bytes — seqlock counter
/// offset 28: dirty       (AtomicU32)   4 bytes — dirty flag
/// ```
///
/// # Safety (shared memory usage)
///
/// This struct is `#[repr(C)]` to guarantee a deterministic field layout
/// when cast from raw pointers in `unified_shm.rs`. The compile-time
/// assertion below ensures the size never drifts from expectations.
#[repr(C, align(32))]
pub struct PointSlot {
    /// Engineering value (IEEE 754 double as bits)
    value_bits: AtomicU64,
    /// Timestamp in milliseconds
    timestamp: AtomicU64,
    /// Raw value (as bits)
    raw_bits: AtomicU64,
    /// Seqlock sequence counter (odd = write in progress, even = idle)
    seq: AtomicU32,
    /// Dirty flag (1 = modified since last flush)
    dirty: AtomicU32,
}

const _: () = assert!(std::mem::size_of::<PointSlot>() == 32);
```

Remove the old `SEQ_INCREMENT` constant and the `SEQ_INCREMENT > u32::MAX` compile-time assert — they are no longer needed.

**Step 2: Update `new()` and `Default`**

```rust
impl Default for PointSlot {
    fn default() -> Self {
        Self::new()
    }
}

impl PointSlot {
    /// Create a new empty point slot
    pub const fn new() -> Self {
        Self {
            value_bits: AtomicU64::new(0),
            timestamp: AtomicU64::new(0),
            raw_bits: AtomicU64::new(0),
            seq: AtomicU32::new(0),
            dirty: AtomicU32::new(0),
        }
    }
```

**Step 3: Add `AtomicU32` to the import line**

The existing import is:
```rust
use std::sync::atomic::{fence, AtomicU64, Ordering};
```

Change to:
```rust
use std::sync::atomic::{fence, AtomicU32, AtomicU64, Ordering};
```

**Step 4: Verify compilation (expect test failures, not build failures)**

Run: `cargo check -p aether-rtdb-shm 2>&1`
Expected: compile errors in `set()`, `is_dirty()`, `clear_dirty()`, `flags_raw()`, `try_load_consistent()`, and internal tests that reference `self.flags` — this is expected, we fix them in the next tasks.

---

## Task 3: Rewrite `set()` with zero-cost seqlock

**Files:**
- Modify: `libs/aether-rtdb-shm/src/vec_impl.rs` — the `set()` method (currently lines ~248-277)

**Step 1: Replace `set()` implementation**

```rust
    /// Write all point data atomically via seqlock protocol.
    ///
    /// # Seqlock write protocol (zero-cost on x86)
    ///
    /// 1. `store(seq+1, Relaxed)` — mark write-in-progress (odd)
    /// 2. `fence(Release)` — compiler barrier (x86: no instruction; ARM64: `dmb ish`)
    /// 3. Data stores (Relaxed) — value, raw, timestamp
    /// 4. `store(seq+2, Release)` — mark write-complete (even)
    ///    (x86: plain `mov`; ARM64: `stlr`)
    /// 5. `store(dirty=1, Relaxed)` — advisory flag, outside seqlock envelope
    ///
    /// On x86 TSO all stores are plain `mov`. Total: ~4ns writer overhead
    /// plus ~50ns cross-core cache-line transfer.
    ///
    /// # Single-writer assumption
    ///
    /// Uses `load` + `store` instead of `fetch_add` for the seq counter.
    /// Correct ONLY when a single thread writes to each slot (guaranteed
    /// by SCADA architecture: comsrv owns telemetry slots, modsrv owns
    /// control slots).
    #[inline]
    pub fn set(&self, value: f64, raw: f64, timestamp: u64) {
        let s = self.seq.load(Ordering::Relaxed);

        // Begin write: seq → odd (signals write-in-progress)
        self.seq.store(s.wrapping_add(1), Ordering::Relaxed);

        // BARRIER: ensures odd seq is visible before data stores.
        // x86: compiler barrier only (0 instructions). ARM64: dmb ish.
        fence(Ordering::Release);

        // Data stores — Relaxed; ordering enforced by surrounding barriers.
        self.value_bits.store(value.to_bits(), Ordering::Relaxed);
        self.raw_bits.store(raw.to_bits(), Ordering::Relaxed);
        self.timestamp.store(timestamp, Ordering::Relaxed);

        // End write: seq → even (signals write-complete).
        // Release prevents data stores from being reordered past this store.
        // x86: plain mov. ARM64: stlr.
        self.seq.store(s.wrapping_add(2), Ordering::Release);

        // Dirty flag — outside seqlock envelope (advisory, not consistency-critical).
        self.dirty.store(1, Ordering::Relaxed);
    }
```

**Step 2: Rewrite `is_dirty`, `clear_dirty`, and rename `flags_raw` → `seq_raw`**

```rust
    /// Check if dirty flag is set
    #[inline]
    pub fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::Relaxed) != 0
    }

    /// Clear the dirty flag
    #[inline]
    pub fn clear_dirty(&self) {
        self.dirty.store(0, Ordering::Relaxed);
    }

    /// Get raw seq counter value (for testing/debugging)
    #[inline]
    pub fn seq_raw(&self) -> u32 {
        self.seq.load(Ordering::Relaxed)
    }
```

---

## Task 4: Rewrite reader-side seqlock (`try_load_consistent`, `load_relaxed`)

**Files:**
- Modify: `libs/aether-rtdb-shm/src/vec_impl.rs` — `try_load_consistent()` (lines ~189-212) and `load_relaxed()` (lines ~220-225)

**Step 1: Replace `try_load_consistent()`**

```rust
    /// Single-attempt seqlock read. Returns `None` on contention.
    ///
    /// ## Algorithm
    ///
    /// 1. `load(seq, Relaxed)` — read sequence counter
    /// 2. If odd → write in progress → return None
    /// 3. `fence(Acquire)` — ensures subsequent data loads see the writer's stores
    ///    (x86: compiler barrier only; ARM64: `dmb ish`)
    /// 4. Read value, raw, timestamp (Relaxed)
    /// 5. `fence(Acquire)` — ensures data loads complete before re-reading seq
    /// 6. Re-read seq — if unchanged, data is consistent
    ///
    /// Pairs with writer's `fence(Release)` + `store(seq, Release)`.
    #[inline]
    pub fn try_load_consistent(&self) -> Option<(f64, f64, u64)> {
        let seq1 = self.seq.load(Ordering::Relaxed);

        if seq1 & 1 != 0 {
            return None; // write in progress
        }

        // ACQUIRE: ensures data loads below see the writer's stores
        // that happened before the writer's Release store of this seq value.
        // x86: compiler barrier only. ARM64: dmb ish.
        fence(Ordering::Acquire);

        let value = f64::from_bits(self.value_bits.load(Ordering::Relaxed));
        let raw = f64::from_bits(self.raw_bits.load(Ordering::Relaxed));
        let ts = self.timestamp.load(Ordering::Relaxed);

        // ACQUIRE: ensures all data loads above complete before re-reading seq.
        fence(Ordering::Acquire);

        let seq2 = self.seq.load(Ordering::Relaxed);

        if seq1 == seq2 {
            Some((value, raw, ts))
        } else {
            None
        }
    }
```

**Step 2: Replace `load_relaxed()`**

```rust
    /// Unprotected read of all fields (no seqlock guarantee).
    ///
    /// Used only as a last-resort fallback after retries are exhausted.
    /// A leading `fence(Acquire)` provides best-effort freshness.
    #[inline]
    fn load_relaxed(&self) -> (f64, f64, u64) {
        fence(Ordering::Acquire);
        let value = f64::from_bits(self.value_bits.load(Ordering::Relaxed));
        let raw = f64::from_bits(self.raw_bits.load(Ordering::Relaxed));
        let ts = self.timestamp.load(Ordering::Relaxed);
        (value, raw, ts)
    }
```

**Step 3: Update `load_consistent()` doc comment**

Change the doc comment reference from `fence(SeqCst)` to `fence(Acquire)` — the method body (`try_load_consistent` loop) doesn't change, only the internal implementation of `try_load_consistent` changed.

---

## Task 5: Fix internal tests in `vec_impl.rs`

**Files:**
- Modify: `libs/aether-rtdb-shm/src/vec_impl.rs` — tests module

**Step 1: Fix `test_seqlock_sequence_counter_values` (line ~976-989)**

Replace `slot.flags.load(Ordering::Relaxed) >> 32` with `slot.seq_raw()`:

```rust
    #[test]
    fn test_seqlock_sequence_counter_values() {
        let slot = PointSlot::new();

        assert_eq!(slot.seq_raw(), 0, "initial sequence should be 0");
        assert_eq!(slot.seq_raw() & 1, 0, "initial sequence should be even");

        slot.set(1.0, 1.0, 100);
        assert_eq!(slot.seq_raw(), 2, "after 1 write, sequence should be 2");

        slot.set(2.0, 2.0, 200);
        assert_eq!(slot.seq_raw(), 4, "after 2 writes, sequence should be 4");
    }
```

**Step 2: Run all tests**

Run: `cargo test -p aether-rtdb-shm 2>&1`
Expected: all tests pass, including the new `test_seqlock_dirty.rs` tests.

**Step 3: Run clippy**

Run: `cargo clippy -p aether-rtdb-shm -- -D warnings 2>&1`
Expected: clean.

**Step 4: Commit**

```bash
git add libs/aether-rtdb-shm/src/vec_impl.rs libs/aether-rtdb-shm/tests/test_seqlock_dirty.rs
git commit -m "perf(shm): zero-cost seqlock — split seq/dirty, eliminate mfence+lock on x86"
```

---

## Task 6: Update snapshot restore byte offsets

**Files:**
- Modify: `libs/aether-rtdb-shm/src/unified_shm.rs` — snapshot restore (lines ~836-848)

**Step 1: Update the PointSlot layout comment**

The snapshot restore reads PointSlot fields as raw bytes. The layout comment must reflect the new field structure. The data fields (offsets 0-24) are unchanged — only the comment about offset 24 changes:

```rust
            // PointSlot layout (#[repr(C, align(32))]):
            //   offset 0:  value_bits (AtomicU64 → read as u64)
            //   offset 8:  timestamp  (AtomicU64 → read as u64)
            //   offset 16: raw_bits   (AtomicU64 → read as u64)
            //   offset 24: seq        (AtomicU32 → not needed for restore)
            //   offset 28: dirty      (AtomicU32 → not needed for restore)
```

The actual byte-read code (`sb[0..8]`, `sb[8..16]`, `sb[16..24]`) does NOT change — it only reads the data fields, not seq/dirty.

**Step 2: Run full workspace check**

Run: `cargo clippy --workspace -- -D warnings 2>&1`
Expected: clean.

Run: `cargo test -p aether-rtdb-shm -p modsrv 2>&1`
Expected: all pass.

**Step 3: Commit**

```bash
git add libs/aether-rtdb-shm/src/unified_shm.rs
git commit -m "docs(shm): update PointSlot layout comments for split seq/dirty"
```

---

## Verification Checklist

After all tasks:

- [ ] `cargo test -p aether-rtdb-shm` — all seqlock tests pass
- [ ] `cargo test -p modsrv` — modsrv tests pass
- [ ] `cargo clippy --workspace -- -D warnings` — zero warnings
- [ ] Confirm on x86: `set()` contains no `mfence` or `lock` prefix instructions

Optional (if `cargo-asm` is installed):
```bash
cargo asm -p aether-rtdb-shm 'aether_rtdb_shm::vec_impl::PointSlot::set' --simplify
```
Expected: only `mov` instructions, no `mfence`, no `lock xadd`, no `lock or`.
