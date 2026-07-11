//! False-sharing quantification for the `PointSlot` SHM layout.
//!
//! `PointSlot` is `#[repr(C, align(32))]` = 32 bytes. A 64-byte cache line
//! (Cortex-A55, x86) therefore holds **two** slots; Apple Silicon's 128-byte
//! line holds four. `allocate_layouts` (libs/aether-rtdb-shm/src/layout.rs)
//! packs slots contiguously with no per-writer alignment, so within a channel
//! the last io-owned slot (S) can share a cache line with the first
//! automation-owned slot (C). When io writes T/S on one core while automation
//! writes C/A on another, that shared line ping-pongs between cores via the
//! coherency protocol (MESI/MOESI) — **false sharing** — even though the two
//! writers touch *different* 32-byte slots.
//!
//! This bench isolates the phenomenon with the cleanest possible A/B:
//!
//! * `same_cache_line` — two threads hammer slot[0] and slot[1]
//!   (both in the same 64-byte line) → false sharing.
//! * `separate_cache_line` — two threads hammer slot[0] and slot[2]
//!   (different 64-byte lines) → no false sharing.
//! * `single_thread` — one thread writes slot[0] (contention-free ref).
//!
//! `separate_cache_line` is exactly what a 64-byte-aligned writer boundary in
//! `allocate_layouts` would buy. The `same / separate` ratio is the
//! false-sharing penalty on the host CPU.
//!
//! Run with:
//!   cargo bench -p aether-rtdb-shm --bench false_sharing
//!
//! NOTE: threads are not pinned (no portable affinity API on macOS). On an
//! otherwise-idle box the OS spreads two busy threads across two cores, which
//! is enough to expose the effect. For deterministic A55 numbers, cross-compile
//! and run pinned (taskset -c 0,1).

#![allow(clippy::disallowed_methods)] // benchmark code – unwrap() is fine

use std::hint::black_box;
use std::sync::Barrier;
use std::thread;
use std::time::{Duration, Instant};

use aether_rtdb_shm::{ChannelPointCounts, PointSlot, allocate_layouts};
use criterion::{Criterion, criterion_group, criterion_main};
use std::collections::BTreeMap;

/// 64-byte-aligned home for four `PointSlot`s so the slot→cache-line mapping
/// is deterministic:
///   slot[0] = bytes [0,32)   → line 0
///   slot[1] = bytes [32,64)  → line 0   (shares with slot[0])
///   slot[2] = bytes [64,96)  → line 1
///   slot[3] = bytes [96,128) → line 1
#[repr(C, align(64))]
struct AlignedSlots([PointSlot; 4]);

impl AlignedSlots {
    fn new() -> Self {
        Self([
            PointSlot::new(),
            PointSlot::new(),
            PointSlot::new(),
            PointSlot::new(),
        ])
    }
}

/// Tight write loop: `iters` seqlock writes to one slot.
#[inline]
fn hammer(slot: &PointSlot, iters: u64) {
    for i in 0..iters {
        let v = i as f64;
        black_box(slot).set(black_box(v), black_box(v), black_box(i));
    }
}

/// Time `iters` concurrent writes from two threads, one per slot index.
/// A barrier aligns the start so the loops overlap (real contention window).
fn two_thread_elapsed(slots: &AlignedSlots, idx_a: usize, idx_b: usize, iters: u64) -> Duration {
    let barrier = Barrier::new(2);
    let slot_a = &slots.0[idx_a];
    let slot_b = &slots.0[idx_b];

    thread::scope(|s| {
        let h = s.spawn(|| {
            barrier.wait();
            hammer(slot_b, iters);
        });

        barrier.wait();
        let t0 = Instant::now();
        hammer(slot_a, iters);
        h.join().unwrap();
        t0.elapsed()
    })
}

fn bench_false_sharing(c: &mut Criterion) {
    let mut group = c.benchmark_group("false_sharing");
    group.measurement_time(Duration::from_secs(8));
    group.sample_size(50);

    let slots = AlignedSlots::new();

    // Two threads, slots 0 & 1 — SAME 64-byte cache line → false sharing.
    group.bench_function("same_cache_line", |b| {
        b.iter_custom(|iters| two_thread_elapsed(&slots, 0, 1, iters));
    });

    // Two threads, slots 0 & 2 — SEPARATE cache lines → no false sharing.
    // This is what 64-byte-aligning the io/automation boundary would yield.
    group.bench_function("separate_cache_line", |b| {
        b.iter_custom(|iters| two_thread_elapsed(&slots, 0, 2, iters));
    });

    // Single thread, no contention — reference floor.
    group.bench_function("single_thread", |b| {
        b.iter_custom(|iters| {
            let t0 = Instant::now();
            hammer(&slots.0[0], iters);
            t0.elapsed()
        });
    });

    // End-to-end check of the REAL production layout: resolve the io/
    // automation ownership boundary through allocate_layouts (channel with
    // T=3, C=1) and hammer the last T slot vs the first C slot. With the
    // cache-line padding in allocate_layouts this must match
    // separate_cache_line / single_thread; without it, same_cache_line.
    group.bench_function("layout_boundary", |b| {
        let counts = ChannelPointCounts::from_map(BTreeMap::from([(1u32, [3u32, 0, 1, 0])]));
        let (layouts, slot_count) = allocate_layouts(&counts);
        let t_last = layouts[1].slot(0, 2).unwrap();
        let c_first = layouts[1].slot(2, 0).unwrap();
        assert!(slot_count <= ARENA_SLOTS, "arena too small for layout");

        // 64-byte-aligned arena mirrors the mmap guarantee (slot array
        // starts at file offset 64).
        let arena = AlignedArena::new();
        b.iter_custom(|iters| {
            let barrier = Barrier::new(2);
            let slot_a = &arena.0[t_last];
            let slot_b = &arena.0[c_first];
            thread::scope(|s| {
                let h = s.spawn(|| {
                    barrier.wait();
                    hammer(slot_b, iters);
                });
                barrier.wait();
                let t0 = Instant::now();
                hammer(slot_a, iters);
                h.join().unwrap();
                t0.elapsed()
            })
        });
    });

    group.finish();
}

const ARENA_SLOTS: usize = 16;

/// 64-byte-aligned slot arena sized for the layout_boundary topology.
#[repr(C, align(64))]
struct AlignedArena([PointSlot; ARENA_SLOTS]);

impl AlignedArena {
    fn new() -> Self {
        Self(std::array::from_fn(|_| PointSlot::new()))
    }
}

criterion_group!(benches, bench_false_sharing);
criterion_main!(benches);
