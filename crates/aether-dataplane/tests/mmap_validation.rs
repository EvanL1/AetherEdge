use std::fs::OpenOptions;

use aether_dataplane::{
    DataplaneError, HeaderSnapshot, SlotIo, SlotReader, SlotWriter, UNIFIED_MAGIC, UNIFIED_VERSION,
    UnifiedHeader, calculate_file_size,
};
use memmap2::MmapOptions;

/// Frozen view compiled into pre-epoch v4 readers.
///
/// Those readers treated the final eight header bytes as reserved and never
/// interpreted them. Keeping this fixture independent from `UnifiedHeader`
/// makes the rolling-compatibility test fail if a future change moves slots or
/// grows the physical header while still claiming v4 compatibility.
#[repr(C, align(64))]
struct LegacyV4Header {
    magic: u64,
    version: u32,
    max_slots: u32,
    slot_count: std::sync::atomic::AtomicU32,
    _pad: [u8; 4],
    last_update_ts: std::sync::atomic::AtomicU64,
    writer_heartbeat: std::sync::atomic::AtomicU64,
    routing_hash: std::sync::atomic::AtomicU64,
    writer_generation: std::sync::atomic::AtomicU64,
    _reserved: [u8; 8],
}

const _: () = assert!(std::mem::size_of::<LegacyV4Header>() == 64);

#[test]
fn publication_epoch_extension_preserves_legacy_public_header_literals() {
    let header = UnifiedHeader {
        magic: UNIFIED_MAGIC,
        version: UNIFIED_VERSION,
        max_slots: 4,
        slot_count: std::sync::atomic::AtomicU32::new(2),
        _pad: [0; 4],
        last_update_ts: std::sync::atomic::AtomicU64::new(0),
        writer_heartbeat: std::sync::atomic::AtomicU64::new(0),
        routing_hash: std::sync::atomic::AtomicU64::new(99),
        writer_generation: std::sync::atomic::AtomicU64::new(2),
        _reserved: 4_096_u64.to_ne_bytes(),
    };
    let snapshot = HeaderSnapshot {
        magic: UNIFIED_MAGIC,
        version: UNIFIED_VERSION,
        max_slots: 4,
        slot_count: 2,
        last_update_ts: 0,
        writer_heartbeat: 0,
        routing_hash: 99,
        writer_generation: 2,
    };

    assert_eq!(header.publication_epoch(), 4_096);
    assert_eq!(snapshot.routing_hash, 99);
}

#[test]
fn reader_rejects_mapping_that_cannot_cover_declared_slots() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("short-reader.shm");
    let file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&path)
        .expect("create short SHM file");
    file.set_len(64).expect("size short SHM file");

    // SAFETY: The file remains open for the creation call and has a stable,
    // non-zero length. The test never dereferences the mapping directly.
    let mmap = unsafe { MmapOptions::new().map(&file) }.expect("map short SHM file");
    let Err(error) = SlotReader::from_mmap(mmap, 2, 1) else {
        panic!("short mapping must fail");
    };

    assert!(error.to_string().contains("mapping too small"));
    assert!(matches!(error, DataplaneError::InvalidLayout(_)));
    assert_eq!(calculate_file_size(2), 128);
}

#[test]
fn writer_rejects_live_count_larger_than_capacity() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("invalid-count.shm");
    let file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&path)
        .expect("create SHM file");
    file.set_len(calculate_file_size(2) as u64)
        .expect("size SHM file");

    // SAFETY: The writable file has exactly the mapped length and stays alive
    // while the OS creates the mapping. No aliasing mapping exists in the test.
    let mmap = unsafe { MmapOptions::new().map_mut(&file) }.expect("map SHM file");
    let Err(error) = SlotWriter::from_mmap(mmap, path, 2, 3) else {
        panic!("slot_count above max_slots must fail");
    };

    assert!(error.to_string().contains("slot_count"));
    assert!(matches!(error, DataplaneError::InvalidLayout(_)));
}

fn write_shm_image(path: &std::path::Path, magic: u64, value: f64) {
    let mut image = vec![0_u8; calculate_file_size(1)];
    image[0..8].copy_from_slice(&magic.to_ne_bytes());
    image[8..12].copy_from_slice(&UNIFIED_VERSION.to_ne_bytes());
    image[12..16].copy_from_slice(&1_u32.to_ne_bytes());
    image[16..20].copy_from_slice(&1_u32.to_ne_bytes());
    image[32..40].copy_from_slice(&1_000_u64.to_ne_bytes());
    image[40..48].copy_from_slice(&7_u64.to_ne_bytes());
    image[48..56].copy_from_slice(&2_u64.to_ne_bytes());

    let slot_offset = 64;
    image[slot_offset..slot_offset + 8].copy_from_slice(&value.to_bits().to_ne_bytes());
    image[slot_offset + 8..slot_offset + 16].copy_from_slice(&900_u64.to_ne_bytes());
    image[slot_offset + 16..slot_offset + 24].copy_from_slice(&value.to_bits().to_ne_bytes());
    std::fs::write(path, image).expect("write SHM image");
}

#[test]
fn reader_open_validates_and_reads_a_read_only_file() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("valid.shm");
    write_shm_image(&path, UNIFIED_MAGIC, 48.5);

    let reader = SlotReader::open(&path).expect("open valid SHM file");
    let slot = reader.read_slot(0).expect("read first slot");

    assert_eq!(slot.value, 48.5);
    assert_eq!(slot.timestamp_ms, 900);
    assert_eq!(reader.header().routing_hash, 7);
    assert_eq!(reader.generation(), 2);
}

#[test]
fn reader_open_rejects_truncated_file_before_header_cast() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("truncated.shm");
    std::fs::write(&path, [0_u8; 8]).expect("write truncated file");

    let Err(error) = SlotReader::open(&path) else {
        panic!("truncated SHM must fail");
    };

    assert!(matches!(error, DataplaneError::InvalidLayout(_)));
    assert!(error.to_string().contains("header"));
}

#[test]
fn reader_open_rejects_invalid_magic() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("invalid-magic.shm");
    write_shm_image(&path, 0, 48.5);

    let Err(error) = SlotReader::open(&path) else {
        panic!("invalid magic must fail");
    };

    assert!(matches!(error, DataplaneError::InvalidLayout(_)));
    assert!(error.to_string().contains("magic"));
}

#[test]
fn writer_create_publishes_a_valid_readable_segment() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("created.shm");
    let writer = SlotWriter::create(&path, 4, 2, 99).expect("create SHM writer");

    assert_eq!(writer.slot_count(), 2);
    assert_eq!(writer.header().snapshot().routing_hash, 99);
    assert_ne!(writer.generation(), 0);
    assert_eq!(writer.generation() & 1, 0);
    assert!(writer.read_slot(0).expect("unwritten slot").value.is_nan());

    writer.set_direct(1, 1.0, 1.0, 1_000);
    writer.flush().expect("flush SHM writer");

    let reader = SlotReader::open(&path).expect("open created segment read-only");
    assert_eq!(reader.read_slot(1).expect("written slot").value, 1.0);
    assert_eq!(reader.header().routing_hash, 99);
}

#[test]
fn writer_create_at_epoch_exposes_the_cross_plane_publication_identity() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("coordinated.shm");
    let publication_epoch = 4_096;

    let writer = SlotWriter::create_at_epoch(&path, 4, 2, 99, publication_epoch)
        .expect("create coordinated SHM writer");
    assert_eq!(writer.header().publication_epoch(), publication_epoch);

    let reader = SlotReader::open(&path).expect("open coordinated SHM reader");
    assert_eq!(reader.publication_epoch(), publication_epoch);
    assert_eq!(reader.header().routing_hash, 99);
}

#[test]
fn legacy_v4_reader_accepts_new_io_segment_during_io_first_rolling_upgrade() {
    use std::sync::atomic::Ordering;

    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("new-io-old-reader.shm");
    let writer =
        SlotWriter::create_at_epoch(&path, 4, 2, 99, 4_096).expect("create coordinated SHM writer");
    writer.set_direct(1, 37.5, 37.5, 1_234);
    writer.flush().expect("flush coordinated SHM writer");

    let file = OpenOptions::new()
        .read(true)
        .open(&path)
        .expect("open new IO segment as a legacy reader");
    // SAFETY: the file is immutable for the lifetime of this read-only mapping
    // and was created at the exact v4 size by `SlotWriter` above.
    let mmap = unsafe { MmapOptions::new().map(&file) }.expect("map new IO segment");
    // SAFETY: the fixture is 64-byte aligned, the mmap begins at a page-aligned
    // address, and the file is at least one complete frozen v4 header long.
    let header = unsafe { &*mmap.as_ptr().cast::<LegacyV4Header>() };

    assert_eq!(header.magic, UNIFIED_MAGIC);
    assert_eq!(header.version, UNIFIED_VERSION);
    assert_eq!(header.max_slots, 4);
    assert_eq!(header.slot_count.load(Ordering::Acquire), 2);
    assert_eq!(header.routing_hash.load(Ordering::Acquire), 99);
    assert_eq!(header.writer_generation.load(Ordering::Acquire) & 1, 0);

    let slot_offset = std::mem::size_of::<LegacyV4Header>() + std::mem::size_of::<u64>() * 4;
    let value_bits = u64::from_ne_bytes(
        mmap[slot_offset..slot_offset + 8]
            .try_into()
            .expect("one legacy slot value"),
    );
    assert_eq!(f64::from_bits(value_bits), 37.5);
}

#[test]
fn writer_open_existing_validates_manifest_and_shares_the_segment() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("existing.shm");
    let owner = SlotWriter::create(&path, 4, 2, 99).expect("create owner");

    let command_side =
        SlotWriter::open_existing(&path, 2, 99).expect("open validated existing segment");
    command_side.set_direct(1, 7.5, 7.5, 1_001);

    assert_eq!(owner.read_slot(1).expect("shared slot").value, 7.5);
    assert_eq!(command_side.generation(), owner.generation());
}

#[test]
fn writer_open_existing_rejects_stale_slot_count_or_layout_hash() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("stale.shm");
    let _owner = SlotWriter::create(&path, 4, 2, 99).expect("create owner");

    for result in [
        SlotWriter::open_existing(&path, 1, 99),
        SlotWriter::open_existing(&path, 2, 100),
    ] {
        let Err(error) = result else {
            panic!("stale manifest must fail closed");
        };
        assert!(matches!(error, DataplaneError::InvalidLayout(_)));
    }
}
