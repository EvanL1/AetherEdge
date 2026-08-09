use std::fs::OpenOptions;
use std::sync::atomic::{AtomicU32, AtomicU64};

use aether_dataplane::{
    AETHER_SHM_MAGIC, DataplaneError, SHM_LAYOUT_VERSION, SNAPSHOT_MAGIC, SNAPSHOT_VERSION,
    ShmHeader, SlotIo, SlotReader, SlotWriter, SnapshotImage, calculate_file_size,
};

#[test]
fn v5_header_has_canonical_identity_and_explicit_fields() {
    let header = ShmHeader {
        magic: AETHER_SHM_MAGIC,
        version: SHM_LAYOUT_VERSION,
        slot_count: AtomicU32::new(2),
        writer_heartbeat: AtomicU64::new(10),
        layout_hash: AtomicU64::new(99),
        writer_generation: AtomicU64::new(2),
        publication_epoch: 4_096,
        _reserved: [0; 16],
    };
    let snapshot = header.snapshot();

    assert_eq!(AETHER_SHM_MAGIC, u64::from_be_bytes(*b"AETHER__"));
    assert_eq!(SHM_LAYOUT_VERSION, 5);
    assert_eq!(std::mem::size_of::<ShmHeader>(), 64);
    assert_eq!(snapshot.layout_hash, 99);
    assert_eq!(snapshot.publication_epoch, 4_096);
}

#[test]
fn reader_rejects_mapping_that_cannot_cover_declared_slots() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("short-reader.shm");
    let writer = SlotWriter::create(&path, 1, 99, 1).expect("create valid SHM file");
    drop(writer);
    OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("open SHM file for truncation")
        .set_len(64)
        .expect("truncate SHM slots");

    let Err(error) = SlotReader::open(&path) else {
        panic!("short mapping must fail");
    };

    assert!(error.to_string().contains("mapping length mismatch"));
    assert!(matches!(error, DataplaneError::InvalidLayout(_)));
    assert_eq!(calculate_file_size(2), 128);
}

fn write_shm_image(path: &std::path::Path, magic: u64, version: u32, value: f64) {
    let mut image = vec![0_u8; calculate_file_size(1)];
    image[0..8].copy_from_slice(&magic.to_ne_bytes());
    image[8..12].copy_from_slice(&version.to_ne_bytes());
    image[12..16].copy_from_slice(&1_u32.to_ne_bytes());
    image[16..24].copy_from_slice(&1_000_u64.to_ne_bytes());
    image[24..32].copy_from_slice(&7_u64.to_ne_bytes());
    image[32..40].copy_from_slice(&2_u64.to_ne_bytes());
    image[40..48].copy_from_slice(&11_u64.to_ne_bytes());

    let slot_offset = 64;
    image[slot_offset..slot_offset + 8].copy_from_slice(&value.to_bits().to_ne_bytes());
    image[slot_offset + 8..slot_offset + 16].copy_from_slice(&900_u64.to_ne_bytes());
    image[slot_offset + 16..slot_offset + 24].copy_from_slice(&value.to_bits().to_ne_bytes());
    image[slot_offset + 28..slot_offset + 32].copy_from_slice(&3_u32.to_ne_bytes());
    std::fs::write(path, image).expect("write SHM image");
}

#[test]
fn reader_open_validates_and_reads_a_v5_file() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("valid.shm");
    write_shm_image(&path, AETHER_SHM_MAGIC, SHM_LAYOUT_VERSION, 48.5);

    let reader = SlotReader::open(&path).expect("open valid SHM file");
    let slot = reader.read_slot(0).expect("read first slot");

    assert_eq!(slot.value, 48.5);
    assert_eq!(slot.timestamp_ms, 900);
    assert_eq!(slot.quality_code, 3);
    assert_eq!(reader.header().layout_hash, 7);
    assert_eq!(reader.publication_epoch(), 11);
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
fn reader_open_rejects_invalid_magic_and_v4_layout() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let invalid_magic = dir.path().join("invalid-magic.shm");
    write_shm_image(&invalid_magic, 0, SHM_LAYOUT_VERSION, 48.5);
    let Err(error) = SlotReader::open(&invalid_magic) else {
        panic!("invalid magic must fail");
    };
    assert!(error.to_string().contains("magic"));

    let v4 = dir.path().join("v4.shm");
    write_shm_image(&v4, AETHER_SHM_MAGIC, 4, 48.5);
    let Err(error) = SlotReader::open(&v4) else {
        panic!("v4 layout must fail");
    };
    assert!(error.to_string().contains("version"));
}

#[test]
fn writer_requires_a_nonzero_publication_epoch() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("uncoordinated.shm");
    let Err(error) = SlotWriter::create(&path, 2, 99, 0) else {
        panic!("zero publication epoch must fail");
    };
    assert!(error.to_string().contains("publication_epoch"));
}

#[test]
fn writer_create_publishes_quality_without_refreshing_heartbeat() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("created.shm");
    let writer = SlotWriter::create(&path, 2, 99, 4_096).expect("create SHM writer");

    assert_eq!(writer.slot_count(), 2);
    assert_eq!(
        std::fs::metadata(&path).expect("stat exact SHM").len(),
        calculate_file_size(2) as u64
    );
    assert_eq!(writer.header().layout_hash, 99);
    assert_eq!(writer.header().publication_epoch, 4_096);
    assert_ne!(writer.generation(), 0);
    assert_eq!(writer.generation() & 1, 0);
    assert!(writer.read_slot(0).expect("unwritten slot").value.is_nan());

    writer.set_direct(1, 1.0, 1.5, 1_000, 2);
    assert_eq!(writer.writer_heartbeat(), 0);
    writer.update_heartbeat(1_001);
    writer.flush().expect("flush SHM writer");

    let reader = SlotReader::open(&path).expect("open created segment read-only");
    let slot = reader.read_slot(1).expect("written slot");
    assert_eq!(slot.value, 1.0);
    assert_eq!(slot.raw, 1.5);
    assert_eq!(slot.quality_code, 2);
    assert_eq!(reader.writer_heartbeat(), 1_001);
    assert_eq!(reader.header().layout_hash, 99);
}

#[test]
fn snapshot_round_trip_is_compact_and_rejects_live_mmap_images() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let shm_path = dir.path().join("live.shm");
    let snapshot_path = dir.path().join("live.snapshot");
    let writer = SlotWriter::create(&shm_path, 2, 99, 8).expect("create SHM writer");
    writer.set_direct(1, 7.5, 75.0, 1_234, 3);
    writer
        .save_snapshot(&snapshot_path)
        .expect("save v5 snapshot");

    let image = SnapshotImage::load(&snapshot_path).expect("load v5 snapshot");
    assert_eq!(image.header().magic, SNAPSHOT_MAGIC);
    assert_eq!(image.header().version, SNAPSHOT_VERSION);
    assert_eq!(image.header().slot_count, 2);
    assert_eq!(image.header().layout_hash, 99);
    let slot = image.slots()[1].expect("saved slot");
    assert_eq!(slot.value, 7.5);
    assert_eq!(slot.raw, 75.0);
    assert_eq!(slot.timestamp_ms, 1_234);
    assert_eq!(slot.quality_code, 3);
    assert_eq!(
        std::fs::metadata(&snapshot_path)
            .expect("stat compact snapshot")
            .len(),
        54
    );
    assert!(!snapshot_path.with_extension("snapshot.tmp").exists());

    let invalid_version_path = dir.path().join("invalid-version.snapshot");
    let mut invalid_version = std::fs::read(&snapshot_path).expect("read compact snapshot");
    invalid_version[8..12].copy_from_slice(&(SNAPSHOT_VERSION + 1).to_le_bytes());
    std::fs::write(&invalid_version_path, invalid_version).expect("write invalid snapshot version");
    let Err(error) = SnapshotImage::load(&invalid_version_path) else {
        panic!("unknown snapshot version must fail");
    };
    assert!(error.to_string().contains("version"));

    let live_image_path = dir.path().join("live-header.snapshot");
    std::fs::copy(&shm_path, &live_image_path).expect("copy live mmap as invalid snapshot");
    let Err(error) = SnapshotImage::load(&live_image_path) else {
        panic!("live mmap image must not decode as a snapshot");
    };
    assert!(error.to_string().contains("magic"));
}

#[test]
fn writer_open_existing_validates_manifest_and_shares_the_segment() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("existing.shm");
    let owner = SlotWriter::create(&path, 2, 99, 10).expect("create owner");

    let command_side =
        SlotWriter::open_existing(&path, 2, 99).expect("open validated existing segment");
    command_side.set_direct(1, 7.5, 7.5, 1_001, 1);

    let slot = owner.read_slot(1).expect("shared slot");
    assert_eq!(slot.value, 7.5);
    assert_eq!(slot.quality_code, 1);
    assert_eq!(command_side.generation(), owner.generation());
}

#[test]
fn writer_open_existing_rejects_stale_slot_count_or_layout_hash() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("stale.shm");
    let _owner = SlotWriter::create(&path, 2, 99, 12).expect("create owner");

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
