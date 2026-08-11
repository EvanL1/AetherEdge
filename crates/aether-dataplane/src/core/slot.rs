//! Vec-based in-memory point storage
//!
//! Provides atomic `PointSlot` for lock-free point data access in shared memory.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering, fence};

// ========== Instance Point Type Constants ==========

// ========== PointSlot ==========

/// Point slot for atomic storage of point data
///
/// 32-byte aligned for cache-line friendliness.
/// Uses atomic operations for lock-free concurrent access.
///
/// # Seqlock Protocol
///
/// Uses `seq: AtomicU32` as a seqlock sequence counter to guarantee
/// consistent reads across all fields. This prevents torn reads when the same
/// slot is written multiple times within the same millisecond (where timestamp
/// alone cannot distinguish writes).
///
/// - **Writer**: increments sequence to odd (write-in-progress), writes data,
///   increments sequence to even (write-complete).
/// - **Reader**: reads sequence, reads data, re-reads sequence. If both reads
///   match and the value is even, the data is consistent.
///
/// # Safety (shared memory usage)
///
/// This struct is `#[repr(C)]` to guarantee a deterministic field layout
/// when cast from raw pointers in `core::slot_io` and `core::snapshot_save`.
/// The compile-time assertion below ensures the size never drifts from
/// expectations.
#[repr(C, align(32))]
pub(crate) struct PointSlot {
    /// Engineering value (IEEE 754 double as bits)
    value_bits: AtomicU64,
    /// Timestamp in milliseconds
    timestamp: AtomicU64,
    /// Raw value (as bits)
    raw_bits: AtomicU64,
    /// Seqlock sequence counter (odd = write in progress, even = idle)
    seq: AtomicU32,
    /// Opaque point-quality code supplied by the composition layer.
    quality_code: AtomicU32,
}

const _: () = assert!(std::mem::size_of::<PointSlot>() == 32);

impl Default for PointSlot {
    fn default() -> Self {
        Self::new()
    }
}

/// Bit pattern of a quiet f64 NaN, used as the "no data" sentinel for slot
/// values. We hardcode the bits (instead of `f64::NAN.to_bits()`) so this
/// remains usable from `const` contexts and is byte-stable across platforms.
/// IEEE 754 binary64 quiet NaN: sign=0, exponent=all 1s, fraction MSB=1.
pub(crate) const SLOT_UNWRITTEN_BITS: u64 = 0x7FF8_0000_0000_0000;

impl PointSlot {
    /// Create a new empty point slot.
    ///
    /// Both `value_bits` and `raw_bits` start as quiet NaN — the explicit
    /// "no data has ever been written here" marker. Readers probe the
    /// returned value with `f64::is_nan()`.
    /// This avoids the historical 0.0 ambiguity where a default-initialised
    /// slot was indistinguishable from a real device reading of zero.
    const fn new() -> Self {
        Self {
            value_bits: AtomicU64::new(SLOT_UNWRITTEN_BITS),
            timestamp: AtomicU64::new(0),
            raw_bits: AtomicU64::new(SLOT_UNWRITTEN_BITS),
            seq: AtomicU32::new(0),
            quality_code: AtomicU32::new(0),
        }
    }

    /// Reset this slot to the "unwritten" sentinel state in-place.
    ///
    /// Used by `SlotWriter::create` after `set_len` zero-fills the mmap
    /// region: zero-filled bytes decode as `(value=0.0, raw=0.0)`, which is
    /// ambiguous with a real reading, so we overwrite those into NaN here.
    /// `timestamp`, `seq`, and `quality_code` correctly stay 0.
    ///
    /// **Single-writer init only.** Must not race with `set()`; intended for
    /// the writer's startup loop before any reader has been published.
    #[inline]
    pub(crate) fn init_unwritten(&self) {
        self.value_bits
            .store(SLOT_UNWRITTEN_BITS, Ordering::Relaxed);
        self.raw_bits.store(SLOT_UNWRITTEN_BITS, Ordering::Relaxed);
    }

    /// Single-attempt seqlock read. Returns `None` on contention instead of retrying.
    ///
    /// Callers that observe contention skip the sample and retry on their next
    /// read cycle rather than spinning in the data plane.
    ///
    /// ## Algorithm (classic Linux seqlock pattern)
    ///
    /// 1. Read seq1 (Relaxed). If odd → write in progress → return None.
    /// 2. **fence(Acquire)** — data loads cannot be reordered before seq1.
    /// 3. Read value, raw, timestamp, and quality (Relaxed).
    /// 4. **fence(Acquire)** — data loads cannot be reordered after seq2.
    /// 5. Read seq2 (Relaxed). If seq1 == seq2 → consistent snapshot.
    ///
    /// ## Why two fences, not `load(Acquire)` on seq2
    ///
    /// A single `load(Acquire)` on seq2 is insufficient on weakly-ordered
    /// architectures (e.g. AArch64). Acquire-load (`ldar`) only prevents
    /// *subsequent* loads from being reordered before it — it does **not**
    /// prevent *preceding* data loads from being reordered after it. That
    /// allows a reader to observe `seq1 == seq2` while its data loads
    /// straddled a full writer cycle, producing a torn read.
    ///
    /// The second `fence(Acquire)` — emitted as `dmb ishld` on AArch64 —
    /// is a bidirectional load-load barrier that prevents data loads from
    /// slipping past seq2. On x86 (TSO) load-load reordering is already
    /// prohibited, so both fences compile to compiler barriers only.
    #[inline]
    pub(crate) fn try_load_consistent(&self) -> Option<(f64, f64, u64, u32)> {
        let seq1 = self.seq.load(Ordering::Relaxed);

        if seq1 & 1 != 0 {
            return None; // write in progress
        }

        // First Acquire fence: prevents data loads from being reordered
        // before seq1. On AArch64: dmb ishld. On x86: compiler barrier.
        fence(Ordering::Acquire);

        let value = f64::from_bits(self.value_bits.load(Ordering::Relaxed));
        let raw = f64::from_bits(self.raw_bits.load(Ordering::Relaxed));
        let ts = self.timestamp.load(Ordering::Relaxed);
        let quality_code = self.quality_code.load(Ordering::Relaxed);

        // Second Acquire fence: prevents data loads from being reordered
        // past seq2. Critical on AArch64 where load-load reordering is
        // allowed and `ldar` alone does not back-fence prior loads.
        fence(Ordering::Acquire);

        let seq2 = self.seq.load(Ordering::Relaxed);

        if seq1 == seq2 {
            Some((value, raw, ts, quality_code))
        } else {
            None
        }
    }

    /// Set all point data with seqlock write protocol.
    ///
    /// Stores the sequence counter to odd (write-in-progress), writes all
    /// data fields, then stores to even (write-complete). Readers that
    /// observe an odd sequence or a changed sequence will retry.
    ///
    /// # Memory Ordering (classic Linux write_seqcount pattern)
    ///
    /// 1. `store(seq+1, Relaxed)` — mark write-in-progress (odd).
    /// 2. **`fence(Release)`** — data stores cannot be reordered before
    ///    the seq→odd store. On AArch64: `dmb ishst`. On x86: compiler barrier.
    /// 3. Data stores (Relaxed) — value, raw, timestamp, and quality.
    /// 4. `store(seq+2, Release)` — mark write-complete (even). Release
    ///    ordering prevents data stores from being reordered past it,
    ///    pairing with the reader's trailing Acquire fence.
    ///
    /// Without the middle Release fence a Release store on seq→odd only
    /// blocks *preceding* ops from moving past it — *subsequent* data
    /// stores could still be reordered before the odd-seq publication on
    /// weakly-ordered hardware (AArch64), allowing readers to observe
    /// seq==even while data is already partially mutated.
    ///
    /// # Single-writer assumption
    ///
    /// Uses `fetch_add` for the seq counter so each begin/end increment is a
    /// single atomic RMW — even if a second writer were to race in, the seq
    /// counter still advances monotonically and any reader observing an odd
    /// value will retry. SCADA architecture still mandates single-writer per
    /// slot (io owns T/S, automation owns C/A) to avoid data tearing, but the
    /// counter itself is no longer load-bearing on that convention.
    #[inline]
    pub(crate) fn set(&self, value: f64, raw: f64, timestamp: u64, quality_code: u32) {
        // Begin write: seq → odd (signals write-in-progress). Relaxed RMW —
        // the following Release fence establishes ordering with data stores.
        let old = self.seq.fetch_add(1, Ordering::Relaxed);
        debug_assert!(
            old & 1 == 0,
            "PointSlot::set() entered with odd prior seq={} — concurrent writer \
             or incomplete prior write. Single-writer invariant violated.",
            old
        );

        // Release fence: data stores below cannot be reordered before the
        // seq→odd RMW above. Critical on AArch64 (dmb ishst) to ensure an
        // observer that sees data=NEW also sees seq=odd.
        fence(Ordering::Release);

        // Data stores — Relaxed; ordering enforced by surrounding fences/RMWs.
        self.value_bits.store(value.to_bits(), Ordering::Relaxed);
        self.raw_bits.store(raw.to_bits(), Ordering::Relaxed);
        self.timestamp.store(timestamp, Ordering::Relaxed);
        self.quality_code.store(quality_code, Ordering::Relaxed);

        // End write: seq → even (signals write-complete). Release prevents
        // prior data stores from being reordered past this RMW, pairing with
        // the reader's trailing Acquire fence. x86: lock add. ARM64: ldaddal.
        self.seq.fetch_add(1, Ordering::Release);
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;

    #[test]
    fn test_point_slot_atomic_ops() {
        let slot = PointSlot::new();
        slot.set(100.5, 1005.0, 1729000000, 3);

        assert_eq!(
            slot.try_load_consistent(),
            Some((100.5, 1005.0, 1729000000, 3))
        );
    }

    // ========== try_load_consistent Tests ==========

    #[test]
    fn test_try_load_consistent_basic() {
        let slot = PointSlot::new();
        slot.set(100.5, 1005.0, 1729000000, 7);

        // load_consistent should return same values as individual getters
        let (value, raw, ts, quality_code) = slot.try_load_consistent().unwrap();
        assert_eq!(value, 100.5);
        assert_eq!(raw, 1005.0);
        assert_eq!(ts, 1729000000);
        assert_eq!(quality_code, 7);
    }

    #[test]
    fn test_try_load_consistent_preserves_all_fields() {
        let slot = PointSlot::new();
        slot.set(42.0, 420.0, 12345, 2);

        // try_load_consistent should succeed without concurrent writes
        let result = slot.try_load_consistent();
        assert!(result.is_some());

        let (value, raw, ts, quality_code) = result.unwrap();
        assert_eq!(value, 42.0);
        assert_eq!(raw, 420.0);
        assert_eq!(ts, 12345);
        assert_eq!(quality_code, 2);
    }

    #[test]
    fn test_try_load_consistent_multiple_updates() {
        let slot = PointSlot::new();

        // Multiple sequential updates
        for i in 1..=10 {
            let v = i as f64 * 10.0;
            slot.set(v, v * 10.0, i as u64 * 1000, i);

            let (value, raw, ts, quality_code) = slot.try_load_consistent().unwrap();
            assert_eq!(value, v);
            assert_eq!(raw, v * 10.0);
            assert_eq!(ts, i as u64 * 1000);
            assert_eq!(quality_code, i);
        }
    }

    #[test]
    fn test_try_load_consistent_default_is_unwritten() {
        // A freshly-created PointSlot is "unwritten" — value/raw are NaN
        // sentinels, never the ambiguous 0.0 default. Metadata stays 0.
        let slot = PointSlot::new();

        let (value, raw, ts, quality_code) = slot.try_load_consistent().unwrap();
        assert!(
            value.is_nan(),
            "default value must be NaN sentinel, got {}",
            value
        );
        assert!(
            raw.is_nan(),
            "default raw must be NaN sentinel, got {}",
            raw
        );
        assert_eq!(ts, 0);
        assert_eq!(quality_code, 0);

        // After a real write, NaN goes away and is_unwritten flips.
        slot.set(0.0, 0.0, 1, 1);
        let (value, raw, ts, quality_code) = slot.try_load_consistent().unwrap();
        assert_eq!(value, 0.0);
        assert_eq!(raw, 0.0);
        assert_eq!(ts, 1);
        assert_eq!(quality_code, 1);
    }

    #[test]
    fn test_try_load_consistent_same_timestamp() {
        // Regression test: multiple writes with identical timestamp must still
        // produce consistent reads via seqlock (not timestamp comparison).
        let slot = PointSlot::new();
        let same_ts = 1729000000u64;

        slot.set(100.0, 1000.0, same_ts, 1);
        let (v1, r1, t1, q1) = slot.try_load_consistent().unwrap();
        assert_eq!(v1, 100.0);
        assert_eq!(r1, 1000.0);
        assert_eq!(t1, same_ts);
        assert_eq!(q1, 1);

        // Second write with SAME timestamp but different values
        slot.set(200.0, 2000.0, same_ts, 2);
        let (v2, r2, t2, q2) = slot.try_load_consistent().unwrap();
        assert_eq!(v2, 200.0);
        assert_eq!(r2, 2000.0);
        assert_eq!(t2, same_ts);
        assert_eq!(q2, 2);

        // Verify try_load_consistent also works
        slot.set(300.0, 3000.0, same_ts, 3);
        let result = slot.try_load_consistent();
        assert!(result.is_some());
        let (v3, r3, t3, q3) = result.unwrap();
        assert_eq!(v3, 300.0);
        assert_eq!(r3, 3000.0);
        assert_eq!(t3, same_ts);
        assert_eq!(q3, 3);
    }

    #[test]
    fn test_seqlock_quality_code_preserved() {
        let slot = PointSlot::new();

        slot.set(1.0, 1.0, 100, 1);
        slot.set(2.0, 2.0, 200, 4);

        let (v, r, ts, quality_code) = slot.try_load_consistent().unwrap();
        assert_eq!(v, 2.0);
        assert_eq!(r, 2.0);
        assert_eq!(ts, 200);
        assert_eq!(quality_code, 4);
    }

    #[test]
    fn test_seqlock_concurrent_read_write() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;
        use std::thread;

        let slot = Arc::new(PointSlot::new());
        let running = Arc::new(AtomicBool::new(true));
        let write_iterations = 100_000;

        // Pre-seed a value so the slot is not empty before threads start
        slot.set(0.0, 0.0, 42, 0);

        // Writer thread: rapidly writes with same timestamp
        let writer_slot = Arc::clone(&slot);
        let writer_running = Arc::clone(&running);
        let writer = thread::spawn(move || {
            for i in 0..write_iterations {
                let v = i as f64;
                writer_slot.set(v, v * 10.0, 42, i);
                // Yield occasionally so the reader can grab a consistent snapshot
                if i % 1000 == 0 {
                    thread::yield_now();
                }
            }
            writer_running.store(false, Ordering::Relaxed);
        });

        // Reader thread: try_load_consistent to verify seqlock correctness
        // (avoids fallback path which is not seqlock-protected)
        let reader_slot = Arc::clone(&slot);
        let reader_running = Arc::clone(&running);
        let reader = thread::spawn(move || {
            let mut consistent_reads = 0u64;
            // Keep reading while writer is active, then do a final batch
            while reader_running.load(Ordering::Relaxed) {
                if let Some((value, raw, _ts, quality_code)) = reader_slot.try_load_consistent() {
                    assert!(
                        (raw - value * 10.0).abs() < f64::EPSILON || value == 0.0,
                        "Torn read detected: value={value}, raw={raw} (expected raw={})",
                        value * 10.0
                    );
                    assert_eq!(quality_code, value as u32);
                    consistent_reads += 1;
                }
                thread::yield_now();
            }
            // Writer is done — reads should always succeed now
            for _ in 0..100 {
                if let Some((value, raw, _ts, quality_code)) = reader_slot.try_load_consistent() {
                    assert!(
                        (raw - value * 10.0).abs() < f64::EPSILON || value == 0.0,
                        "Torn read detected: value={value}, raw={raw} (expected raw={})",
                        value * 10.0
                    );
                    assert_eq!(quality_code, value as u32);
                    consistent_reads += 1;
                }
            }
            consistent_reads
        });

        writer.join().unwrap();
        let reads = reader.join().unwrap();
        assert!(reads > 0, "No consistent reads achieved");
    }

    // ========== Additional Seqlock Tests (test-expert) ==========

    #[test]
    fn test_seqlock_same_timestamp_rapid_writes() {
        // Hammers the same slot with identical timestamps, validates consistency
        let slot = PointSlot::new();
        let ts = 9999u64;

        for i in 0..1000 {
            let v = i as f64 * 0.01;
            let r = v + 1000.0;
            slot.set(v, r, ts, i);

            let (rv, rr, rt, rq) = slot.try_load_consistent().unwrap();
            assert_eq!(rv, v, "value mismatch at iteration {}", i);
            assert_eq!(rr, r, "raw mismatch at iteration {}", i);
            assert_eq!(rt, ts, "timestamp mismatch at iteration {}", i);
            assert_eq!(rq, i, "quality mismatch at iteration {}", i);
        }
    }

    #[test]
    fn test_seqlock_multi_reader_stress() {
        // Multiple reader threads + 1 writer: verifies the seqlock protocol itself
        // is correct — uses try_load_consistent() which returns None instead of
        // falling back to unprotected reads under extreme contention.
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;
        use std::thread;

        let slot = Arc::new(PointSlot::new());
        let running = Arc::new(AtomicBool::new(true));

        // Writer: encodes i into all fields with a known relationship
        let w_slot = Arc::clone(&slot);
        let w_running = Arc::clone(&running);
        let writer = thread::spawn(move || {
            let mut i = 0u64;
            while w_running.load(Ordering::Relaxed) {
                let v = i as f64;
                w_slot.set(v, v * 10.0, i, i as u32);
                i += 1;
            }
            i
        });

        // 4 readers verify consistency using try_load_consistent (no fallback)
        let readers: Vec<_> = (0..4)
            .map(|_| {
                let r_slot = Arc::clone(&slot);
                let r_running = Arc::clone(&running);
                thread::spawn(move || {
                    let mut torn = 0u64;
                    let mut consistent = 0u64;
                    let mut skipped = 0u64;
                    while r_running.load(Ordering::Relaxed) {
                        // try_load_consistent returns None on contention instead
                        // of falling back to an unprotected read
                        if let Some((value, raw, ts, quality_code)) = r_slot.try_load_consistent() {
                            consistent += 1;
                            // A freshly-created slot is NaN ("unwritten")
                            // before the writer's first set(). That's a legitimate
                            // sentinel, not a torn read.
                            if value.is_nan() {
                                continue;
                            }
                            if value != 0.0 {
                                let raw_ok = (raw - value * 10.0).abs() < f64::EPSILON;
                                let ts_ok = ts == value as u64;
                                let quality_ok = quality_code == value as u32;
                                if !raw_ok || !ts_ok || !quality_ok {
                                    torn += 1;
                                }
                            }
                        } else {
                            skipped += 1;
                        }
                    }
                    (consistent, skipped, torn)
                })
            })
            .collect();

        thread::sleep(std::time::Duration::from_millis(150));
        running.store(false, Ordering::Relaxed);

        let writes = writer.join().unwrap();
        for handle in readers {
            let (consistent, _skipped, torn) = handle.join().unwrap();
            assert_eq!(
                torn, 0,
                "Torn reads detected: {torn}/{consistent} (seqlock protocol bug!)"
            );
        }
        assert!(writes > 100, "Writer did too few iterations: {writes}");
    }

    #[test]
    fn test_point_slot_layout_stability() {
        // The v5 slot ABI is exactly 32 bytes and 32-byte aligned.
        assert_eq!(std::mem::size_of::<PointSlot>(), 32);
        assert_eq!(std::mem::align_of::<PointSlot>(), 32);
    }
}
