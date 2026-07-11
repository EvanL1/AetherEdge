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

    let (value, raw, ts) = slot.load_consistent().unwrap();
    assert!((value - 49.0).abs() < 0.001);
    assert!((raw - 98.0).abs() < 0.001);
    assert_eq!(ts, 1049);
    assert_eq!(slot.seq_raw(), 100); // 50 writes × 2
}

/// Verify that the seqlock sequence counter wraps correctly at u32::MAX.
///
/// When seq == u32::MAX - 1 (even), the next write does:
///   open:  wrapping_add(1) → u32::MAX       (odd  — write in progress)
///   close: wrapping_add(2) → 1              (odd?  — no: MAX+2 wraps to 1, which is ODD)
///
/// Wait — let's be precise. If seq starts at u32::MAX - 3 (even), two writes produce:
///   write 1 open:  (MAX-3).wrapping_add(1) = MAX-2  (even? MAX-3 is odd if MAX is odd…)
///
/// u32::MAX = 4_294_967_295 (odd). So:
///   MAX - 3 = 4_294_967_292 (even) ← good starting point
///   write 1 open:  wrapping_add(1) = MAX-2 = 4_294_967_293  (odd  ✓)
///   write 1 close: wrapping_add(2) = MAX-1 = 4_294_967_294  (even ✓)
///   write 2 open:  wrapping_add(1) = MAX   = 4_294_967_295  (odd  ✓)
///   write 2 close: wrapping_add(2) → 0+1   = 1              — ERROR: MAX+2 wraps to 1 (ODD!)
///
/// Actually: set() loads the current (even) seq `s`, then stores s+1 (odd) and s+2 (even).
/// So starting from MAX-1 (even):
///   write 1 open:  MAX-1+1 = MAX       (odd  ✓)
///   write 1 close: MAX-1+2 = MAX+1 = 0 (even ✓, wraps to 0)
///   write 2 open:  0+1 = 1             (odd  ✓)
///   write 2 close: 0+2 = 2             (even ✓)
///
/// Starting point: seed seq = u32::MAX - 1 (even), do 2 writes, expect final seq == 2.
#[test]
fn test_seq_wrapping_at_u32_max() {
    use aether_rtdb_shm::PointSlot;

    let slot = PointSlot::new();

    // Seed the seq counter just below the wrapping boundary.
    // u32::MAX = 4_294_967_295 (odd), so MAX-1 is even — valid idle state.
    let seed = u32::MAX - 1;
    slot.set_seq_for_testing(seed);
    assert_eq!(slot.seq_raw(), seed);
    assert_eq!(seed % 2, 0, "seed must be even (idle seqlock state)");

    // Write 1: seq goes  MAX-1 → MAX (odd, write-in-progress) → 0 (even, done)
    slot.set(1.0, 10.0, 1001);
    assert_eq!(slot.seq_raw(), 0, "after write 1: seq should wrap to 0");

    // Read back is consistent
    let result = slot.try_load_consistent();
    assert!(
        result.is_some(),
        "try_load_consistent must succeed when seq is even"
    );
    let (v, r, ts) = result.unwrap();
    assert!((v - 1.0).abs() < f64::EPSILON);
    assert!((r - 10.0).abs() < f64::EPSILON);
    assert_eq!(ts, 1001);

    // Write 2: seq goes  0 → 1 (odd) → 2 (even)
    slot.set(2.0, 20.0, 2002);
    assert_eq!(slot.seq_raw(), 2, "after write 2: seq should be 2");

    // Read back is still consistent
    let result = slot.try_load_consistent();
    assert!(
        result.is_some(),
        "try_load_consistent must succeed after wrap"
    );
    let (v, r, ts) = result.unwrap();
    assert!((v - 2.0).abs() < f64::EPSILON);
    assert!((r - 20.0).abs() < f64::EPSILON);
    assert_eq!(ts, 2002);
}

/// Verify that `try_load_consistent` returns `None` when seq is odd.
///
/// An odd sequence counter signals that a write is in progress. The reader
/// must bail out immediately rather than returning potentially torn data.
#[test]
fn test_load_consistent_returns_none_during_write() {
    use aether_rtdb_shm::PointSlot;

    let slot = PointSlot::new();

    // Prime the slot with a known value so data fields are non-zero.
    slot.set(42.0, 420.0, 12345);
    assert_eq!(
        slot.seq_raw() % 2,
        0,
        "seq must be even after a completed write"
    );

    // Force seq to odd — simulates the mid-write window where open-seq has been
    // stored but data fields and close-seq have not yet been written.
    let current_even = slot.seq_raw();
    slot.set_seq_for_testing(current_even + 1); // now odd
    assert_eq!(
        slot.seq_raw() % 2,
        1,
        "seq must be odd to simulate write-in-progress"
    );

    // try_load_consistent must detect the odd seq and return None.
    let result = slot.try_load_consistent();
    assert!(
        result.is_none(),
        "try_load_consistent must return None when seq is odd (write in progress)"
    );

    // Restore seq to even — simulates write completion.
    slot.set_seq_for_testing(current_even + 2); // back to even
    assert_eq!(slot.seq_raw() % 2, 0, "restored seq should be even");

    // Now try_load_consistent must succeed again.
    let result = slot.try_load_consistent();
    assert!(
        result.is_some(),
        "try_load_consistent must succeed once seq is even again"
    );
}
