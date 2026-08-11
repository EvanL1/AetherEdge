use aether_dataplane::SlotWriter;
use aether_shm_bridge::{
    ShmObservationStatus, ShmObserver, channel_health_path_from_shm, commit_topology_publication,
    timestamp_ms,
};

fn published_fixture() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    SlotWriter,
    SlotWriter,
) {
    let directory = tempfile::tempdir().expect("observer fixture directory");
    let point_path = directory.path().join("aether-live-state.shm");
    let health_path = channel_health_path_from_shm(&point_path);
    let point = SlotWriter::create(&point_path, 2, 0x1010, 7).expect("point plane");
    let health = SlotWriter::create(&health_path, 1, 0x2020, 7).expect("health plane");
    point.set_direct(0, 12.5, 125.0, 1_000, 0);
    health.set_direct(0, 1.0, 1.0, 1_001, 0);
    let now = timestamp_ms();
    point.update_heartbeat(now);
    health.update_heartbeat(now);
    commit_topology_publication(&point_path, &health_path, 7).expect("topology witness");
    (directory, point_path, point, health)
}

#[test]
fn observer_reports_one_committed_dual_plane_without_writing_it() {
    let (_directory, point_path, point, health) = published_fixture();
    let point_generation = point.generation();
    let health_generation = health.generation();
    let point_heartbeat = point.writer_heartbeat();
    let health_heartbeat = health.writer_heartbeat();

    let observation = ShmObserver::new(&point_path).inspect();

    assert_eq!(observation.status, ShmObservationStatus::Healthy);
    assert_eq!(observation.publication_epoch, Some(7));
    assert!(observation.findings.is_empty());
    let point_plane = observation.point.expect("point observation");
    assert_eq!(point_plane.slot_count, 2);
    assert_eq!(point_plane.writer_generation, point_generation);
    let slots = point_plane.slots.expect("point slot summary");
    assert_eq!(slots.present, 1);
    assert_eq!(slots.unwritten, 1);
    assert_eq!(slots.good, 1);
    let health_plane = observation.health.expect("health observation");
    assert_eq!(health_plane.writer_generation, health_generation);
    let slots = health_plane.slots.expect("health slot summary");
    assert_eq!(slots.online, 1);
    assert_eq!(slots.offline, 0);

    assert_eq!(point_generation, point.generation());
    assert_eq!(health_generation, health.generation());
    assert_eq!(point_heartbeat, point.writer_heartbeat());
    assert_eq!(health_heartbeat, health.writer_heartbeat());
}

#[test]
fn heartbeat_age_moves_from_degraded_to_unhealthy_at_explicit_thresholds() {
    let (_directory, point_path, point, health) = published_fixture();
    let observer = ShmObserver::new(&point_path)
        .with_liveness_thresholds(1_000, 10_000)
        .expect("valid thresholds");
    let now = timestamp_ms();
    point.update_heartbeat(now.saturating_sub(5_000));
    health.update_heartbeat(now.saturating_sub(5_000));

    let degraded = observer.inspect();
    assert_eq!(degraded.status, ShmObservationStatus::Degraded);
    assert!(
        degraded
            .findings
            .iter()
            .any(|finding| finding.code == "writer_heartbeat_delayed")
    );

    point.update_heartbeat(now.saturating_sub(20_000));
    health.update_heartbeat(now.saturating_sub(20_000));
    let unhealthy = observer.inspect();
    assert_eq!(unhealthy.status, ShmObservationStatus::Unhealthy);
    assert!(
        unhealthy
            .findings
            .iter()
            .any(|finding| finding.code == "writer_heartbeat_stale")
    );
}

#[test]
fn missing_or_uncommitted_planes_fail_closed_as_observations() {
    let directory = tempfile::tempdir().expect("observer fixture directory");
    let missing = directory.path().join("missing.shm");
    let missing_observation = ShmObserver::new(&missing).inspect();
    assert_eq!(missing_observation.status, ShmObservationStatus::Unhealthy);
    assert!(missing_observation.point.is_none());

    let (_directory, point_path, _point, _health) = published_fixture();
    std::fs::remove_file(aether_shm_bridge::topology_commit_path_from_shm(
        &point_path,
    ))
    .expect("remove witness");
    let uncommitted = ShmObserver::new(&point_path).inspect();
    assert_eq!(uncommitted.status, ShmObservationStatus::Unhealthy);
    assert!(
        uncommitted
            .findings
            .iter()
            .any(|finding| finding.code == "topology_commit_unavailable")
    );
}
