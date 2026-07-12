use std::sync::Arc;

use aether_sdk::application::{Actor, RequestContext};
use aether_sdk::domain::{
    CommandId, ControlCommand, InstanceId, PointAddress, PointId, PointKind, PointQuality,
    PointSample, TimestampMs,
};
use aether_sdk::ports::{CommandDispatcher, CommandReceipt, LiveStateWriter, PortResult};
use aether_sdk::{AetherBuilder, BuildError};
use aether_store_local::{MemoryAuditSink, MemoryLiveState};
use async_trait::async_trait;

struct NoopDispatcher;

#[async_trait]
impl CommandDispatcher for NoopDispatcher {
    async fn dispatch(&self, command: ControlCommand) -> PortResult<CommandReceipt> {
        Ok(CommandReceipt::new(command.id(), command.issued_at()))
    }
}

fn address() -> PointAddress {
    PointAddress::new(InstanceId::new(1), PointKind::Telemetry, PointId::new(2))
}

#[test]
fn builder_reports_missing_ports_without_connecting_to_external_services() {
    let error = match AetherBuilder::new().build() {
        Ok(_) => panic!("builder without ports must fail"),
        Err(error) => error,
    };

    assert_eq!(error, BuildError::MissingPort("live_state"));
}

#[test]
fn sdk_exposes_the_versioned_pack_contract() {
    let runtime = aether_sdk::pack::PackRuntime::new("0.5.0")
        .with_capabilities(["point.read"])
        .with_protocols(["modbus_tcp"]);

    assert!(format!("{runtime:?}").contains("point.read"));
}

#[tokio::test]
async fn sdk_composes_user_selected_adapters_and_exposes_the_application_api() {
    let state = Arc::new(MemoryLiveState::new());
    let expected = PointSample::new(address(), 42.5, TimestampMs::new(1_000), PointQuality::Good);
    state.write(expected).await.unwrap();

    let application = AetherBuilder::new()
        .with_live_state(Arc::clone(&state))
        .with_command_dispatcher(Arc::new(NoopDispatcher))
        .with_audit_sink(Arc::new(MemoryAuditSink::new()))
        .build()
        .expect("all required ports are present");

    let context = RequestContext::new(
        "read-1",
        Actor::new("embedded-host").with_permission("device.read"),
        false,
        TimestampMs::new(2_000),
    );
    assert_eq!(
        application.read_point(&context, address()).await.unwrap(),
        Some(expected)
    );

    let _opaque_command_id = CommandId::new(1);
}
