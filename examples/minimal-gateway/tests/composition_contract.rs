use aether_example_minimal_gateway::MinimalGateway;
use aether_sdk::application::{Actor, ApplicationError, RequestContext};
use aether_sdk::domain::{
    CommandId, InstanceId, PointAddress, PointId, PointKind, PointQuality, PointSample, TimestampMs,
};

fn telemetry() -> PointAddress {
    PointAddress::new(InstanceId::new(1), PointKind::Telemetry, PointId::new(1))
}

fn action() -> PointAddress {
    PointAddress::new(InstanceId::new(1), PointKind::Action, PointId::new(2))
}

#[tokio::test]
async fn minimal_gateway_reads_local_state_without_external_services() {
    let gateway = MinimalGateway::new().expect("local ports are complete");
    let sample = PointSample::new(
        telemetry(),
        88.0,
        TimestampMs::new(1_000),
        PointQuality::Good,
    );
    gateway.publish(sample).await.unwrap();

    let context = RequestContext::new(
        "read-1",
        Actor::new("local-agent").with_permission("device.read"),
        false,
        TimestampMs::new(1_001),
    );
    assert_eq!(
        gateway
            .application()
            .read_point(&context, telemetry())
            .await
            .unwrap(),
        Some(sample)
    );
}

#[tokio::test]
async fn gateway_without_a_device_driver_fails_control_closed_and_audits_it() {
    let gateway = MinimalGateway::new().expect("local ports are complete");
    let context = RequestContext::new(
        "write-1",
        Actor::new("local-agent").with_permission("device.control"),
        true,
        TimestampMs::new(2_000),
    );
    let result = gateway
        .application()
        .write_point(&context, CommandId::new(1), action(), 1.0)
        .await;

    assert!(matches!(result, Err(ApplicationError::Port(_))));
    assert_eq!(gateway.audit_records().unwrap().len(), 2);
}
