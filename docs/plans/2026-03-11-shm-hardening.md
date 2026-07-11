# SHM Hardening Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Fix all Critical/High severity shared memory bugs discovered during the 4-agent SHM audit — eliminate UB, harden ARM64 memory ordering, improve error recovery, and optimize batch writes.

**Architecture:** Targeted fixes to `aether-rtdb-shm` crate (vec_impl, ring_buffer, unified_shm) and `modsrv/shm_dispatch.rs`. All changes are backwards-compatible at the SHM file format level (header magic/version unchanged). TDD approach: write failing test first, then fix.

**Tech Stack:** Rust atomics, `memmap2`, `std::sync::atomic`, `tempfile` (tests)

---

## Fix Summary

| ID | Severity | File | Issue |
|----|----------|------|-------|
| C1 | Critical | unified_shm.rs | `read_unaligned` on types containing `AtomicU64` is UB |
| C2 | Critical | unified_shm.rs | `reconfigure_existing` lacks fence between fill(0) and header stores |
| H1 | High | ring_buffer.rs | `push()` head uses `Relaxed` before `write_volatile` on ARM64 |
| H2 | High | shm_dispatch.rs | `rebuild_writer` failure silently clears writer → dispatch disabled |
| H3 | High | unified_shm.rs | No `mmap.flush()` after reconfigure → cross-process visibility gap |
| H4 | High | vec_impl.rs | Dirty flag `fetch_or(1)` after seqlock even-write causes false retries |
| H5 | High | ring_buffer.rs | `open_readonly` uses `MmapMut` (read+write) instead of `Mmap` |
| H6 | High | ring_buffer.rs | `push_batch` is serial loop — no batch head reservation |

## Execution Groups

- **Group A** (Task 1-5): `vec_impl.rs` — H4 dirty flag separation
- **Group B** (Task 6-17): `ring_buffer.rs` — H1 ordering, H5 readonly, H6 batch
- **Group C** (Task 18-29): `unified_shm.rs` — C1 read_unaligned, C2 fence, H3 flush
- **Group D** (Task 30-35): `shm_dispatch.rs` — H2 error propagation

---

### Task 1: Write failing test for H4 — dirty flag causes seqlock reader false retry

**Files:**
- Test: `libs/aether-rtdb-shm/tests/test_seqlock_dirty.rs`

**Step 1: Write the failing test**

```rust
//! Tests that dirty flag does NOT interfere with seqlock consistency.
//!
//! H4: fetch_or(1, Relaxed) after the seqlock even-write creates a window
//! where a reader sees an odd sequence (dirty OR'd into seq bits), triggering
//! a spurious retry.

use std::sync::atomic::Ordering;

#[test]
fn test_dirty_flag_does_not_corrupt_seqlock_sequence() {
    use aether_rtdb_shm::PointSlot;

    let slot = PointSlot::new();

    // Write a value (this sets dirty + advances seqlock)
    slot.set(42.0, 420.0, 1000);

    // After set(), the sequence counter (upper 32 bits of flags) should be EVEN,
    // indicating write-complete. The dirty flag (bit 0) should NOT make the
    // sequence appear odd to a reader checking `flags >> 32`.
    //
    // Current bug: fetch_or(1) after even increment can be reordered to appear
    // as if seq is odd on a reader's timeline.
    let flags = slot.flags_raw();
    let seq = flags >> 32;
    assert_eq!(seq % 2, 0, "Sequence should be even after write, got seq={}", seq);

    // The dirty bit should be separate and not affect sequence
    assert!(slot.is_dirty(), "Dirty flag should be set after write");

    // Do 100 writes and verify sequence stays even after each
    for i in 0..100 {
        slot.set(i as f64, i as f64 * 10.0, 2000 + i);
        let flags = slot.flags_raw();
        let seq = flags >> 32;
        assert_eq!(
            seq % 2, 0,
            "Sequence should be even after write #{}, got seq={}",
            i, seq
        );
    }
}

#[test]
fn test_dirty_flag_independent_of_seqlock() {
    use aether_rtdb_shm::PointSlot;

    let slot = PointSlot::new();

    // Initially: not dirty, seq=0
    assert!(!slot.is_dirty());

    // Write sets dirty
    slot.set(1.0, 1.0, 100);
    assert!(slot.is_dirty());

    // Clear dirty — should NOT affect seqlock
    slot.clear_dirty();
    assert!(!slot.is_dirty());

    // Sequence should still be 2 (one write = +2 increments)
    let flags = slot.flags_raw();
    let seq = flags >> 32;
    assert_eq!(seq, 2, "Seq should be 2 after one write cycle");
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p aether-rtdb-shm --test test_seqlock_dirty -- --nocapture 2>&1 | head -30`
Expected: FAIL — `flags_raw()` method doesn't exist yet (compilation error)

**Step 3: Commit test (RED phase)**

```bash
git add libs/aether-rtdb-shm/tests/test_seqlock_dirty.rs
git commit -m "test(rtdb-shm): add failing tests for H4 dirty flag seqlock interference"
```

---

### Task 2: Add `flags_raw()` accessor to PointSlot

**Files:**
- Modify: `libs/aether-rtdb-shm/src/vec_impl.rs` (after `clear_dirty` at line ~285)

**Step 1: Add the accessor**

Add after the `clear_dirty` method (line 285):

```rust
    /// Get raw flags value (for testing/debugging seqlock state)
    #[inline]
    pub fn flags_raw(&self) -> u64 {
        self.flags.load(Ordering::Relaxed)
    }
```

**Step 2: Run test to verify it compiles but test logic passes**

Run: `cargo test -p aether-rtdb-shm --test test_seqlock_dirty -- --nocapture 2>&1 | head -30`
Expected: Tests PASS (the current single-threaded test won't reliably catch the reordering, but the API is ready)

**Step 3: Commit**

```bash
git add libs/aether-rtdb-shm/src/vec_impl.rs
git commit -m "feat(rtdb-shm): add flags_raw() accessor for seqlock diagnostics"
```

---

### Task 3: Fix H4 — move dirty flag inside seqlock fence envelope

**Files:**
- Modify: `libs/aether-rtdb-shm/src/vec_impl.rs:246-273`

**Step 1: Apply the fix**

Replace the `set()` method body (lines 246-273):

```rust
    #[inline]
    pub fn set(&self, value: f64, raw: f64, timestamp: u64) {
        // Begin write: sequence → odd (signals write-in-progress)
        // Relaxed: ordering enforced by the fence below, not by this RMW.
        self.flags.fetch_add(SEQ_INCREMENT, Ordering::Relaxed);

        // FULL BARRIER (dmb ish on ARM, mfence on x86):
        // Ensures the odd sequence is globally visible to ALL cores
        // before any data store. Prevents Store→Store reordering across
        // different addresses (flags vs value_bits/raw_bits/timestamp).
        fence(Ordering::SeqCst);

        // Data stores — Relaxed because ordering is fence-enclosed.
        self.value_bits.store(value.to_bits(), Ordering::Relaxed);
        self.raw_bits.store(raw.to_bits(), Ordering::Relaxed);
        self.timestamp.store(timestamp, Ordering::Relaxed);

        // Dirty flag — INSIDE the fence envelope so it's part of the
        // atomic write group, not a separate operation after seqlock close.
        // Bit 0 only; upper 32 bits (seq counter) are untouched by fetch_or.
        self.flags.fetch_or(1, Ordering::Relaxed);

        // FULL BARRIER:
        // Ensures ALL data stores + dirty flag are globally visible before
        // the even sequence is published.
        fence(Ordering::SeqCst);

        // End write: sequence → even (signals write-complete)
        self.flags.fetch_add(SEQ_INCREMENT, Ordering::Relaxed);
    }
```

Key change: `fetch_or(1)` moved from **after** the closing `fetch_add` to **between** the two fences. This guarantees the dirty flag is visible atomically with the data, and the final sequence increment is clean (no trailing `fetch_or` that could appear as a torn state to readers).

**Step 2: Run tests**

Run: `cargo test -p aether-rtdb-shm -- --nocapture 2>&1 | tail -20`
Expected: ALL PASS

**Step 3: Commit**

```bash
git add libs/aether-rtdb-shm/src/vec_impl.rs
git commit -m "fix(rtdb-shm): H4 move dirty flag inside seqlock fence envelope"
```

---

### Task 4: Write test verifying seqlock read consistency after H4 fix

**Files:**
- Modify: `libs/aether-rtdb-shm/tests/test_seqlock_dirty.rs`

**Step 1: Add consistency test**

Append to the test file:

```rust
#[test]
fn test_seqlock_read_returns_consistent_data() {
    use aether_rtdb_shm::PointSlot;

    let slot = PointSlot::new();

    // Write multiple times with distinct values
    for i in 0..50u64 {
        slot.set(i as f64, i as f64 * 2.0, 1000 + i);
    }

    // Read should return the last written value consistently
    let (value, raw, ts) = slot.get_with_raw();
    assert!((value - 49.0).abs() < 0.001);
    assert!((raw - 98.0).abs() < 0.001);
    assert_eq!(ts, 1049);

    // Sequence should be 100 (50 writes × 2 increments each)
    let flags = slot.flags_raw();
    let seq = flags >> 32;
    assert_eq!(seq, 100);
}
```

**Step 2: Run test**

Run: `cargo test -p aether-rtdb-shm --test test_seqlock_dirty -- --nocapture`
Expected: PASS (may need `get_with_raw()` — if it doesn't exist, add it in Task 5)

**Step 3: Commit**

```bash
git add libs/aether-rtdb-shm/tests/test_seqlock_dirty.rs
git commit -m "test(rtdb-shm): add seqlock read consistency test"
```

---

### Task 5: Add `get_with_raw()` if needed and verify Group A complete

**Files:**
- Modify: `libs/aether-rtdb-shm/src/vec_impl.rs` (if `get_with_raw` doesn't exist)

**Step 1: Check if method exists**

Search for `get_with_raw` in `vec_impl.rs`. If it doesn't exist, add after the existing `get()` method:

```rust
    /// Read value, raw, and timestamp with seqlock protection
    #[inline]
    pub fn get_with_raw(&self) -> (f64, f64, u64) {
        loop {
            let flags1 = self.flags.load(Ordering::Relaxed);
            let seq1 = flags1 >> 32;
            if seq1 % 2 != 0 {
                std::hint::spin_loop();
                continue;
            }
            fence(Ordering::SeqCst);

            let value = f64::from_bits(self.value_bits.load(Ordering::Relaxed));
            let raw = f64::from_bits(self.raw_bits.load(Ordering::Relaxed));
            let ts = self.timestamp.load(Ordering::Relaxed);

            fence(Ordering::SeqCst);
            let flags2 = self.flags.load(Ordering::Relaxed);
            let seq2 = flags2 >> 32;

            if seq1 == seq2 {
                return (value, raw, ts);
            }
            std::hint::spin_loop();
        }
    }
```

**Step 2: Run full test suite for Group A**

Run: `cargo test -p aether-rtdb-shm -- --nocapture 2>&1 | tail -20`
Expected: ALL PASS

**Step 3: Commit**

```bash
git add libs/aether-rtdb-shm/src/vec_impl.rs libs/aether-rtdb-shm/tests/test_seqlock_dirty.rs
git commit -m "feat(rtdb-shm): add get_with_raw() seqlock reader — Group A complete"
```

---

### Task 6: Write failing test for H1 — RingBuffer push ordering on ARM64

**Files:**
- Test: `libs/aether-rtdb-shm/tests/test_ring_buffer_ordering.rs`

**Step 1: Write the failing test**

```rust
//! Tests for ring buffer memory ordering correctness.
//!
//! H1: push() uses Relaxed fetch_add for head before write_volatile.
//! On ARM64, a reader could see the updated head but stale data.

use tempfile::tempdir;
use aether_rtdb_shm::ring_buffer::{DataPoint, ShmRingBuffer};

fn make_point(id: u32, value: f64, ts: u64) -> DataPoint {
    DataPoint {
        channel_id: 1001,
        point_type: 0,
        point_id: id,
        _padding: 0,
        value,
        timestamp_us: ts,
    }
}

#[test]
fn test_push_then_read_returns_correct_data() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("test_ordering.ring");

    let mut buffer = ShmRingBuffer::create_or_open(&path, 1024, 60_000_000).unwrap();

    // Write 100 points
    for i in 0..100u32 {
        buffer.push(make_point(i, i as f64 * 1.5, 1000 + i as u64));
    }

    // Read back — all data should be consistent
    let points = buffer.buffer().read_range(1000, 1099);
    assert_eq!(points.len(), 100, "Should read back all 100 points");

    for p in &points {
        let expected_value = p.point_id as f64 * 1.5;
        assert!(
            (p.value - expected_value).abs() < 0.001,
            "Point {} value mismatch: {} vs {}",
            p.point_id,
            p.value,
            expected_value
        );
    }
}

#[test]
fn test_push_total_writes_matches_head() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("test_writes.ring");

    let mut buffer = ShmRingBuffer::create_or_open(&path, 1024, 60_000_000).unwrap();

    for i in 0..50 {
        buffer.push(make_point(i, i as f64, 1000 + i as u64));
    }

    // After the fix, total_writes should exactly match push count
    assert_eq!(buffer.buffer().total_writes(), 50);
    assert_eq!(buffer.buffer().head(), 50);
}
```

**Step 2: Run test**

Run: `cargo test -p aether-rtdb-shm --test test_ring_buffer_ordering -- --nocapture 2>&1 | head -30`
Expected: PASS on x86 (TSO hides ordering bugs), but the test establishes the contract for the fix

**Step 3: Commit**

```bash
git add libs/aether-rtdb-shm/tests/test_ring_buffer_ordering.rs
git commit -m "test(rtdb-shm): add ring buffer ordering tests for H1"
```

---

### Task 7: Fix H1 — RingBuffer push() head ordering

**Files:**
- Modify: `libs/aether-rtdb-shm/src/ring_buffer.rs:241-258`

**Step 1: Apply the fix**

Replace the `push()` method:

```rust
    #[inline]
    pub fn push(&mut self, point: DataPoint) {
        // Reserve slot: Relaxed is safe here because &mut self guarantees
        // single writer, and the Acquire fence in read_range pairs with
        // the Release fence below.
        let pos =
            unsafe { (*self.header).head.fetch_add(1, Ordering::Relaxed) as usize % self.capacity };

        // Write data to reserved slot
        unsafe {
            std::ptr::write_volatile(self.points.add(pos), point);
        }

        // Release fence: ensures the DataPoint write is visible to readers
        // BEFORE total_writes is updated. On ARM64, this emits `dmb ish`.
        // Readers use Acquire fence before reading data.
        fence(Ordering::Release);

        unsafe {
            (*self.header).total_writes.fetch_add(1, Ordering::Release);
        }
    }
```

Key change: `total_writes.fetch_add` now uses `Release` ordering (was `Relaxed`). This ensures readers who observe the updated `total_writes` via `Acquire` will also see the completed `write_volatile`.

**Step 2: Run tests**

Run: `cargo test -p aether-rtdb-shm -- --nocapture 2>&1 | tail -20`
Expected: ALL PASS

**Step 3: Commit**

```bash
git add libs/aether-rtdb-shm/src/ring_buffer.rs
git commit -m "fix(rtdb-shm): H1 use Release ordering for ring buffer total_writes"
```

---

### Task 8: Write failing test for H5 — open_readonly uses MmapMut

**Files:**
- Modify: `libs/aether-rtdb-shm/tests/test_ring_buffer_ordering.rs`

**Step 1: Add test**

Append to the test file:

```rust
#[test]
fn test_open_readonly_cannot_mutate() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("test_readonly.ring");

    // Create and write data
    {
        let mut buffer = ShmRingBuffer::create_or_open(&path, 256, 60_000_000).unwrap();
        buffer.push(make_point(0, 42.0, 1000));
    }

    // Open read-only and verify data is accessible
    let reader = ShmRingBuffer::open_readonly(&path).unwrap();
    let points = reader.buffer().read_range(1000, 1000);
    assert_eq!(points.len(), 1);
    assert!((points[0].value - 42.0).abs() < 0.001);
}
```

**Step 2: Run test**

Run: `cargo test -p aether-rtdb-shm --test test_ring_buffer_ordering test_open_readonly -- --nocapture`
Expected: PASS (current code works but uses MmapMut unnecessarily)

**Step 3: Commit**

```bash
git add libs/aether-rtdb-shm/tests/test_ring_buffer_ordering.rs
git commit -m "test(rtdb-shm): add open_readonly reader test for H5"
```

---

### Task 9: Fix H5 — open_readonly should use read-only Mmap

**Files:**
- Modify: `libs/aether-rtdb-shm/src/ring_buffer.rs:461-492`

**Step 1: Apply the fix**

Replace the `open_readonly()` method:

```rust
    /// Open read-only (for the reader side)
    ///
    /// Uses read-only `Mmap` to enforce no-write at the OS level.
    /// The file is opened read-only and mapped as immutable.
    pub fn open_readonly(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let file = OpenOptions::new().read(true).open(path)?;

        // SAFETY: File is opened read-only. The file was previously created by
        // create_or_open() with a valid layout.
        let mmap = unsafe { Mmap::map(&file)? };
        let data = NonNull::new(mmap.as_ptr() as *mut u8)
            .ok_or_else(|| std::io::Error::other("mmap returned null pointer"))?;

        // Validate header
        let header = unsafe { &*(data.as_ptr() as *const RingBufferHeader) };
        if !header.validate() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Invalid ring buffer header",
            ));
        }

        let capacity = header.capacity as usize;
        let inner = unsafe { HighFreqRingBuffer::from_raw(data, capacity, false, 0) };

        Ok(Self {
            inner,
            _mmap: ShmRingBufferMmap::ReadOnly(mmap),
            _file: file,
        })
    }
```

This requires changing `_mmap` from `MmapMut` to an enum that holds either. Add an enum:

```rust
/// Backing mmap - either read-write (writer) or read-only (reader)
enum ShmRingBufferMmap {
    ReadWrite(MmapMut),
    ReadOnly(Mmap),
}
```

Update `ShmRingBuffer._mmap` field type and `create_or_open` to use `ShmRingBufferMmap::ReadWrite(mmap)`.

**Step 2: Run tests**

Run: `cargo test -p aether-rtdb-shm -- --nocapture 2>&1 | tail -20`
Expected: ALL PASS

**Step 3: Commit**

```bash
git add libs/aether-rtdb-shm/src/ring_buffer.rs
git commit -m "fix(rtdb-shm): H5 use read-only Mmap for open_readonly"
```

---

### Task 10: Write failing test for H6 — push_batch should be atomic reservation

**Files:**
- Modify: `libs/aether-rtdb-shm/tests/test_ring_buffer_ordering.rs`

**Step 1: Add test**

```rust
#[test]
fn test_push_batch_writes_all_points() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("test_batch.ring");

    let mut buffer = ShmRingBuffer::create_or_open(&path, 1024, 60_000_000).unwrap();

    let points: Vec<DataPoint> = (0..50)
        .map(|i| make_point(i, i as f64 * 3.0, 2000 + i as u64))
        .collect();

    buffer.push_batch(&points);

    assert_eq!(buffer.buffer().total_writes(), 50);
    assert_eq!(buffer.buffer().head(), 50);

    let read_back = buffer.buffer().read_range(2000, 2049);
    assert_eq!(read_back.len(), 50);
}

#[test]
fn test_push_batch_contiguous_slots() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("test_batch_contiguous.ring");

    let mut buffer = ShmRingBuffer::create_or_open(&path, 1024, 60_000_000).unwrap();

    // Write a batch — all points should occupy contiguous slots
    let points: Vec<DataPoint> = (0..10)
        .map(|i| make_point(i, i as f64, 5000 + i as u64))
        .collect();

    let head_before = buffer.buffer().head();
    buffer.push_batch(&points);
    let head_after = buffer.buffer().head();

    // Head should advance by exactly the batch size
    assert_eq!(head_after - head_before, 10);
}
```

**Step 2: Run test**

Run: `cargo test -p aether-rtdb-shm --test test_ring_buffer_ordering test_push_batch -- --nocapture`
Expected: PASS (current serial implementation is correct, just slow)

**Step 3: Commit**

```bash
git add libs/aether-rtdb-shm/tests/test_ring_buffer_ordering.rs
git commit -m "test(rtdb-shm): add push_batch tests for H6 optimization"
```

---

### Task 11: Fix H6 — optimize push_batch with single head reservation

**Files:**
- Modify: `libs/aether-rtdb-shm/src/ring_buffer.rs:261-266`

**Step 1: Apply the fix**

Replace the `push_batch()` method:

```rust
    /// Batch write with single head reservation
    ///
    /// Reserves N consecutive slots with one atomic fetch_add, then writes
    /// all points without per-item atomic overhead. Single Release fence
    /// after all writes ensures batch visibility.
    #[inline]
    pub fn push_batch(&mut self, points: &[DataPoint]) {
        if points.is_empty() {
            return;
        }

        let count = points.len();
        // Reserve `count` consecutive slots with a single atomic operation
        let start_pos = unsafe {
            (*self.header).head.fetch_add(count as u64, Ordering::Relaxed) as usize
        };

        // Write all points to reserved slots
        for (i, point) in points.iter().enumerate() {
            let pos = (start_pos + i) % self.capacity;
            unsafe {
                std::ptr::write_volatile(self.points.add(pos), *point);
            }
        }

        // Single Release fence for the entire batch
        fence(Ordering::Release);

        // Update total_writes by batch count
        unsafe {
            (*self.header)
                .total_writes
                .fetch_add(count as u64, Ordering::Release);
        }
    }
```

Key improvement: Previously, N pushes = N `fetch_add` + N fences. Now: 1 `fetch_add` + N `write_volatile` + 1 fence. For a typical 50-point batch, this eliminates 49 atomic RMW operations and 49 fences.

**Step 2: Run tests**

Run: `cargo test -p aether-rtdb-shm -- --nocapture 2>&1 | tail -20`
Expected: ALL PASS

**Step 3: Commit**

```bash
git add libs/aether-rtdb-shm/src/ring_buffer.rs
git commit -m "fix(rtdb-shm): H6 optimize push_batch with single head reservation"
```

---

### Task 12: Add RingBufferHeader alignment

**Files:**
- Modify: `libs/aether-rtdb-shm/src/ring_buffer.rs:128-129`

**Step 1: Apply the fix**

Change `#[repr(C)]` to `#[repr(C, align(64))]` on `RingBufferHeader`:

```rust
#[repr(C, align(64))]
pub struct RingBufferHeader {
```

This ensures the header occupies exactly one cache line, preventing false sharing between the header's atomics and the first data point.

**Step 2: Run tests**

Run: `cargo test -p aether-rtdb-shm -- --nocapture 2>&1 | tail -20`
Expected: ALL PASS (size may change; check `calculate_size` still works)

**Step 3: Commit**

```bash
git add libs/aether-rtdb-shm/src/ring_buffer.rs
git commit -m "fix(rtdb-shm): align RingBufferHeader to 64-byte cache line"
```

---

### Task 13: Verify Group B complete

**Step 1: Run full crate test suite**

Run: `cargo test -p aether-rtdb-shm -- --nocapture 2>&1 | tail -30`
Expected: ALL PASS

**Step 2: Run clippy**

Run: `cargo clippy -p aether-rtdb-shm -- -D warnings 2>&1 | tail -20`
Expected: No warnings

**Step 3: Commit if any remaining changes**

```bash
git add -A libs/aether-rtdb-shm/
git commit -m "test(rtdb-shm): Group B complete — H1, H5, H6 ring buffer fixes verified"
```

---

### Task 14: Write failing test for C1 — read_unaligned on Atomic types

**Files:**
- Test: `libs/aether-rtdb-shm/tests/test_snapshot_safety.rs`

**Step 1: Write the test**

```rust
//! Tests for snapshot restore safety.
//!
//! C1: read_unaligned on UnifiedHeader/PointSlot containing AtomicU64 is UB
//! because Atomic types require alignment for correctness.

use std::collections::HashMap;
use tempfile::tempdir;
use aether_routing::RoutingCache;
use aether_rtdb_shm::{SharedConfig, UnifiedWriter};

fn test_config(dir: &std::path::Path) -> SharedConfig {
    SharedConfig::default()
        .with_path(dir.join("test.shm"))
        .with_max_slots(1000)
}

fn test_routing_cache() -> RoutingCache {
    let mut c2m = HashMap::new();
    for i in 0..10 {
        c2m.insert(format!("1001:T:{}", i), format!("23:M:{}", i));
    }
    RoutingCache::from_maps(c2m, HashMap::new(), HashMap::new())
}

#[test]
fn test_snapshot_save_and_restore_roundtrip() {
    let dir = tempdir().unwrap();
    let config = test_config(dir.path());
    let routing = test_routing_cache();
    let now = 1704067200000u64;

    // Create writer and write data
    let writer = UnifiedWriter::create(&config, &routing).unwrap();
    for i in 0..10 {
        writer.set(1001, 0, i, i as f64 * 10.0, i as f64 * 100.0, now + i as u64);
    }
    writer.flush().unwrap();

    // Save snapshot
    let snapshot_path = dir.path().join("snapshot.bin");
    writer.save_snapshot(&snapshot_path).unwrap();

    // Restore from snapshot — this exercises the C1 code path
    let restored =
        UnifiedWriter::restore_from_snapshot(&config, &routing, &snapshot_path).unwrap();

    // Verify restored data
    for i in 0..10 {
        let (value, ts) = restored.get_channel_value(1001, 0, i).unwrap();
        assert!(
            (value - i as f64 * 10.0).abs() < 0.001,
            "Point {} value mismatch after restore",
            i
        );
    }
}
```

**Step 2: Run test**

Run: `cargo test -p aether-rtdb-shm --test test_snapshot_safety -- --nocapture 2>&1 | head -30`
Expected: PASS on x86 (alignment is naturally satisfied), but the UB must be fixed for correctness

**Step 3: Commit**

```bash
git add libs/aether-rtdb-shm/tests/test_snapshot_safety.rs
git commit -m "test(rtdb-shm): add snapshot restore roundtrip test for C1"
```

---

### Task 15: Fix C1 — replace read_unaligned with safe field extraction

**Files:**
- Modify: `libs/aether-rtdb-shm/src/unified_shm.rs:743-747` and `libs/aether-rtdb-shm/src/unified_shm.rs:806-812`

**Step 1: Fix snapshot header reading (line ~747)**

Replace:
```rust
let snapshot_header =
    unsafe { std::ptr::read_unaligned(snapshot_data.as_ptr() as *const UnifiedHeader) };
```

With a safe extraction that reads plain fields manually:

```rust
// Read header fields individually from unaligned buffer.
// We cannot use read_unaligned on UnifiedHeader because it contains
// AtomicU64 fields, and creating an Atomic via read_unaligned is UB
// (atomics require alignment for hardware atomic instructions).
let snapshot_header = {
    let ptr = snapshot_data.as_ptr();
    let magic = u64::from_ne_bytes(snapshot_data[0..8].try_into().unwrap());
    let version = u32::from_ne_bytes(snapshot_data[8..12].try_into().unwrap());
    // routing_hash is at a known offset in UnifiedHeader
    // We read it as plain u64 since this is a file snapshot, not live SHM
    SnapshotHeader { magic, version }
};

if snapshot_header.magic != UNIFIED_MAGIC {
    bail!(
        "Invalid snapshot magic: expected 0x{:X}, got 0x{:X}",
        UNIFIED_MAGIC,
        snapshot_header.magic
    );
}
if snapshot_header.version != UNIFIED_VERSION {
    bail!(
        "Snapshot version mismatch: expected {}, got {}",
        UNIFIED_VERSION,
        snapshot_header.version
    );
}
```

Add a plain struct for snapshot header:

```rust
/// Non-atomic header for reading from file snapshots.
/// Avoids UB from read_unaligned on Atomic types.
struct SnapshotHeader {
    magic: u64,
    version: u32,
}
```

Read `routing_hash` and `slot_count` as plain `u64`/`u32` from known offsets using byte slicing instead of `read_unaligned`.

**Step 2: Fix PointSlot reading (line ~808)**

Replace:
```rust
let snapshot_slot = unsafe {
    std::ptr::read_unaligned(
        snapshot_data.as_ptr().add(slot_offset_in_file) as *const PointSlot
    )
};
```

With safe field extraction:

```rust
// Read PointSlot fields individually from unaligned buffer.
// PointSlot contains AtomicU64 — read_unaligned is UB on Atomic types.
let slot_bytes = &snapshot_data[slot_offset_in_file..slot_offset_in_file + slot_size];
let value = f64::from_bits(u64::from_ne_bytes(slot_bytes[0..8].try_into().unwrap()));
let timestamp = u64::from_ne_bytes(slot_bytes[8..16].try_into().unwrap());
let raw = f64::from_bits(u64::from_ne_bytes(slot_bytes[16..24].try_into().unwrap()));
// flags at bytes 24..32 — we only need the data, not the seqlock state
```

Then use the extracted `value`, `raw`, `timestamp` directly instead of `snapshot_slot.get_value()` etc.

**Step 3: Run tests**

Run: `cargo test -p aether-rtdb-shm -- --nocapture 2>&1 | tail -20`
Expected: ALL PASS

**Step 4: Commit**

```bash
git add libs/aether-rtdb-shm/src/unified_shm.rs
git commit -m "fix(rtdb-shm): C1 eliminate read_unaligned UB on Atomic types in snapshot restore"
```

---

### Task 16: Write failing test for C2 + H3 — reconfigure fence and flush

**Files:**
- Modify: `libs/aether-rtdb-shm/tests/test_snapshot_safety.rs`

**Step 1: Add test**

```rust
#[test]
fn test_reconfigure_existing_clears_slots_before_publishing() {
    let dir = tempdir().unwrap();
    let config = test_config(dir.path());
    let routing = test_routing_cache();
    let now = 1704067200000u64;

    // Create initial writer with data
    let writer = UnifiedWriter::create(&config, &routing).unwrap();
    for i in 0..10 {
        writer.set(1001, 0, i, 999.0, 999.0, now);
    }
    writer.flush().unwrap();
    drop(writer);

    // Reconfigure with same routing — all slots should be zeroed
    let writer2 = UnifiedWriter::reconfigure_existing(&config, &routing).unwrap();

    // All slots should be zero after reconfigure (not stale 999.0)
    for i in 0..10 {
        let (value, ts) = writer2.get_channel_value(1001, 0, i).unwrap_or((0.0, 0));
        assert!(
            (value - 0.0).abs() < 0.001,
            "Slot {} should be zeroed after reconfigure, got {}",
            i,
            value
        );
        assert_eq!(ts, 0, "Slot {} timestamp should be 0 after reconfigure", i);
    }
}
```

**Step 2: Run test**

Run: `cargo test -p aether-rtdb-shm --test test_snapshot_safety test_reconfigure -- --nocapture`
Expected: PASS

**Step 3: Commit**

```bash
git add libs/aether-rtdb-shm/tests/test_snapshot_safety.rs
git commit -m "test(rtdb-shm): add reconfigure_existing slot clearing test for C2+H3"
```

---

### Task 17: Fix C2 — add fence between fill(0) and header stores in reconfigure_existing

**Files:**
- Modify: `libs/aether-rtdb-shm/src/unified_shm.rs:660-675`

**Step 1: Apply the fix**

Replace the reconfigure_existing section (after `fill(0)` and before header stores):

```rust
        // Zero all slot data to prevent stale values
        mmap[slot_offset()..].fill(0);

        // FULL BARRIER: ensure all zero-fills are globally visible
        // before the new routing_hash/slot_count are published.
        // Without this fence, a reader on ARM64 could see the new
        // routing_hash but read stale (non-zero) slot data.
        fence(Ordering::SeqCst);

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or(std::time::Duration::ZERO)
            .as_millis() as u64;

        let header = unsafe { &mut *(mmap.as_mut_ptr() as *mut UnifiedHeader) };
        header
            .slot_count
            .store(slot_count as u32, Ordering::Release);
        header.last_update_ts.store(now_ms, Ordering::Relaxed);
        header.writer_heartbeat.store(now_ms, Ordering::Relaxed);
        header
            .routing_hash
            .store(routing_cache.content_hash(), Ordering::Release);

        // Flush mmap to backing file — ensures cross-process visibility
        // on systems where mmap coherency isn't guaranteed (H3).
        mmap.flush()?;
```

Add `use std::sync::atomic::fence;` at the top of the file if not already imported.

**Step 2: Run tests**

Run: `cargo test -p aether-rtdb-shm -- --nocapture 2>&1 | tail -20`
Expected: ALL PASS

**Step 3: Commit**

```bash
git add libs/aether-rtdb-shm/src/unified_shm.rs
git commit -m "fix(rtdb-shm): C2+H3 add fence before header publish and mmap flush in reconfigure"
```

---

### Task 18: Verify Group C complete

**Step 1: Run full crate tests**

Run: `cargo test -p aether-rtdb-shm -- --nocapture 2>&1 | tail -30`
Expected: ALL PASS

**Step 2: Run clippy**

Run: `cargo clippy -p aether-rtdb-shm -- -D warnings 2>&1 | tail -20`
Expected: No warnings

**Step 3: Commit**

```bash
git add -A libs/aether-rtdb-shm/
git commit -m "test(rtdb-shm): Group C complete — C1, C2, H3 unified_shm fixes verified"
```

---

### Task 19: Write failing test for H2 — rebuild_writer error propagation

**Files:**
- Test: `libs/aether-rtdb-shm/tests/test_shm_dispatch_recovery.rs` (note: this tests the trait behavior, actual ShmDispatch tests need modsrv context)

Since `ShmDispatch` lives in modsrv, we test at the `ActionDispatch` trait level.

**Step 1: Write test in modsrv**

File: `services/modsrv/src/infra/shm_dispatch.rs` — add test module at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rebuild_writer_failure_logs_warning() {
        // ShmDispatch with no config set — rebuild should be a no-op
        let dispatch = ShmDispatch::new();
        let routing = aether_routing::RoutingCache::empty();

        // Should not panic, just return early
        dispatch.rebuild_writer(&routing);

        // Writer should still be None (not cleared from a valid state)
        assert!(dispatch.writer.load().is_none());
    }

    #[test]
    fn test_rebuild_writer_with_invalid_config_clears_writer() {
        let dispatch = ShmDispatch::new();

        // Set config with a non-existent path
        let bad_config = aether_rtdb_shm::SharedConfig::default()
            .with_path(std::path::PathBuf::from("/nonexistent/path/test.shm"));
        let _ = dispatch.config.set(bad_config);

        let routing = aether_routing::RoutingCache::empty();
        dispatch.rebuild_writer(&routing);

        // After failed rebuild, writer should be None
        // H2 fix: this should return a Result, but for now verify the clear behavior
        assert!(dispatch.writer.load().is_none());
    }
}
```

**Step 2: Run test**

Run: `cargo test -p modsrv shm_dispatch -- --nocapture 2>&1 | head -30`
Expected: PASS (tests document current behavior)

**Step 3: Commit**

```bash
git add services/modsrv/src/infra/shm_dispatch.rs
git commit -m "test(modsrv): add shm_dispatch rebuild_writer tests for H2"
```

---

### Task 20: Fix H2 — rebuild_writer returns Result instead of silently clearing

**Files:**
- Modify: `services/modsrv/src/infra/shm_dispatch.rs:185-199`

**Step 1: Change trait to return Result**

In the `ActionDispatch` trait (line 44):

```rust
    /// Rebuild internal writer state after routing changes
    ///
    /// Returns Ok(()) on success, Err on failure (caller decides recovery strategy).
    fn rebuild_writer(&self, routing_cache: &aether_routing::RoutingCache) -> anyhow::Result<()>;
```

Update `ShmDispatch::rebuild_writer`:

```rust
    fn rebuild_writer(&self, routing_cache: &aether_routing::RoutingCache) -> anyhow::Result<()> {
        let Some(config) = self.config.get() else {
            return Ok(()); // SHM not configured — not an error
        };
        match aether_rtdb_shm::UnifiedWriter::open_for_actions(config, routing_cache) {
            Ok(writer) => {
                self.writer.store(Some(Arc::new(writer)));
                info!("SHM action writer rebuilt after routing change");
                Ok(())
            },
            Err(e) => {
                // Keep the EXISTING writer active — don't clear it on transient failures.
                // The old writer may have stale routing but is better than no writer.
                warn!("SHM action writer rebuild failed, keeping previous writer: {}", e);
                Err(e)
            },
        }
    }
```

Update `NoopDispatch::rebuild_writer`:

```rust
    fn rebuild_writer(&self, _routing_cache: &aether_routing::RoutingCache) -> anyhow::Result<()> {
        Ok(())
    }
```

**Step 2: Update callers**

Search for all `rebuild_writer` calls and update to handle the Result. The caller in `instance_routing.rs` should log the error but continue:

```rust
if let Err(e) = self.dispatch.rebuild_writer(routing_cache) {
    tracing::warn!("Dispatch writer rebuild failed: {e}");
}
```

**Step 3: Run tests**

Run: `cargo test -p modsrv -- --nocapture 2>&1 | tail -20`
Expected: ALL PASS (may need to fix compilation errors from trait change)

Run: `cargo check -p modsrv -p comsrv 2>&1 | tail -20`
Expected: PASS

**Step 4: Commit**

```bash
git add services/modsrv/ libs/aether-rtdb-shm/
git commit -m "fix(modsrv): H2 rebuild_writer returns Result, keeps stale writer on failure"
```

---

### Task 21: Verify Group D complete

**Step 1: Run full workspace check**

Run: `cargo check --workspace 2>&1 | tail -20`
Expected: PASS

**Step 2: Run modsrv tests**

Run: `cargo test -p modsrv -- --nocapture 2>&1 | tail -20`
Expected: ALL PASS

**Step 3: Commit**

```bash
git commit --allow-empty -m "test(modsrv): Group D complete — H2 dispatch recovery verified"
```

---

### Task 22: Final integration verification

**Step 1: Run full workspace tests**

Run: `cargo test --workspace 2>&1 | tail -40`
Expected: ALL PASS

**Step 2: Run clippy on full workspace**

Run: `cargo clippy --workspace -- -D warnings 2>&1 | tail -20`
Expected: No warnings

**Step 3: Run quick-check**

Run: `./scripts/quick-check.sh 2>&1 | tail -20`
Expected: ALL PASS

**Step 4: Final commit**

```bash
git commit --allow-empty -m "chore: SHM hardening complete — C1,C2,H1-H6 all verified"
```

---

## Verification Checklist

- [ ] C1: No `read_unaligned` on types containing `AtomicU64`
- [ ] C2: Fence between `fill(0)` and header Release stores in `reconfigure_existing`
- [ ] H1: `total_writes.fetch_add` uses `Release` ordering
- [ ] H2: `rebuild_writer` returns `Result`, keeps stale writer on failure
- [ ] H3: `mmap.flush()` called after reconfigure
- [ ] H4: Dirty flag `fetch_or(1)` moved inside seqlock fence envelope
- [ ] H5: `open_readonly` uses read-only `Mmap`
- [ ] H6: `push_batch` uses single head reservation
- [ ] All tests pass: `cargo test --workspace`
- [ ] No clippy warnings: `cargo clippy --workspace -- -D warnings`
