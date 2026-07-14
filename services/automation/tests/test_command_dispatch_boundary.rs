//! Logical application commands route to one typed physical C/A sink.

use std::sync::{Arc, Mutex};

use aether_application::{Actor, ControlApplication, RequestContext, SafetyPolicy};
use aether_automation::infra::application_control::AutomationCommandDispatcher;
use aether_automation::infra::runtime_topology::AutomationTopologyHandle;
use aether_automation::instance_manager::InstanceManager;
use aether_automation::product_loader::ProductLoader;
use aether_domain::{
    CommandId, InstanceId, PhysicalDeviceCommand, PointAddress, PointId, PointKind, TimestampMs,
};
use aether_ports::{CommandDispatcher, CommandReceipt, DeviceCommandSink, PortResult};
use aether_shm_bridge::{
    ShmChannelHealthWriterHandle, ShmDeviceCommandSink, ShmRuntimeConfig, ShmWriterHandle,
    commit_topology_publication,
};
use aether_store_local::SqliteAuditSink;
use async_trait::async_trait;

#[derive(Default)]
struct RecordingDeviceSink {
    commands: Mutex<Vec<PhysicalDeviceCommand>>,
}

impl RecordingDeviceSink {
    fn commands(&self) -> Vec<PhysicalDeviceCommand> {
        self.commands
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

#[async_trait]
impl DeviceCommandSink for RecordingDeviceSink {
    async fn send(&self, command: PhysicalDeviceCommand) -> PortResult<CommandReceipt> {
        self.commands
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(command);
        Ok(CommandReceipt::new(command.id(), command.issued_at()))
    }
}

async fn application(
    directory: &tempfile::TempDir,
) -> (ControlApplication, Arc<RecordingDeviceSink>) {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("open automation database");
    common::test_utils::schema::init_automation_schema(&pool)
        .await
        .expect("automation schema");
    common::test_utils::schema::init_io_schema(&pool)
        .await
        .expect("IO schema");
    sqlx::query(
        "INSERT INTO channels (channel_id, name, protocol, enabled)
         VALUES (2, 'device', 'virtual', 1)",
    )
    .execute(&pool)
    .await
    .expect("channel");
    sqlx::query(
        "INSERT INTO adjustment_points
         (channel_id, point_id, signal_name, min_value, max_value, step)
         VALUES (2, 5, 'setpoint', 0.0, 100.0, 1.0)",
    )
    .execute(&pool)
    .await
    .expect("action point");
    sqlx::query(
        "INSERT INTO instances (instance_id, instance_name, product_name)
         VALUES (1001, 'controller', 'GenericController')",
    )
    .execute(&pool)
    .await
    .expect("instance");
    sqlx::query(
        "INSERT INTO action_routing
         (instance_id, instance_name, action_id, channel_id, channel_type, channel_point_id, enabled)
         VALUES (1001, 'controller', 1, 2, 'A', 5, 1)",
    )
    .execute(&pool)
    .await
    .expect("logical action route");

    let manager = Arc::new(InstanceManager::new(
        pool.clone(),
        Arc::new(ProductLoader::new(pool.clone())),
    ));

    let snapshot = aether_store_local::load_sqlite_live_topology(&pool)
        .await
        .expect("coherent topology snapshot");
    let point_path = directory.path().join("live.shm");
    let health_path = directory.path().join("channel-health.shm");
    let epoch = 77;
    let _point_writer = ShmWriterHandle::create_published_at_epoch(
        ShmRuntimeConfig::new(&point_path, 32),
        Arc::new(snapshot.point_manifest().clone()),
        None,
        epoch,
    )
    .expect("point writer");
    let health_writer = ShmChannelHealthWriterHandle::create_at_epoch(
        &health_path,
        Arc::new(snapshot.health_manifest().clone()),
        epoch,
    )
    .expect("health writer");
    commit_topology_publication(&point_path, &health_path, epoch).expect("topology commit");
    let health_timestamp = chrono::Utc::now().timestamp_millis().max(0) as u64;
    health_writer
        .set_online(2, true, health_timestamp)
        .expect("online channel");
    let topology = Arc::new(
        AutomationTopologyHandle::new_lazy(
            point_path,
            health_path,
            snapshot,
            Arc::new(ShmDeviceCommandSink::new()),
        )
        .expect("automation topology"),
    );
    topology.refresh(&pool).await.expect("topology refresh");
    manager
        .set_runtime_topology(topology)
        .expect("install runtime topology");

    let sink = Arc::new(RecordingDeviceSink::default());
    let dispatcher: Arc<dyn CommandDispatcher> = Arc::new(AutomationCommandDispatcher::new(
        Arc::clone(&manager),
        Arc::clone(&sink) as Arc<dyn DeviceCommandSink>,
    ));
    let audit = Arc::new(SqliteAuditSink::initialize(pool).await.expect("audit sink"));
    (
        ControlApplication::new(dispatcher, audit, SafetyPolicy),
        sink,
    )
}

fn context() -> RequestContext {
    let now = chrono::Utc::now().timestamp_millis().max(0) as u64;
    RequestContext::new(
        "command-boundary-contract",
        Actor::new("operator:7").with_permission("device.control"),
        true,
        TimestampMs::new(now),
    )
}

fn logical_action(value: f64) -> (CommandId, PointAddress, f64) {
    (
        CommandId::new(77),
        PointAddress::new(InstanceId::new(1001), PointKind::Action, PointId::new(1)),
        value,
    )
}

#[tokio::test]
async fn real_control_application_routes_and_limits_before_the_physical_sink() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let (application, sink) = application(&directory).await;
    let (id, target, value) = logical_action(42.0);

    let receipt = application
        .write_point(&context(), id, target, value)
        .await
        .expect("command transport accepted");

    assert_eq!(receipt.command_id(), id);
    let commands = sink.commands();
    assert_eq!(commands.len(), 1);
    let physical = commands[0];
    assert_eq!(physical.id(), id);
    assert_eq!(physical.target().channel_id().get(), 2);
    assert_eq!(physical.target().kind(), PointKind::Action);
    assert_eq!(physical.target().point_id().get(), 5);
    assert_eq!(physical.value(), 42.0);
    assert_eq!(
        physical.expires_at().get() - physical.issued_at().get(),
        aether_domain::DEFAULT_COMMAND_TTL_MS
    );
}

#[tokio::test]
async fn configured_limits_reject_before_the_physical_sink() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let (application, sink) = application(&directory).await;
    let (id, target, value) = logical_action(101.0);

    let error = application
        .write_point(&context(), id, target, value)
        .await
        .expect_err("out-of-range command must fail");

    assert!(error.to_string().contains("outside the allowed range"));
    assert!(sink.commands().is_empty());
}
