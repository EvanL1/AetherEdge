//! Exercise the production startup order with two real IO processes.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use aether_shm_bridge::read_topology_publication_commit;

struct IoProcess {
    child: Child,
    log: PathBuf,
}

impl IoProcess {
    fn spawn(root: &Path, name: &str, point: &Path, health: &Path) -> Self {
        let log = root.join(format!("{name}.log"));
        let stdout = File::create(&log).expect("create process log");
        let child = Command::new(env!("CARGO_BIN_EXE_aether-io"))
            .args(["--no-color", "--bind-address", "127.0.0.1:0"])
            .current_dir(root)
            .env("RUST_LOG", "info")
            .env("AETHER_DB_PATH", root.join("config.db"))
            .env("AETHER_SHM_PATH", point)
            .env("AETHER_CHANNEL_HEALTH_SHM_PATH", health)
            .env("AETHER_M2C_SOCKET", root.join(format!("{name}.sock")))
            .env("AETHER_LOG_DIR", root.join(format!("{name}-logs")))
            .env("SHM_SNAPSHOT_PATH", root.join(format!("{name}.snapshot")))
            .env("SHM_RESTORE_ON_START", "false")
            .env("JWT_SECRET_KEY", "0123456789abcdef0123456789abcdef")
            .stdout(Stdio::from(stdout.try_clone().expect("clone log file")))
            .stderr(Stdio::from(stdout))
            .spawn()
            .expect("start aether-io");
        Self { child, log }
    }

    fn output(&self) -> String {
        std::fs::read_to_string(&self.log).expect("read process log")
    }

    async fn settled(&mut self) -> Option<ExitStatus> {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = self.child.try_wait().expect("poll IO process") {
                return Some(status);
            }
            if self.output().contains("API server listening on") {
                return None;
            }
            assert!(
                Instant::now() < deadline,
                "IO startup stalled: {}",
                self.output()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn stop(&mut self) {
        if self.child.try_wait().expect("poll before stop").is_none() {
            self.child.kill().expect("stop test IO process");
        }
        self.child.wait().expect("reap IO process");
    }
}

impl Drop for IoProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn duplicate_writer_is_rejected(share_point: bool) {
    let directory = tempfile::Builder::new()
        .prefix("ae-io-")
        .tempdir_in("/tmp")
        .expect("isolated IO files");
    let root = directory.path();
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .connect_with(common::bootstrap_database::sqlite_connect_options(
            root.join("config.db").to_str().expect("database path"),
        ))
        .await
        .expect("configuration database");
    common::schema::init_io_schema(&pool)
        .await
        .expect("IO schema");
    common::schema::init_automation_schema(&pool)
        .await
        .expect("routing schema");
    pool.close().await;

    let point = root.join("points.shm");
    let health = root.join("health.shm");
    let mut first = IoProcess::spawn(root, "first", &point, &health);
    assert!(
        first.settled().await.is_none(),
        "first IO failed: {}",
        first.output()
    );
    let original = read_topology_publication_commit(&point).expect("first topology commit");
    let second_point = if share_point {
        point.clone()
    } else {
        root.join("other.shm")
    };
    let mut second = IoProcess::spawn(root, "second", &second_point, &health);
    let status = second.settled().await;

    let after = read_topology_publication_commit(&point).expect("first topology still readable");
    assert_eq!(
        after, original,
        "second IO replaced the running writer's topology"
    );
    let health_header = aether_dataplane::SlotReader::open(&health).expect("health plane");
    assert_eq!(
        health_header.publication_epoch(),
        original.publication_epoch(),
        "second IO replaced the running health plane"
    );
    assert!(
        status.is_some_and(|status| !status.success()),
        "duplicate IO must fail startup: {}",
        second.output()
    );
    assert!(
        second
            .output()
            .contains("already owned by another aether-io process"),
        "{}",
        second.output()
    );
    assert!(first.child.try_wait().expect("first IO status").is_none());
    if !share_point {
        assert!(
            !second_point.exists(),
            "ownership must be checked before publishing either plane"
        );
    }

    // The OS must release ownership even after an abrupt exit, allowing restart.
    first.stop();
    let mut restarted = IoProcess::spawn(root, "restarted", &point, &health);
    assert!(
        restarted.settled().await.is_none(),
        "restart failed: {}",
        restarted.output()
    );
    assert!(
        read_topology_publication_commit(&point)
            .expect("restarted topology")
            .publication_epoch()
            > original.publication_epoch()
    );
}

#[tokio::test]
async fn second_io_cannot_replace_a_running_io_topology() {
    duplicate_writer_is_rejected(true).await;
}

#[tokio::test]
async fn second_io_cannot_share_only_the_health_plane() {
    duplicate_writer_is_rejected(false).await;
}
