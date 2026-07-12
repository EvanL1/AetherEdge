use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use aether_dataplane::{AuthorityWriteGuard, SlotIo, SlotWriter};
use aether_domain::{
    AcquiredPointSample, ChannelId, ChannelPointAddress, PointId, PointKind, PointQuality,
    TimestampMs,
};
use aether_ports::{AcquisitionStateWriter, PortErrorKind};
use aether_shm_bridge::{
    AcquisitionCommitObserver, ChannelPointManifest, ShmAcquisitionStateWriter,
};

fn sample(
    channel_id: u32,
    kind: PointKind,
    point_id: u32,
    value: f64,
    raw: f64,
    timestamp_ms: u64,
) -> AcquiredPointSample {
    let address =
        ChannelPointAddress::new(ChannelId::new(channel_id), kind, PointId::new(point_id))
            .expect("contract fixtures use acquisition-owned point kinds");
    AcquiredPointSample::new(
        address,
        value,
        raw,
        TimestampMs::new(timestamp_ms),
        PointQuality::Good,
    )
    .expect("contract fixtures use finite values")
}

fn fixture(
    manifest: ChannelPointManifest,
    writer_layout_hash: u64,
) -> (
    tempfile::TempDir,
    Arc<SlotWriter>,
    ShmAcquisitionStateWriter,
) {
    let directory = tempfile::tempdir().expect("create test SHM directory");
    let writer = Arc::new(
        SlotWriter::create(
            directory.path().join("acquisition.shm"),
            16,
            manifest.slot_count(),
            writer_layout_hash,
        )
        .expect("create test slot writer"),
    );
    let adapter = ShmAcquisitionStateWriter::new(Arc::clone(&writer), Arc::new(manifest));
    (directory, writer, adapter)
}

#[tokio::test]
async fn writes_validated_telemetry_and_status_with_source_fields() {
    let manifest = ChannelPointManifest::from_entries([(7, [2, 1, 1, 1])]);
    let layout_hash = manifest.layout_hash();
    let (_directory, writer, adapter) = fixture(manifest, layout_hash);
    let samples = [
        sample(7, PointKind::Telemetry, 1, 42.5, 4_250.0, 1_001),
        sample(7, PointKind::Status, 0, 1.0, 0.01, 1_002),
    ];

    let written = adapter
        .write_batch(&samples)
        .await
        .expect("valid batch must be committed");

    assert_eq!(written, 2);
    let telemetry = writer.read_slot(1).expect("telemetry slot");
    assert_eq!(telemetry.value, 42.5);
    assert_eq!(telemetry.raw, 4_250.0);
    assert_eq!(telemetry.timestamp_ms, 1_001);
    let status = writer.read_slot(2).expect("status slot");
    assert_eq!(status.value, 1.0);
    assert_eq!(status.raw, 0.01);
    assert_eq!(status.timestamp_ms, 1_002);
}

#[tokio::test]
async fn unknown_address_rejects_the_whole_batch_before_any_write() {
    let manifest = ChannelPointManifest::from_entries([(7, [1, 0, 0, 0])]);
    let layout_hash = manifest.layout_hash();
    let (_directory, writer, adapter) = fixture(manifest, layout_hash);
    let samples = [
        sample(7, PointKind::Telemetry, 0, 11.0, 110.0, 2_001),
        sample(7, PointKind::Telemetry, 1, 22.0, 220.0, 2_002),
    ];

    let error = adapter
        .write_batch(&samples)
        .await
        .expect_err("unknown address must reject the batch");

    assert_eq!(error.kind(), PortErrorKind::NotFound);
    assert!(writer.read_slot(0).expect("known slot").value.is_nan());
    assert!(adapter.take_dirty_slots().is_empty());
}

#[tokio::test]
async fn duplicate_address_rejects_the_whole_batch_before_any_write() {
    let manifest = ChannelPointManifest::from_entries([(7, [1, 0, 0, 0])]);
    let layout_hash = manifest.layout_hash();
    let (_directory, writer, adapter) = fixture(manifest, layout_hash);
    let samples = [
        sample(7, PointKind::Telemetry, 0, 11.0, 110.0, 3_001),
        sample(7, PointKind::Telemetry, 0, 22.0, 220.0, 3_002),
    ];

    let error = adapter
        .write_batch(&samples)
        .await
        .expect_err("duplicate address must reject the batch");

    assert_eq!(error.kind(), PortErrorKind::InvalidData);
    assert!(writer.read_slot(0).expect("known slot").value.is_nan());
    assert!(adapter.take_dirty_slots().is_empty());
}

#[tokio::test]
async fn manifest_mismatch_rejects_the_whole_batch_before_any_write() {
    let manifest = ChannelPointManifest::from_entries([(7, [1, 0, 0, 0])]);
    let wrong_layout_hash = manifest.layout_hash() ^ 1;
    let (_directory, writer, adapter) = fixture(manifest, wrong_layout_hash);
    let samples = [sample(7, PointKind::Telemetry, 0, 11.0, 110.0, 4_001)];

    let error = adapter
        .write_batch(&samples)
        .await
        .expect_err("manifest mismatch must reject the batch");

    assert_eq!(error.kind(), PortErrorKind::Conflict);
    assert!(writer.read_slot(0).expect("known slot").value.is_nan());
    assert!(adapter.take_dirty_slots().is_empty());
}

#[tokio::test]
async fn exposes_only_narrow_writer_lifecycle_operations() {
    let manifest = ChannelPointManifest::from_entries([(7, [1, 0, 0, 0])]);
    let layout_hash = manifest.layout_hash();
    let (directory, _writer, adapter) = fixture(manifest, layout_hash);

    adapter.update_heartbeat(9_001);
    assert_eq!(adapter.writer_heartbeat(), 9_001);
    assert_eq!(adapter.write_batch(&[]).await.expect("empty batch"), 0);

    adapter
        .write_batch(&[sample(7, PointKind::Telemetry, 0, 11.0, 110.0, 9_002)])
        .await
        .expect("write one sample");
    assert_eq!(adapter.take_dirty_slots(), vec![0]);

    let snapshot_path = directory.path().join("acquisition.snapshot");
    adapter
        .save_snapshot(&snapshot_path)
        .expect("save acquisition snapshot");
    assert!(snapshot_path.is_file());
}

#[test]
fn domain_address_rejects_command_owned_kinds_before_the_writer_boundary() {
    for kind in [PointKind::Command, PointKind::Action] {
        assert!(
            ChannelPointAddress::new(ChannelId::new(7), kind, PointId::new(0)).is_err(),
            "{kind:?} must be unrepresentable as an acquired sample"
        );
    }
}

struct ReplaceCanonicalBeforeConfirmation {
    staging: PathBuf,
    canonical: PathBuf,
}

impl AcquisitionCommitObserver for ReplaceCanonicalBeforeConfirmation {
    fn before_authority_confirmation(&self) {
        std::fs::rename(&self.staging, &self.canonical)
            .expect("atomically replace canonical acquisition SHM");
    }

    fn point_committed(&self, _slot: usize, _sample: AcquiredPointSample) {}
}

#[cfg(unix)]
#[tokio::test]
async fn canonical_inode_replacement_during_batch_fails_closed() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let canonical = directory.path().join("authoritative.shm");
    let staging = directory.path().join("replacement.shm");
    let manifest = Arc::new(ChannelPointManifest::from_entries([(7, [1, 0, 0, 0])]));
    let old_writer = Arc::new(
        SlotWriter::create(
            &canonical,
            16,
            manifest.slot_count(),
            manifest.layout_hash(),
        )
        .expect("create old canonical generation"),
    );
    let replacement =
        SlotWriter::create(&staging, 16, manifest.slot_count(), manifest.layout_hash())
            .expect("create replacement generation");
    let adapter = ShmAcquisitionStateWriter::new(Arc::clone(&old_writer), manifest).with_observer(
        Arc::new(ReplaceCanonicalBeforeConfirmation {
            staging,
            canonical: canonical.clone(),
        }),
    );

    let error = adapter
        .write_batch(&[sample(7, PointKind::Telemetry, 0, 11.0, 110.0, 9_003)])
        .await
        .expect_err("a batch overlapping canonical replacement must fail closed");

    assert_eq!(error.kind(), PortErrorKind::Conflict);
    let canonical_reader = SlotWriter::open_existing(
        &canonical,
        replacement.slot_count(),
        replacement.header().snapshot().routing_hash,
    )
    .expect("open replacement through canonical path");
    assert!(
        canonical_reader
            .read_slot(0)
            .expect("replacement slot")
            .value
            .is_nan(),
        "a failed stale batch must not mutate the replacement authority"
    );
}

struct AssertReplacementExcluded {
    canonical: PathBuf,
    local_gate: Arc<RwLock<()>>,
}

impl AcquisitionCommitObserver for AssertReplacementExcluded {
    fn before_authority_confirmation(&self) {
        assert!(
            self.local_gate.try_write().is_err(),
            "the local replacement gate must remain read-locked through confirmation"
        );
        assert!(
            AuthorityWriteGuard::try_acquire(&self.canonical)
                .expect("try cross-process replacement lease")
                .is_none(),
            "canonical replacement must remain excluded through confirmation"
        );
    }

    fn point_committed(&self, _slot: usize, _sample: AcquiredPointSample) {}
}

#[tokio::test]
async fn acquisition_commit_holds_local_and_cross_process_authority_leases() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let canonical = directory.path().join("linearized-acquisition.shm");
    let manifest = Arc::new(ChannelPointManifest::from_entries([(7, [1, 0, 0, 0])]));
    let writer = Arc::new(
        SlotWriter::create(
            &canonical,
            16,
            manifest.slot_count(),
            manifest.layout_hash(),
        )
        .expect("create canonical generation"),
    );
    let local_gate = Arc::new(RwLock::new(()));
    let adapter = ShmAcquisitionStateWriter::new(writer, manifest)
        .with_local_authority_gate(Arc::clone(&local_gate))
        .with_observer(Arc::new(AssertReplacementExcluded {
            canonical,
            local_gate,
        }));

    assert_eq!(
        adapter
            .write_batch(&[sample(7, PointKind::Telemetry, 0, 19.0, 190.0, 9_004)])
            .await
            .expect("linearized acquisition commit"),
        1
    );
}
