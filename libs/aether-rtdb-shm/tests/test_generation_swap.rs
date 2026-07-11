//! Atomic per-generation file swap mechanism (Step 3 PR 1).
//!
//! Validates the building blocks that PR 2 will assemble into the full
//! io/automation reload protocol:
//!
//! - `generation_file_path` derives staging paths from the canonical path
//! - `UnifiedWriter::create` can be pointed at an arbitrary path via
//!   `SharedConfig::with_path`
//! - `commit_generation_swap` atomically replaces the canonical file with
//!   the staging file
//! - Critical property: **an existing reader holding a mmap of the previous
//!   file continues to see its data** after the swap, while a freshly
//!   opened reader sees the new file. This is what makes the design
//!   tear-free without cross-process coordination — the POSIX inode lives
//!   as long as any mmap references it.

#![allow(clippy::disallowed_methods)]

use aether_rtdb_shm::{
    ChannelPointCounts, SharedConfig, SlotIo, UnifiedReader, UnifiedReaderHandle, UnifiedWriter,
    core::config::{commit_generation_swap, generation_file_path},
};
use std::collections::BTreeMap;
use std::sync::Arc;
use tempfile::tempdir;

fn config_at(path: std::path::PathBuf) -> SharedConfig {
    SharedConfig::default().with_path(path).with_max_slots(256)
}

fn counts_v1() -> ChannelPointCounts {
    let mut map = BTreeMap::new();
    map.insert(100u32, [4u32, 0, 0, 0]); // 4 telemetry points
    ChannelPointCounts::from_map(map)
}

fn counts_v2() -> ChannelPointCounts {
    let mut map = BTreeMap::new();
    map.insert(100u32, [4u32, 0, 0, 0]);
    map.insert(200u32, [2u32, 0, 0, 0]); // adds channel 200 with 2 points
    ChannelPointCounts::from_map(map)
}

/// End-to-end: create gen 1 → open reader R1 → create gen 2 staging →
/// commit swap → R1 still sees gen 1 data (mmap holds inode) → new reader
/// R2 opened at canonical path sees gen 2 layout.
#[test]
fn atomic_swap_preserves_existing_readers() {
    let dir = tempdir().unwrap();
    let canonical = dir.path().join("aether-rtdb.shm");

    // --- Phase 1: create generation 1 at canonical path ---
    let cfg1 = config_at(canonical.clone());
    let cp1 = counts_v1();
    let writer1 = UnifiedWriter::create(&cfg1, &cp1).unwrap();
    let gen1_routing_hash = SlotIo::header(&writer1).routing_hash;

    // Write a sentinel value into channel 100, point 0
    let slot = writer1.lookup(100, 0, 0).expect("lookup gen1 slot");
    writer1.set_direct(slot, 99.9, 0.0, 1_000_000);

    // --- Phase 2: open reader R1 ---
    let reader1 = UnifiedReader::open(&cfg1, &cp1).unwrap();
    let r1_routing_hash = SlotIo::header(&reader1).routing_hash;
    assert_eq!(r1_routing_hash, gen1_routing_hash);

    let (value_before, _ts) = reader1.get_channel(100, 0, 0).expect("R1 reads gen1");
    assert_eq!(value_before, 99.9);

    // --- Phase 3: create generation 2 at staging path ---
    let staging = generation_file_path(&canonical, 2);
    let cfg2 = config_at(staging.clone());
    let cp2 = counts_v2();
    let writer2 = UnifiedWriter::create(&cfg2, &cp2).unwrap();
    let gen2_routing_hash = SlotIo::header(&writer2).routing_hash;

    assert_ne!(
        gen1_routing_hash, gen2_routing_hash,
        "different channel_points must produce different routing_hash"
    );

    // Write a sentinel value into channel 200, point 0 (new channel in gen 2)
    let new_slot = writer2.lookup(200, 0, 0).expect("lookup gen2 slot");
    writer2.set_direct(new_slot, 42.0, 0.0, 2_000_000);

    // Drop writer2 to release the mmap before rename (some platforms need this)
    drop(writer2);

    // --- Phase 4: commit swap (atomic rename) ---
    commit_generation_swap(&staging, &canonical).unwrap();
    assert!(
        !staging.exists(),
        "staging file must be consumed by the rename"
    );
    assert!(canonical.exists(), "canonical must now point at gen 2 file");

    // --- Phase 5: R1 still sees gen 1 data ---
    //
    // This is the critical property. R1's mmap was created on the gen 1
    // inode. After rename, that inode is unlinked from the directory but
    // remains live in memory because R1's mmap holds a reference. R1
    // continues to read gen 1 values without observing the swap at all.
    let r1_routing_hash_after = SlotIo::header(&reader1).routing_hash;
    assert_eq!(
        r1_routing_hash_after, gen1_routing_hash,
        "R1's mmap must still reflect gen 1"
    );
    let (value_after, _ts) = reader1.get_channel(100, 0, 0).expect("R1 still reads gen1");
    assert_eq!(value_after, 99.9);

    // --- Phase 6: a freshly-opened reader sees gen 2 ---
    let reader2 = UnifiedReader::open(&cfg1, &cp2).unwrap();
    let r2_routing_hash = SlotIo::header(&reader2).routing_hash;
    assert_eq!(
        r2_routing_hash, gen2_routing_hash,
        "R2 opened at canonical path must see gen 2 routing"
    );

    let (gen2_value, _ts) = reader2.get_channel(200, 0, 0).expect("R2 reads gen2 chan");
    assert_eq!(gen2_value, 42.0);
}

#[test]
fn published_create_preserves_a_reader_across_writer_restart() {
    let dir = tempdir().unwrap();
    let canonical = dir.path().join("restart-safe.shm");
    let config = config_at(canonical.clone());
    let first_counts = counts_v1();
    let first_writer = UnifiedWriter::create_published(&config, &first_counts).unwrap();
    let slot = first_writer.lookup(100, 0, 0).unwrap();
    first_writer.set_direct(slot, 17.5, 17.5, 1_000);
    let surviving_reader = UnifiedReader::open(&config, &first_counts).unwrap();

    let second_counts = counts_v2();
    let second_writer = UnifiedWriter::create_published(&config, &second_counts).unwrap();

    assert_eq!(
        surviving_reader.get_channel(100, 0, 0).unwrap().0,
        17.5,
        "the surviving process must keep a valid old-generation mmap"
    );
    let fresh_reader = UnifiedReader::open(&config, &second_counts).unwrap();
    assert_eq!(
        SlotIo::header(&fresh_reader).routing_hash,
        second_counts.layout_hash()
    );
    assert_ne!(
        SlotIo::header(&surviving_reader).writer_generation,
        SlotIo::header(&fresh_reader).writer_generation
    );
    drop(second_writer);
}

#[test]
fn reader_handle_switches_future_reads_to_the_new_generation() {
    let dir = tempdir().unwrap();
    let canonical = dir.path().join("reader-handle.shm");
    let config = config_at(canonical);
    let first_counts = counts_v1();
    let first_writer = UnifiedWriter::create_published(&config, &first_counts).unwrap();
    let first_reader = Arc::new(UnifiedReader::open(&config, &first_counts).unwrap());
    let handle = UnifiedReaderHandle::new(first_reader);
    let first_generation = handle.generation();

    let second_counts = counts_v2();
    let second_writer = UnifiedWriter::create_published(&config, &second_counts).unwrap();
    let second_reader = Arc::new(UnifiedReader::open(&config, &second_counts).unwrap());
    handle.replace(second_reader);

    assert_ne!(handle.generation(), first_generation);
    drop((first_writer, second_writer));
}
