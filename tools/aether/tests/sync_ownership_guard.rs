//! The offline-sync guard must follow the site's runtime artefacts.
//!
//! It used to TCP-probe compiled-in ports 6001/6002, which answered the wrong
//! question in both directions: any unrelated listener on 6001 refused every
//! sync forever, and a real `aether-io` on a different port went undetected so
//! the guard permitted exactly the concurrent write it exists to prevent.

use std::net::TcpListener;
use std::process::Command;

use tempfile::TempDir;

/// Runs `aether sync --confirmed` against an isolated site.
fn run_sync(workspace: &TempDir, shm_path: &std::path::Path) -> std::process::Output {
    let data_path = workspace.path().join("data");
    let config_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("config.template");

    Command::new(env!("CARGO_BIN_EXE_aether"))
        .env("AETHER_SHM_PATH", shm_path)
        .args([
            "--json",
            "--config-path",
            config_path.to_str().expect("UTF-8 config path"),
            "--db-path",
            data_path.to_str().expect("UTF-8 data path"),
            "sync",
            "--confirmed",
        ])
        .output()
        .expect("run aether sync")
}

#[test]
fn an_unrelated_listener_on_the_default_port_no_longer_blocks_configuration() {
    // A Docker proxy, a leftover container, or any other service bound to 6001
    // is not an acquisition owner, and must not lock a site out of its own
    // configuration. Binding an ephemeral socket here is not the default port,
    // so this asserts the guard has stopped consulting ports at all: with no
    // SHM writer and no PointWatch socket, sync must proceed.
    let squatter = TcpListener::bind("127.0.0.1:0").expect("bind squatter");
    let _squatter_port = squatter.local_addr().expect("squatter addr").port();

    let workspace = TempDir::new().expect("workspace");
    let shm_path = workspace.path().join("absent-rtdb.shm");

    let output = run_sync(&workspace, &shm_path);

    assert!(
        output.status.success(),
        "sync must proceed when no runtime owner is live: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn a_missing_shared_memory_is_a_stopped_site_rather_than_an_unknown_one() {
    // A fresh install has no SHM at all. Treating that as "cannot tell, refuse"
    // would make the very first sync impossible.
    let workspace = TempDir::new().expect("workspace");
    let shm_path = workspace.path().join("never-created.shm");

    let output = run_sync(&workspace, &shm_path);

    assert!(
        output.status.success(),
        "first sync on a fresh site must not be refused: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn a_bound_point_watch_socket_still_refuses_the_apply() {
    // The dangerous direction: automation is live, so an offline apply would
    // race its in-memory desired state. The guard must catch this regardless of
    // which HTTP port that process happens to listen on.
    let workspace = TempDir::new().expect("workspace");
    let shm_path = workspace.path().join("rtdb.shm");
    let socket = aether_shm_bridge::point_watch_socket_from_shm(&shm_path, "automation");
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent).expect("socket directory");
    }
    let _listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind point watch");

    let output = run_sync(&workspace, &shm_path);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !output.status.success(),
        "sync must refuse while automation holds its socket: {stdout}"
    );
    assert!(
        stdout.contains("aether-automation"),
        "the refusal must name the live owner: {stdout}"
    );
}
