use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use aether_domain::{
    AcquiredPointSample, ChannelPointAddress, PointKind, PointQuality, TimestampMs,
};
use aether_ports::{PortError, PortErrorKind, PortResult};
use aether_shm_bridge::{
    ChannelHealthManifest, ChannelPointManifest, PhysicalPointAddress,
    ShmChannelHealthWriterHandle, ShmClientConfig, ShmReadTopologyGeneration, ShmRuntimeConfig,
    ShmWriterHandle, SlotSource, begin_topology_publication,
};

const ROLE_ENV: &str = "AETHER_TOPOLOGY_STRESS_ROLE";
const POINT_PATH_ENV: &str = "AETHER_TOPOLOGY_STRESS_POINT_PATH";
const HEALTH_PATH_ENV: &str = "AETHER_TOPOLOGY_STRESS_HEALTH_PATH";
const STOP_PATH_ENV: &str = "AETHER_TOPOLOGY_STRESS_STOP_PATH";
const READY_PATH_ENV: &str = "AETHER_TOPOLOGY_STRESS_READY_PATH";
const EPOCH_BASE_ENV: &str = "AETHER_TOPOLOGY_STRESS_EPOCH_BASE";
const CYCLES_ENV: &str = "AETHER_TOPOLOGY_STRESS_CYCLES";
const TOPOLOGY_ENV: &str = "AETHER_TOPOLOGY_STRESS_TOPOLOGY";

#[derive(Clone, Copy)]
enum FixtureTopology {
    A,
    B,
}

impl FixtureTopology {
    const fn alternate(self) -> Self {
        match self {
            Self::A => Self::B,
            Self::B => Self::A,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::A => "a",
            Self::B => "b",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "a" => Self::A,
            "b" => Self::B,
            other => panic!("unknown stress topology {other:?}"),
        }
    }

    fn points(self) -> Arc<ChannelPointManifest> {
        let entries = match self {
            Self::A => vec![(1, [2, 1, 0, 0])],
            Self::B => vec![(1, [1, 1, 0, 0]), (2, [1, 0, 0, 0])],
        };
        Arc::new(ChannelPointManifest::from_entries(entries))
    }

    fn health(self) -> Arc<ChannelHealthManifest> {
        let channels: &[u32] = match self {
            Self::A => &[1],
            Self::B => &[1, 2],
        };
        Arc::new(ChannelHealthManifest::from_channel_ids(
            channels.iter().copied(),
        ))
    }

    fn configured_points(self) -> Vec<PhysicalPointAddress> {
        match self {
            Self::A => vec![
                PhysicalPointAddress::from_legacy_raw(1, PointKind::Telemetry, 0),
                PhysicalPointAddress::from_legacy_raw(1, PointKind::Telemetry, 1),
                PhysicalPointAddress::from_legacy_raw(1, PointKind::Status, 0),
            ],
            Self::B => vec![
                PhysicalPointAddress::from_legacy_raw(1, PointKind::Telemetry, 0),
                PhysicalPointAddress::from_legacy_raw(1, PointKind::Status, 0),
                PhysicalPointAddress::from_legacy_raw(2, PointKind::Telemetry, 0),
            ],
        }
    }
}

#[test]
fn helper_writer_process() {
    if std::env::var(ROLE_ENV).as_deref() != Ok("writer") {
        return;
    }
    let point_path = required_path(POINT_PATH_ENV);
    let health_path = required_path(HEALTH_PATH_ENV);
    let epoch_base = required_u64(EPOCH_BASE_ENV);
    let cycles = required_u64(CYCLES_ENV);
    publish_cycles(&point_path, &health_path, epoch_base, cycles);
}

#[test]
fn helper_crashing_writer_process() {
    if std::env::var(ROLE_ENV).as_deref() != Ok("crash-writer") {
        return;
    }
    let point_path = required_path(POINT_PATH_ENV);
    let ready_path = required_path(READY_PATH_ENV);
    let topology =
        FixtureTopology::parse(&std::env::var(TOPOLOGY_ENV).expect("crash writer topology"));
    let epoch = required_u64(EPOCH_BASE_ENV);
    let publication = begin_topology_publication(&point_path).expect("begin partial publication");
    let point = ShmWriterHandle::create_published_at_epoch(
        ShmRuntimeConfig::new(&point_path, 64),
        topology.points(),
        None,
        epoch,
    )
    .expect("publish only the point plane");
    std::fs::write(&ready_path, b"point-published").expect("signal crash window");
    std::hint::black_box((&publication, &point));
    loop {
        std::thread::park();
    }
}

#[test]
fn helper_reader_process() {
    if std::env::var(ROLE_ENV).as_deref() != Ok("reader") {
        return;
    }
    let point_path = required_path(POINT_PATH_ENV);
    let health_path = required_path(HEALTH_PATH_ENV);
    let stop_path = required_path(STOP_PATH_ENV);
    let mut last_progress = Instant::now();
    let mut coherent_reads = 0_u64;

    while !stop_path.exists() || coherent_reads == 0 {
        assert!(
            last_progress.elapsed() < Duration::from_secs(45),
            "reader made no coherent progress for 45 seconds"
        );
        for topology in [FixtureTopology::A, FixtureTopology::B] {
            match open_and_verify(&point_path, &health_path, topology) {
                Ok(()) => {
                    coherent_reads += 1;
                    last_progress = Instant::now();
                },
                Err(error) if error.is_retryable() => {},
                Err(error) => panic!("non-retryable topology read failure: {error}"),
            }
        }
        // Keep the soak resource-bounded while still sampling publication
        // windows thousands of times per second across reader processes.
        std::thread::sleep(Duration::from_micros(50));
    }
}

#[test]
fn cross_process_topology_switch_restart_and_partial_publish_stress() {
    run_process_stress(3, 30, 3);
}

#[test]
#[ignore = "explicit long-running SHM topology soak gate"]
fn cross_process_topology_switch_restart_and_partial_publish_soak() {
    run_process_stress(12, 500, 4);
}

fn run_process_stress(restarts: u64, cycles_per_restart: u64, reader_count: usize) {
    let directory = tempfile::tempdir().expect("stress directory");
    let point_path = directory.path().join("live.shm");
    let health_path = directory.path().join("health.shm");
    let stop_path = directory.path().join("stop");
    let mut readers = (0..reader_count)
        .map(|_| spawn_reader(&point_path, &health_path, &stop_path))
        .collect::<Vec<_>>();

    let mut final_topology = FixtureTopology::A;
    for restart in 0..restarts {
        let epoch_base = 1_000 + restart * (cycles_per_restart + 10);
        let status = child_command("helper_writer_process", "writer")
            .env(POINT_PATH_ENV, &point_path)
            .env(HEALTH_PATH_ENV, &health_path)
            .env(EPOCH_BASE_ENV, epoch_base.to_string())
            .env(CYCLES_ENV, cycles_per_restart.to_string())
            .status()
            .expect("start writer process");
        assert!(
            status.success(),
            "writer restart {restart} failed: {status}"
        );
        final_topology = if (cycles_per_restart - 1) & 1 == 0 {
            FixtureTopology::A
        } else {
            FixtureTopology::B
        };

        if restart + 1 < restarts {
            let partial_topology = final_topology.alternate();
            let partial_epoch = epoch_base + cycles_per_restart + 1;
            crash_after_point_publication(
                &point_path,
                partial_epoch,
                partial_topology,
                directory.path(),
            );
            assert_no_topology_is_authorized(&point_path, &health_path);
        }
        assert_reader_resources_bounded(&readers);
    }

    open_and_verify(&point_path, &health_path, final_topology)
        .expect("final recovered topology is coherent");
    std::fs::write(&stop_path, b"stop").expect("signal readers to stop");
    for (index, reader) in readers.iter_mut().enumerate() {
        let status = reader.wait().expect("wait for reader process");
        assert!(status.success(), "reader {index} failed: {status}");
    }

    let staging_files = std::fs::read_dir(directory.path())
        .expect("read stress directory")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains(".staging"))
        .count();
    assert_eq!(staging_files, 0, "publication staging files accumulated");
    let directory_entries = std::fs::read_dir(directory.path())
        .expect("read bounded stress directory")
        .count();
    assert!(
        directory_entries <= 16,
        "SHM publication artifacts grew without bound: {directory_entries} entries"
    );
}

fn crash_after_point_publication(
    point_path: &Path,
    epoch: u64,
    topology: FixtureTopology,
    directory: &Path,
) {
    let ready_path = directory.join(format!("crash-ready-{epoch}"));
    let mut writer = child_command("helper_crashing_writer_process", "crash-writer")
        .env(POINT_PATH_ENV, point_path)
        .env(READY_PATH_ENV, &ready_path)
        .env(EPOCH_BASE_ENV, epoch.to_string())
        .env(TOPOLOGY_ENV, topology.label())
        .stdout(Stdio::null())
        .spawn()
        .expect("start crashing writer process");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready_path.exists() {
        assert!(
            Instant::now() < deadline,
            "crashing writer did not enter the point-only publication window"
        );
        assert!(
            writer.try_wait().expect("poll crashing writer").is_none(),
            "crashing writer exited before the parent killed it"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    writer.kill().expect("kill writer in point-only window");
    let status = writer.wait().expect("reap killed writer");
    assert!(
        !status.success(),
        "killed writer unexpectedly exited cleanly"
    );
    std::fs::remove_file(ready_path).expect("remove crash readiness marker");
}

#[cfg(target_os = "linux")]
fn assert_reader_resources_bounded(readers: &[Child]) {
    const MAX_OPEN_FILES: usize = 64;
    const MAX_RSS_KIB: u64 = 256 * 1024;

    for reader in readers {
        let fd_count = std::fs::read_dir(format!("/proc/{}/fd", reader.id()))
            .expect("read reader file descriptors")
            .count();
        assert!(
            fd_count <= MAX_OPEN_FILES,
            "reader {} leaked file descriptors: {fd_count} > {MAX_OPEN_FILES}",
            reader.id()
        );

        let status = std::fs::read_to_string(format!("/proc/{}/status", reader.id()))
            .expect("read reader process status");
        let rss_kib = status
            .lines()
            .find_map(|line| line.strip_prefix("VmRSS:"))
            .and_then(|value| value.split_whitespace().next())
            .and_then(|value| value.parse::<u64>().ok())
            .expect("reader VmRSS");
        assert!(
            rss_kib <= MAX_RSS_KIB,
            "reader {} exceeded RSS bound: {rss_kib} KiB > {MAX_RSS_KIB} KiB",
            reader.id()
        );
    }
}

#[cfg(not(target_os = "linux"))]
fn assert_reader_resources_bounded(_readers: &[Child]) {}

fn spawn_reader(point_path: &Path, health_path: &Path, stop_path: &Path) -> Child {
    child_command("helper_reader_process", "reader")
        .env(POINT_PATH_ENV, point_path)
        .env(HEALTH_PATH_ENV, health_path)
        .env(STOP_PATH_ENV, stop_path)
        .stdout(Stdio::null())
        .spawn()
        .expect("start reader process")
}

fn child_command(test_name: &str, role: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().expect("current test executable"));
    command
        .arg("--exact")
        .arg(test_name)
        .arg("--nocapture")
        .env(ROLE_ENV, role);
    command
}

fn publish_cycles(point_path: &Path, health_path: &Path, epoch_base: u64, cycles: u64) {
    assert!(cycles > 0);
    let first_topology = FixtureTopology::A;
    let first_epoch = epoch_base;
    let first_publication =
        begin_topology_publication(point_path).expect("begin initial publication");
    let point = ShmWriterHandle::create_published_at_epoch(
        ShmRuntimeConfig::new(point_path, 64),
        first_topology.points(),
        None,
        first_epoch,
    )
    .expect("publish initial point plane");
    let health = ShmChannelHealthWriterHandle::create_at_epoch(
        health_path,
        first_topology.health(),
        first_epoch,
    )
    .expect("publish initial health plane");
    write_epoch_state(&point, &health, first_topology, first_epoch);
    first_publication
        .commit(health_path, first_epoch)
        .expect("commit initial publication");

    let mut topology = first_topology;
    for offset in 1..cycles {
        topology = topology.alternate();
        let epoch = epoch_base + offset;
        let publication = begin_topology_publication(point_path).expect("begin publication");
        point
            .rebuild_for_publication(topology.points(), epoch)
            .expect("publish point plane");
        std::thread::sleep(Duration::from_micros(100));
        health
            .rebuild_for_publication(topology.health(), epoch)
            .expect("publish health plane");
        write_epoch_state(&point, &health, topology, epoch);
        publication
            .commit(health_path, epoch)
            .expect("commit publication");
    }
}

fn write_epoch_state(
    point: &ShmWriterHandle,
    health: &ShmChannelHealthWriterHandle,
    topology: FixtureTopology,
    epoch: u64,
) {
    let timestamp_ms = aether_shm_bridge::timestamp_ms();
    let samples = topology
        .configured_points()
        .into_iter()
        .map(|address| {
            let address =
                ChannelPointAddress::new(address.channel_id(), address.kind(), address.point_id())
                    .expect("valid stress point address");
            AcquiredPointSample::new(
                address,
                epoch as f64,
                epoch as f64,
                TimestampMs::new(timestamp_ms),
                PointQuality::Good,
            )
            .expect("valid stress sample")
        })
        .collect::<Vec<_>>();
    point
        .generation()
        .expect("point generation")
        .acquisition_writer()
        .commit_batch(&samples)
        .expect("write epoch point state");
    for channel_id in topology.health().channel_ids() {
        health
            .set_online(channel_id, epoch & 1 != 0, timestamp_ms)
            .expect("write epoch health state");
    }
}

fn open_and_verify(
    point_path: &Path,
    health_path: &Path,
    topology: FixtureTopology,
) -> PortResult<()> {
    let points = topology.points();
    let health = topology.health();
    let generation = ShmReadTopologyGeneration::open(
        ShmClientConfig::new(point_path, points.layout_hash())
            .with_identity_check_interval(Duration::ZERO)
            .with_writer_stale_after(Duration::from_secs(10)),
        ShmClientConfig::new(health_path, health.layout_hash())
            .with_identity_check_interval(Duration::ZERO)
            .with_writer_stale_after(Duration::from_secs(10)),
        Arc::clone(&points),
        Arc::clone(&health),
    )?;
    let epoch = generation.publication_epoch();
    for address in topology.configured_points() {
        let slot = points
            .slot_for(address)
            .ok_or_else(|| invalid("missing stress slot"))?;
        let sample = generation
            .point_source()
            .read_slot(slot)?
            .ok_or_else(|| invalid("committed stress point is unwritten"))?;
        if sample.value() != epoch as f64 || sample.raw() != epoch as f64 {
            return Err(invalid(format!(
                "mixed/torn point state: epoch={epoch}, value={}, raw={}",
                sample.value(),
                sample.raw()
            )));
        }
    }
    for channel_id in health.channel_ids() {
        let observation = generation
            .channel_health()
            .read_channel(channel_id)?
            .ok_or_else(|| invalid("committed stress health is unwritten"))?;
        if observation.online() != (epoch & 1 != 0) {
            return Err(invalid(format!(
                "mixed health state for epoch {epoch} and channel {channel_id}"
            )));
        }
    }
    Ok(())
}

fn assert_no_topology_is_authorized(point_path: &Path, health_path: &Path) {
    for topology in [FixtureTopology::A, FixtureTopology::B] {
        let error = open_and_verify(point_path, health_path, topology)
            .expect_err("partial point-only publication must fail closed");
        assert!(error.is_retryable(), "unexpected partial error: {error}");
    }
}

fn invalid(message: impl Into<String>) -> PortError {
    PortError::new(PortErrorKind::InvalidData, message)
}

fn required_path(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("missing {name}")))
}

fn required_u64(name: &str) -> u64 {
    std::env::var(name)
        .unwrap_or_else(|_| panic!("missing {name}"))
        .parse()
        .unwrap_or_else(|error| panic!("invalid {name}: {error}"))
}
