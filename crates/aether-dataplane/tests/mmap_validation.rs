use std::fs::OpenOptions;

use aether_dataplane::{
    DataplaneError, SlotIo, SlotReader, SlotWriter, UNIFIED_MAGIC, UNIFIED_VERSION,
    calculate_file_size,
};
use memmap2::MmapOptions;

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
