//! `aether-uplink` — the sole CloudLink composition owner.
//!
//! The process has one cloud protocol, one MQTT topic tree, and one durable
//! spool. Generic MQTT property/status/read/write/call-data surfaces are not
//! compiled into this binary.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::info;

mod cloudlink_runtime;
mod config;
mod live_values;
mod routes;
mod state;

use crate::cloudlink_runtime::{CloudLinkRuntime, CloudLinkRuntimeStatus};
use crate::config::EnvConfig;
use crate::state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let env = EnvConfig::from_env()?;
    let (addr, listener) = reserve_http_listener(&env)?;
    let alarm_broadcast_token = config::alarm_broadcast_token_from_env()?;

    common::service_bootstrap::init_service(
        "aether-uplink",
        "CloudLink edge-to-cloud service",
        env.api_port,
    )?;
    info!(
        cloudlink_enabled = env.cloudlink.is_some(),
        "aether-uplink starting"
    );
    run_service(env, alarm_broadcast_token, addr, listener).await
}

fn reserve_http_listener(
    env: &EnvConfig,
) -> anyhow::Result<(std::net::SocketAddr, tokio::net::TcpListener)> {
    let addr = common::bind_address(&env.api_host, env.api_port)?;
    let listener = common::shutdown::bind_http_listener(addr)?;
    Ok((addr, listener))
}

async fn run_service(
    mut env: EnvConfig,
    alarm_broadcast_token: String,
    addr: std::net::SocketAddr,
    listener: tokio::net::TcpListener,
) -> anyhow::Result<()> {
    let cloudlink_settings = env.cloudlink.take();

    let env = Arc::new(env);
    let spool = Arc::new(
        aether_store_local::FileCloudLinkSpool::open_with_limits(
            &env.spool_path,
            "business",
            env.spool_capacity,
            env.acknowledged_receipt_capacity,
            env.spool_max_live_bytes,
            env.spool_max_journal_bytes,
        )
        .map_err(|error| anyhow::anyhow!("CloudLink spool open failed: {error}"))?,
    );
    let runtime_status = Arc::new(CloudLinkRuntimeStatus::new(cloudlink_settings.is_some()));
    let delivery_wake = Arc::new(tokio::sync::Notify::new());
    let cloudlink = if let Some(settings) = cloudlink_settings {
        let sqlite = common::bootstrap_database::open_service_pool(&env.db_path).await?;
        let topology_snapshot = aether_sqlite_topology::load_sqlite_live_topology(&sqlite)
            .await
            .map_err(|error| anyhow::anyhow!("Live topology load failed: {error}"))?;
        let live_topology = Arc::new(
            live_values::UplinkTopologyHandle::new_lazy(topology_snapshot, &env)
                .map_err(|error| anyhow::anyhow!("Live topology initialization failed: {error}"))?,
        );
        let (runtime, manager) = CloudLinkRuntime::compose(
            settings,
            Arc::clone(&spool),
            Arc::clone(&live_topology),
            sqlite.clone(),
            Arc::clone(&runtime_status),
            Arc::clone(&delivery_wake),
        )
        .await?;
        Some((runtime, manager, live_topology, sqlite))
    } else {
        None
    };
    let state = Arc::new(AppState {
        spool,
        alarm_broadcast_token: Arc::from(alarm_broadcast_token),
        cloudlink: runtime_status,
        delivery_wake,
    });

    let shutdown = CancellationToken::new();
    let supervisor_task = if let Some((cloudlink, manager, topology, pool)) = cloudlink {
        let mut supervisor =
            common::task_supervisor::CriticalTaskSupervisor::new(std::time::Duration::from_secs(5));
        let config = Arc::clone(&env);
        let task_shutdown = shutdown.clone();
        supervisor.spawn("uplink-topology-refresh", async move {
            live_values::run_topology_refresher(topology, pool, config, task_shutdown).await;
        });
        let manager_shutdown = shutdown.clone();
        supervisor.spawn_result("uplink-cloudlink-mqtt", async move {
            manager
                .run(manager_shutdown)
                .await
                .map_err(|error| anyhow::anyhow!("CloudLink MQTT manager failed: {error}"))
        });
        let task_shutdown = shutdown.clone();
        supervisor.spawn_result("uplink-cloudlink", async move {
            cloudlink.run(task_shutdown).await
        });
        let supervisor_shutdown = shutdown.clone();
        Some(tokio::spawn(async move {
            supervisor.run(supervisor_shutdown).await
        }))
    } else {
        None
    };

    let app = routes::build_router(state).layer(axum::middleware::from_fn(
        common::logging::http_request_logger,
    ));
    let server_result =
        common::shutdown::serve_prebound_with_shutdown(listener, addr, app, shutdown.clone()).await;
    finish_service(server_result, shutdown, supervisor_task).await?;
    info!("aether-uplink stopped");
    Ok(())
}

async fn finish_service(
    server_result: anyhow::Result<()>,
    shutdown: CancellationToken,
    supervisor_task: Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
) -> anyhow::Result<()> {
    shutdown.cancel();
    let supervisor_result = match supervisor_task {
        Some(supervisor_task) => match supervisor_task.await {
            Ok(result) => result,
            Err(error) => Err(anyhow::anyhow!(
                "Uplink task supervisor join failed: {error}"
            )),
        },
        None => Ok(()),
    };
    combine_service_results(server_result, supervisor_result)
}

fn combine_service_results(
    server_result: anyhow::Result<()>,
    supervisor_result: anyhow::Result<()>,
) -> anyhow::Result<()> {
    match (server_result, supervisor_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(server), Ok(())) => Err(server),
        (Ok(()), Err(supervisor)) => Err(supervisor),
        (Err(server), Err(supervisor)) => Err(anyhow::anyhow!(
            "HTTP server failed: {server:#}; supervisor cleanup also failed: {supervisor:#}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};

    use aether_cloudlink_mqtt::CloudLinkTlsConfig;

    use super::*;
    use crate::config::CloudLinkSettings;

    #[tokio::test]
    async fn occupied_http_port_fails_before_spool_or_mqtt_side_effects() {
        let occupied = std::net::TcpListener::bind("127.0.0.1:0").expect("occupied HTTP port");
        let address = occupied.local_addr().expect("occupied address");
        let broker = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("broker probe");
        let broker_address = broker.local_addr().expect("broker address");
        let root = tempfile::tempdir().expect("temp dir");
        let spool_path = root.path().join("cloudlink.spool");
        let env = EnvConfig {
            api_host: address.ip().to_string(),
            api_port: address.port(),
            spool_path: spool_path.clone(),
            cloudlink: Some(CloudLinkSettings {
                broker_host: broker_address.ip().to_string(),
                broker_port: broker_address.port(),
                topic_prefix: "aether".to_owned(),
                username: None,
                password: None,
                tls: CloudLinkTlsConfig::SystemRoots,
                keep_alive_secs: 30,
                reconnect_delay_secs: 1,
                request_capacity: 8,
                identity_directory: root.path().join("identity"),
                cloud_key_id: "cloud-key".to_owned(),
                cloud_verifying_key: "invalid-but-unread".to_owned(),
                credential_id: "credential".to_owned(),
                credential_generation: 1,
                challenge_ledger_path: root.path().join("challenge-ledger.json"),
                challenge_ledger_capacity: 8,
                challenge_request_ttl_ms: 60_000,
                telemetry_interval_secs: 30,
                runtime_manifest_path: PathBuf::from("missing-runtime-manifest.json"),
            }),
            ..EnvConfig::default()
        };

        let result = reserve_http_listener(&env);

        assert!(result.is_err());
        assert!(
            !spool_path.exists(),
            "spool creation must follow HTTP pre-bind"
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), broker.accept())
                .await
                .is_err(),
            "occupied HTTP startup must not reach MQTT"
        );
    }

    #[tokio::test]
    async fn server_error_still_cancels_and_joins_supervised_tasks() {
        let shutdown = CancellationToken::new();
        let drained = Arc::new(AtomicBool::new(false));
        let task_shutdown = shutdown.clone();
        let task_drained = Arc::clone(&drained);
        let supervisor = tokio::spawn(async move {
            task_shutdown.cancelled().await;
            task_drained.store(true, Ordering::Relaxed);
            Ok(())
        });

        let error = finish_service(
            Err(anyhow::anyhow!("simulated HTTP failure")),
            shutdown,
            Some(supervisor),
        )
        .await
        .expect_err("HTTP error must propagate after drain");

        assert!(drained.load(Ordering::Relaxed));
        assert!(error.to_string().contains("simulated HTTP failure"));
    }

    #[test]
    fn simultaneous_server_and_supervisor_errors_are_both_preserved() {
        let error = combine_service_results(
            Err(anyhow::anyhow!("server failed")),
            Err(anyhow::anyhow!("manager failed")),
        )
        .expect_err("dual failure");
        let message = error.to_string();
        assert!(message.contains("server failed"));
        assert!(message.contains("manager failed"));
    }
}
