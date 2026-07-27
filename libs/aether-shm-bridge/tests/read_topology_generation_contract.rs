use std::sync::Arc;

use aether_ports::PortErrorKind;
use aether_shm_bridge::{
    ChannelHealthManifest, ChannelPointManifest, ShmChannelHealthWriterHandle, ShmClientConfig,
    ShmReadTopologyGeneration, ShmReadTopologyHandle, ShmRuntimeConfig, ShmWriterHandle,
    SlotSource, begin_topology_publication, commit_topology_publication,
    topology_commit_path_from_shm,
};

fn point_manifest(entries: &[(u32, [u32; 4])]) -> Arc<ChannelPointManifest> {
    Arc::new(ChannelPointManifest::from_entries(entries.iter().copied()))
}

fn health_manifest(channel_ids: &[u32]) -> Arc<ChannelHealthManifest> {
    Arc::new(ChannelHealthManifest::from_channel_ids(
        channel_ids.iter().copied(),
    ))
}

#[test]
fn validated_reader_generation_opens_only_when_both_planes_match() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let point_path = directory.path().join("live.shm");
    let health_path = directory.path().join("health.shm");
    let points = point_manifest(&[(7, [1, 1, 0, 0])]);
    let health = health_manifest(&[7]);

    let publication_epoch = 2;
    let point_writer = ShmWriterHandle::create_published_at_epoch(
        ShmRuntimeConfig::new(&point_path, 32),
        Arc::clone(&points),
        None,
        publication_epoch,
    )
    .expect("publish point generation");
    let health_writer = ShmChannelHealthWriterHandle::create_at_epoch(
        &health_path,
        Arc::clone(&health),
        publication_epoch,
    )
    .expect("publish health generation");
    commit_topology_publication(&point_path, &health_path, publication_epoch)
        .expect("commit topology publication");
    let now_ms = aether_shm_bridge::timestamp_ms();
    point_writer
        .generation()
        .expect("point generation")
        .acquisition_writer()
        .update_heartbeat(now_ms);
    health_writer
        .update_heartbeat(now_ms)
        .expect("publish health heartbeat");

    let generation = ShmReadTopologyGeneration::open(
        ShmClientConfig::new(&point_path, points.layout_hash()),
        ShmClientConfig::new(&health_path, health.layout_hash()),
        Arc::clone(&points),
        Arc::clone(&health),
    )
    .expect("open coherent read generation");

    assert_eq!(
        generation.point_manifest().layout_hash(),
        points.layout_hash()
    );
    assert_eq!(
        generation.health_manifest().layout_hash(),
        health.layout_hash()
    );
    assert_eq!(generation.point_source().slot_count().unwrap(), 2);
    assert!(
        generation
            .channel_health()
            .read_channel(7)
            .unwrap()
            .is_none()
    );
}

#[test]
fn publication_guard_allocates_after_durable_and_partial_plane_epochs() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let point_path = directory.path().join("live.shm");
    let health_path = directory.path().join("health.shm");
    let points = point_manifest(&[(7, [1, 0, 0, 0])]);
    let health = health_manifest(&[7]);

    let point_writer = ShmWriterHandle::create_published_at_epoch(
        ShmRuntimeConfig::new(&point_path, 32),
        Arc::clone(&points),
        None,
        900,
    )
    .expect("publish initial point generation");
    let _health_writer =
        ShmChannelHealthWriterHandle::create_at_epoch(&health_path, Arc::clone(&health), 900)
            .expect("publish initial health generation");
    commit_topology_publication(&point_path, &health_path, 900).expect("commit initial topology");

    point_writer
        .rebuild_for_publication(points, 950)
        .expect("fault-inject a newer partial point publication");

    let mut publication =
        begin_topology_publication(&point_path).expect("acquire publication authority");
    let next_epoch = publication
        .next_publication_epoch(&health_path)
        .expect("allocate durable next epoch");

    assert_eq!(next_epoch, 951);
}

#[test]
fn durable_publication_epoch_cannot_be_reused() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let point_path = directory.path().join("live.shm");
    let health_path = directory.path().join("health.shm");
    let points = point_manifest(&[(7, [1, 0, 0, 0])]);
    let health = health_manifest(&[7]);

    let point_writer = ShmWriterHandle::create_published_at_epoch(
        ShmRuntimeConfig::new(&point_path, 32),
        Arc::clone(&points),
        None,
        1_000,
    )
    .expect("publish initial point generation");
    let health_writer =
        ShmChannelHealthWriterHandle::create_at_epoch(&health_path, Arc::clone(&health), 1_000)
            .expect("publish initial health generation");
    commit_topology_publication(&point_path, &health_path, 1_000).expect("commit initial topology");

    drop(point_writer);
    drop(health_writer);
    let _restarted_point = ShmWriterHandle::create_published_at_epoch(
        ShmRuntimeConfig::new(&point_path, 32),
        points,
        None,
        1_000,
    )
    .expect("fault-inject a restarted point writer reusing the epoch");
    let _restarted_health =
        ShmChannelHealthWriterHandle::create_at_epoch(&health_path, health, 1_000)
            .expect("fault-inject a restarted health writer reusing the epoch");

    let error = commit_topology_publication(&point_path, &health_path, 1_000)
        .expect_err("the durable epoch must never be committed twice");
    assert_eq!(error.kind(), PortErrorKind::Conflict);
    assert!(error.to_string().contains("reuse"));
}

#[test]
fn durable_publication_epoch_cannot_move_backwards() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let point_path = directory.path().join("live.shm");
    let health_path = directory.path().join("health.shm");
    let points = point_manifest(&[(7, [1, 0, 0, 0])]);
    let health = health_manifest(&[7]);

    let point_writer = ShmWriterHandle::create_published_at_epoch(
        ShmRuntimeConfig::new(&point_path, 32),
        Arc::clone(&points),
        None,
        1_000,
    )
    .expect("publish initial point generation");
    let health_writer =
        ShmChannelHealthWriterHandle::create_at_epoch(&health_path, Arc::clone(&health), 1_000)
            .expect("publish initial health generation");
    commit_topology_publication(&point_path, &health_path, 1_000).expect("commit initial topology");
    drop(point_writer);
    drop(health_writer);

    let _rolled_back_point = ShmWriterHandle::create_published_at_epoch(
        ShmRuntimeConfig::new(&point_path, 32),
        points,
        None,
        900,
    )
    .expect("fault-inject a lower point epoch");
    let _rolled_back_health =
        ShmChannelHealthWriterHandle::create_at_epoch(&health_path, health, 900)
            .expect("fault-inject a lower health epoch");

    let error = commit_topology_publication(&point_path, &health_path, 900)
        .expect_err("the durable epoch must be monotonic");
    assert_eq!(error.kind(), PortErrorKind::Conflict);
    assert!(error.to_string().contains("roll back"));
}

#[test]
fn publication_epoch_exhaustion_and_aliased_planes_fail_before_commit() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let point_path = directory.path().join("live.shm");
    let health_path = directory.path().join("health.shm");
    let points = point_manifest(&[(7, [1, 0, 0, 0])]);
    let health = health_manifest(&[7]);
    let _point_writer = ShmWriterHandle::create_published_at_epoch(
        ShmRuntimeConfig::new(&point_path, 32),
        points,
        None,
        u64::MAX - 1,
    )
    .expect("publish final allocatable point epoch");
    let _health_writer =
        ShmChannelHealthWriterHandle::create_at_epoch(&health_path, health, u64::MAX - 1)
            .expect("publish final allocatable health epoch");

    let mut publication =
        begin_topology_publication(&point_path).expect("acquire publication authority");
    let exhausted = publication
        .next_publication_epoch(&health_path)
        .expect_err("reserved maximum epoch must not be allocated");
    assert_eq!(exhausted.kind(), PortErrorKind::InvalidData);
    drop(publication);

    let aliased = commit_topology_publication(&point_path, &point_path, u64::MAX - 1)
        .expect_err("one file cannot witness both SHM planes");
    assert_eq!(aliased.kind(), PortErrorKind::InvalidData);
}

#[test]
fn partial_dual_plane_publication_is_never_accepted_as_a_reader_generation() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let point_path = directory.path().join("live.shm");
    let health_path = directory.path().join("health.shm");
    let old_points = point_manifest(&[(7, [1, 0, 0, 0])]);
    let old_health = health_manifest(&[7]);
    let new_points = point_manifest(&[(7, [1, 0, 0, 0]), (9, [1, 0, 0, 0])]);
    let new_health = health_manifest(&[7, 9]);

    let point_writer = ShmWriterHandle::create_published_at_epoch(
        ShmRuntimeConfig::new(&point_path, 32),
        old_points,
        None,
        10,
    )
    .expect("publish old point generation");
    let _health_writer =
        ShmChannelHealthWriterHandle::create_at_epoch(&health_path, old_health, 10)
            .expect("publish old health generation");
    commit_topology_publication(&point_path, &health_path, 10).expect("commit old topology");
    point_writer
        .rebuild_for_publication(Arc::clone(&new_points), 12)
        .expect("publish only the new point plane");

    let error = ShmReadTopologyGeneration::open(
        ShmClientConfig::new(&point_path, new_points.layout_hash()),
        ShmClientConfig::new(&health_path, new_health.layout_hash()),
        new_points,
        new_health,
    )
    .expect_err("mixed point/health generations must fail closed");

    assert_eq!(error.kind(), PortErrorKind::Conflict);
    assert!(error.is_retryable());
}

#[test]
fn handle_retains_its_previous_generation_when_candidate_validation_fails() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let point_path = directory.path().join("live.shm");
    let health_path = directory.path().join("health.shm");
    let old_points = point_manifest(&[(7, [1, 0, 0, 0])]);
    let old_health = health_manifest(&[7]);
    let new_points = point_manifest(&[(7, [1, 0, 0, 0]), (9, [1, 0, 0, 0])]);
    let new_health = health_manifest(&[7, 9]);
    let point_writer = ShmWriterHandle::create_published_at_epoch(
        ShmRuntimeConfig::new(&point_path, 32),
        Arc::clone(&old_points),
        None,
        20,
    )
    .expect("publish old point generation");
    let _health_writer =
        ShmChannelHealthWriterHandle::create_at_epoch(&health_path, Arc::clone(&old_health), 20)
            .expect("publish old health generation");
    commit_topology_publication(&point_path, &health_path, 20).expect("commit old topology");
    let initial = Arc::new(
        ShmReadTopologyGeneration::open(
            ShmClientConfig::new(&point_path, old_points.layout_hash()),
            ShmClientConfig::new(&health_path, old_health.layout_hash()),
            Arc::clone(&old_points),
            Arc::clone(&old_health),
        )
        .expect("open initial topology"),
    );
    let handle = ShmReadTopologyHandle::new(initial);

    point_writer
        .rebuild_for_publication(Arc::clone(&new_points), 22)
        .expect("publish only the new point plane");
    let candidate = Arc::new(
        ShmReadTopologyGeneration::new_lazy(
            ShmClientConfig::new(&point_path, new_points.layout_hash()),
            ShmClientConfig::new(&health_path, new_health.layout_hash()),
            new_points,
            new_health,
        )
        .expect("compose replacement candidate"),
    );

    let error = handle
        .publish(candidate)
        .expect_err("partial physical publication must not advance the handle");

    assert_eq!(error.kind(), PortErrorKind::Conflict);
    assert_eq!(
        handle.load().point_manifest().layout_hash(),
        old_points.layout_hash()
    );
    assert_eq!(
        handle.load().health_manifest().layout_hash(),
        old_health.layout_hash()
    );
}

#[test]
fn composition_hash_mismatch_is_permanent_but_physical_lag_is_retryable() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let point_path = directory.path().join("live.shm");
    let health_path = directory.path().join("health.shm");
    let points = point_manifest(&[(7, [1, 0, 0, 0])]);
    let health = health_manifest(&[7]);

    let error = ShmReadTopologyGeneration::new_lazy(
        ShmClientConfig::new(&point_path, points.layout_hash().wrapping_add(1)),
        ShmClientConfig::new(&health_path, health.layout_hash()),
        points,
        health,
    )
    .expect_err("composition-provided config and manifest must agree");

    assert_eq!(error.kind(), PortErrorKind::InvalidData);
    assert!(!error.is_retryable());
}

#[test]
fn lazy_reader_generation_keeps_service_startup_independent_from_io() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let point_path = directory.path().join("missing-live.shm");
    let health_path = directory.path().join("missing-health.shm");
    let points = point_manifest(&[(7, [1, 0, 0, 0])]);
    let health = health_manifest(&[7]);

    let generation = ShmReadTopologyGeneration::new_lazy(
        ShmClientConfig::new(&point_path, points.layout_hash()),
        ShmClientConfig::new(&health_path, health.layout_hash()),
        points,
        health,
    )
    .expect("compose lazy generation");

    let error = generation
        .point_source()
        .slot_count()
        .expect_err("missing io writer is a retryable read-time condition");
    assert_eq!(error.kind(), PortErrorKind::Unavailable);
    assert!(error.is_retryable());
}

#[test]
fn coordinated_reader_requires_a_committed_common_publication_epoch() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let point_path = directory.path().join("live.shm");
    let health_path = directory.path().join("health.shm");
    let points = point_manifest(&[(7, [1, 0, 0, 0])]);
    let health = health_manifest(&[7]);
    let publication_epoch = 42;

    let _point_writer = ShmWriterHandle::create_published_at_epoch(
        ShmRuntimeConfig::new(&point_path, 32),
        Arc::clone(&points),
        None,
        publication_epoch,
    )
    .expect("publish coordinated point generation");
    let _health_writer = ShmChannelHealthWriterHandle::create_at_epoch(
        &health_path,
        Arc::clone(&health),
        publication_epoch,
    )
    .expect("publish coordinated health generation");

    let uncommitted = ShmReadTopologyGeneration::open(
        ShmClientConfig::new(&point_path, points.layout_hash()),
        ShmClientConfig::new(&health_path, health.layout_hash()),
        Arc::clone(&points),
        Arc::clone(&health),
    )
    .expect_err("matching files without a commit witness must fail closed");
    assert_eq!(uncommitted.kind(), PortErrorKind::Conflict);
    assert!(uncommitted.is_retryable());

    commit_topology_publication(&point_path, &health_path, publication_epoch)
        .expect("commit coordinated topology");
    let committed = ShmReadTopologyGeneration::open(
        ShmClientConfig::new(&point_path, points.layout_hash()),
        ShmClientConfig::new(&health_path, health.layout_hash()),
        points,
        health,
    )
    .expect("open committed coordinated topology");
    assert_eq!(committed.publication_epoch(), publication_epoch);
}

#[test]
fn matching_manifests_from_different_publication_epochs_are_rejected() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let point_path = directory.path().join("live.shm");
    let health_path = directory.path().join("health.shm");
    let points = point_manifest(&[(7, [1, 0, 0, 0])]);
    let health = health_manifest(&[7]);

    let _point_writer = ShmWriterHandle::create_published_at_epoch(
        ShmRuntimeConfig::new(&point_path, 32),
        Arc::clone(&points),
        None,
        100,
    )
    .expect("publish point epoch");
    let _health_writer =
        ShmChannelHealthWriterHandle::create_at_epoch(&health_path, Arc::clone(&health), 102)
            .expect("publish different health epoch");

    let commit_error = commit_topology_publication(&point_path, &health_path, 102)
        .expect_err("different plane epochs must not be committed");
    assert_eq!(commit_error.kind(), PortErrorKind::Conflict);

    let read_error = ShmReadTopologyGeneration::open(
        ShmClientConfig::new(&point_path, points.layout_hash()),
        ShmClientConfig::new(&health_path, health.layout_hash()),
        points,
        health,
    )
    .expect_err("equal manifests do not prove a common publication");
    assert_eq!(read_error.kind(), PortErrorKind::Conflict);
    assert!(read_error.is_retryable());
}

#[test]
fn stale_commit_witness_is_rejected_after_one_plane_is_republished() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let point_path = directory.path().join("live.shm");
    let health_path = directory.path().join("health.shm");
    let points = point_manifest(&[(7, [1, 0, 0, 0])]);
    let health = health_manifest(&[7]);
    let committed_epoch = 200;

    let point_writer = ShmWriterHandle::create_published_at_epoch(
        ShmRuntimeConfig::new(&point_path, 32),
        Arc::clone(&points),
        None,
        committed_epoch,
    )
    .expect("publish point generation");
    let _health_writer = ShmChannelHealthWriterHandle::create_at_epoch(
        &health_path,
        Arc::clone(&health),
        committed_epoch,
    )
    .expect("publish health generation");
    commit_topology_publication(&point_path, &health_path, committed_epoch)
        .expect("commit initial topology");

    point_writer
        .rebuild_for_publication(Arc::clone(&points), 202)
        .expect("republish only point plane");
    let error = ShmReadTopologyGeneration::open(
        ShmClientConfig::new(&point_path, points.layout_hash()),
        ShmClientConfig::new(&health_path, health.layout_hash()),
        points,
        health,
    )
    .expect_err("old witness must not authorize a republished point plane");

    assert_eq!(error.kind(), PortErrorKind::Conflict);
    assert!(error.is_retryable());
}

#[test]
fn lazy_generation_cannot_read_a_point_plane_before_dual_plane_commit() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let point_path = directory.path().join("live.shm");
    let health_path = directory.path().join("health.shm");
    let points = point_manifest(&[(7, [1, 0, 0, 0])]);
    let health = health_manifest(&[7]);
    let generation = ShmReadTopologyGeneration::new_lazy(
        ShmClientConfig::new(&point_path, points.layout_hash()),
        ShmClientConfig::new(&health_path, health.layout_hash()),
        Arc::clone(&points),
        health,
    )
    .expect("compose lazy generation");
    let point_writer = ShmWriterHandle::create_published_at_epoch(
        ShmRuntimeConfig::new(&point_path, 32),
        points,
        None,
        300,
    )
    .expect("publish point only");
    point_writer
        .generation()
        .expect("point generation")
        .acquisition_writer()
        .update_heartbeat(aether_shm_bridge::timestamp_ms());

    let error = generation
        .point_source()
        .slot_count()
        .expect_err("uncommitted point plane must stay unreadable");
    assert_eq!(error.kind(), PortErrorKind::Conflict);
    assert!(error.is_retryable());
}

#[test]
fn retained_generation_cannot_reconnect_across_a_same_layout_epoch_change() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let point_path = directory.path().join("live.shm");
    let health_path = directory.path().join("health.shm");
    let points = point_manifest(&[(7, [1, 0, 0, 0])]);
    let health = health_manifest(&[7]);
    let point_writer = ShmWriterHandle::create_published_at_epoch(
        ShmRuntimeConfig::new(&point_path, 32),
        Arc::clone(&points),
        None,
        400,
    )
    .expect("publish initial point generation");
    let health_writer =
        ShmChannelHealthWriterHandle::create_at_epoch(&health_path, Arc::clone(&health), 400)
            .expect("publish initial health generation");
    commit_topology_publication(&point_path, &health_path, 400).expect("commit initial topology");
    let generation = ShmReadTopologyGeneration::open(
        ShmClientConfig::new(&point_path, points.layout_hash())
            .with_identity_check_interval(std::time::Duration::ZERO),
        ShmClientConfig::new(&health_path, health.layout_hash())
            .with_identity_check_interval(std::time::Duration::ZERO),
        Arc::clone(&points),
        Arc::clone(&health),
    )
    .expect("open initial topology");

    point_writer
        .rebuild_for_publication(Arc::clone(&points), 402)
        .expect("republish same point manifest");
    health_writer
        .rebuild_for_publication(health, 402)
        .expect("republish same health manifest");
    commit_topology_publication(&point_path, &health_path, 402)
        .expect("commit replacement topology");
    point_writer
        .generation()
        .expect("replacement point generation")
        .acquisition_writer()
        .update_heartbeat(aether_shm_bridge::timestamp_ms());

    let error = generation
        .point_source()
        .slot_count()
        .expect_err("retained generation must not reconnect across epochs");
    assert_eq!(error.kind(), PortErrorKind::Conflict);
    assert!(error.is_retryable());
    assert_eq!(generation.publication_epoch(), 400);
}

#[test]
fn retained_generation_rejects_a_committed_writer_pair_that_reuses_its_epoch() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let point_path = directory.path().join("live.shm");
    let health_path = directory.path().join("health.shm");
    let points = point_manifest(&[(7, [1, 0, 0, 0])]);
    let health = health_manifest(&[7]);
    let publication_epoch = 450;
    let point_writer = ShmWriterHandle::create_published_at_epoch(
        ShmRuntimeConfig::new(&point_path, 32),
        Arc::clone(&points),
        None,
        publication_epoch,
    )
    .expect("publish initial point generation");
    let health_writer = ShmChannelHealthWriterHandle::create_at_epoch(
        &health_path,
        Arc::clone(&health),
        publication_epoch,
    )
    .expect("publish initial health generation");
    commit_topology_publication(&point_path, &health_path, publication_epoch)
        .expect("commit initial topology");
    let generation = ShmReadTopologyGeneration::open(
        ShmClientConfig::new(&point_path, points.layout_hash()),
        ShmClientConfig::new(&health_path, health.layout_hash()),
        Arc::clone(&points),
        Arc::clone(&health),
    )
    .expect("open initial topology");
    drop(point_writer);
    drop(health_writer);

    let _replacement_point = ShmWriterHandle::create_published_at_epoch(
        ShmRuntimeConfig::new(&point_path, 32),
        points,
        None,
        publication_epoch,
    )
    .expect("fault-inject a replacement point writer that reused the epoch");
    let _replacement_health =
        ShmChannelHealthWriterHandle::create_at_epoch(&health_path, health, publication_epoch)
            .expect("fault-inject a replacement health writer that reused the epoch");
    std::fs::remove_file(topology_commit_path_from_shm(&point_path))
        .expect("fault-inject loss of the durable epoch floor");
    commit_topology_publication(&point_path, &health_path, publication_epoch)
        .expect("fault-inject a valid replacement witness that reused the epoch");

    let error = generation
        .validate_layouts()
        .expect_err("a retained generation must pin the exact committed writer pair");
    assert_eq!(error.kind(), PortErrorKind::Conflict);
    assert!(error.is_retryable());
    assert_eq!(generation.publication_epoch(), publication_epoch);
}

#[test]
fn retained_generation_rejects_an_uncommitted_writer_restart_that_reuses_its_epoch() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let point_path = directory.path().join("live.shm");
    let health_path = directory.path().join("health.shm");
    let points = point_manifest(&[(7, [1, 0, 0, 0])]);
    let health = health_manifest(&[7]);
    let publication_epoch = 500;
    let point_writer = ShmWriterHandle::create_published_at_epoch(
        ShmRuntimeConfig::new(&point_path, 32),
        Arc::clone(&points),
        None,
        publication_epoch,
    )
    .expect("publish initial point generation");
    let _health_writer = ShmChannelHealthWriterHandle::create_at_epoch(
        &health_path,
        Arc::clone(&health),
        publication_epoch,
    )
    .expect("publish initial health generation");
    commit_topology_publication(&point_path, &health_path, publication_epoch)
        .expect("commit initial topology");
    let generation = ShmReadTopologyGeneration::open(
        ShmClientConfig::new(&point_path, points.layout_hash())
            .with_identity_check_interval(std::time::Duration::ZERO),
        ShmClientConfig::new(&health_path, health.layout_hash()),
        Arc::clone(&points),
        health,
    )
    .expect("open initial topology");
    drop(point_writer);

    let replacement = ShmWriterHandle::create_published_at_epoch(
        ShmRuntimeConfig::new(&point_path, 32),
        points,
        None,
        publication_epoch,
    )
    .expect("fault-inject a point writer restart with a reused epoch");
    replacement
        .generation()
        .expect("replacement point generation")
        .acquisition_writer()
        .update_heartbeat(aether_shm_bridge::timestamp_ms());

    let error = generation
        .point_source()
        .slot_count()
        .expect_err("the old witness must pin the original writer generation too");
    assert_eq!(error.kind(), PortErrorKind::Conflict);
    assert!(error.is_retryable());
}

#[test]
fn topology_commit_recovers_a_stale_staging_file_without_accumulating_orphans() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let point_path = directory.path().join("live.shm");
    let health_path = directory.path().join("health.shm");
    let points = point_manifest(&[(7, [1, 0, 0, 0])]);
    let health = health_manifest(&[7]);
    let publication_epoch = 700;
    let _point_writer = ShmWriterHandle::create_published_at_epoch(
        ShmRuntimeConfig::new(&point_path, 32),
        points,
        None,
        publication_epoch,
    )
    .expect("publish point generation");
    let _health_writer =
        ShmChannelHealthWriterHandle::create_at_epoch(&health_path, health, publication_epoch)
            .expect("publish health generation");
    let commit_path = topology_commit_path_from_shm(&point_path);
    let commit_name = commit_path
        .file_name()
        .and_then(|name| name.to_str())
        .expect("commit filename");
    let staging_path = commit_path.with_file_name(format!(".{commit_name}.staging"));
    std::fs::write(&staging_path, b"stale interrupted publication")
        .expect("fault-inject stale commit staging file");

    commit_topology_publication(&point_path, &health_path, publication_epoch)
        .expect("replace stale staging file and commit");

    assert!(!staging_path.exists());
    let staging_files = std::fs::read_dir(directory.path())
        .expect("read publication directory")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains(".staging"))
        .count();
    assert_eq!(staging_files, 0);
}

#[test]
fn truncated_commit_witness_fails_closed_without_authorizing_matching_planes() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let point_path = directory.path().join("live.shm");
    let health_path = directory.path().join("health.shm");
    let points = point_manifest(&[(7, [1, 0, 0, 0])]);
    let health = health_manifest(&[7]);
    let publication_epoch = 800;
    let _point_writer = ShmWriterHandle::create_published_at_epoch(
        ShmRuntimeConfig::new(&point_path, 32),
        Arc::clone(&points),
        None,
        publication_epoch,
    )
    .expect("publish point generation");
    let _health_writer = ShmChannelHealthWriterHandle::create_at_epoch(
        &health_path,
        Arc::clone(&health),
        publication_epoch,
    )
    .expect("publish health generation");
    commit_topology_publication(&point_path, &health_path, publication_epoch)
        .expect("commit topology");
    std::fs::write(topology_commit_path_from_shm(&point_path), b"torn")
        .expect("fault-inject truncated witness");

    let error = ShmReadTopologyGeneration::open(
        ShmClientConfig::new(&point_path, points.layout_hash()),
        ShmClientConfig::new(&health_path, health.layout_hash()),
        points,
        health,
    )
    .expect_err("a truncated final witness must fail closed");

    assert_eq!(error.kind(), PortErrorKind::Conflict);
    assert!(error.is_retryable());
}
