use aether_domain::{
    InstanceId, PointAddress, PointId, PointKind, PointQuality, PointSample, TimestampMs,
};
use aether_ports::{
    CloudLinkMessageKind, CloudLinkSpool, CloudLinkTransport, CloudLinkTransportEvent,
    CloudLinkTransportMessage, LiveState, LiveStateWriter, PortError, PortErrorKind, PortResult,
};
use aether_store_local::{MemoryCloudLinkSpool, MemoryLiveState};
use async_trait::async_trait;

struct UnavailableCloud;

#[async_trait]
impl CloudLinkTransport for UnavailableCloud {
    async fn send(&self, _message: CloudLinkTransportMessage) -> PortResult<()> {
        Err(PortError::new(
            PortErrorKind::Unavailable,
            "test cloud unavailable",
        ))
    }

    async fn receive(&self) -> PortResult<CloudLinkTransportEvent> {
        Err(PortError::new(
            PortErrorKind::Unavailable,
            "test cloud unavailable",
        ))
    }
}

#[tokio::test]
async fn cloud_disconnect_does_not_change_acquisition_authority_or_local_progress() {
    let address = PointAddress::new(InstanceId::new(1), PointKind::Telemetry, PointId::new(2));
    let live = MemoryLiveState::new();
    let spool = MemoryCloudLinkSpool::new("telemetry", 8).expect("spool");
    let cloud = UnavailableCloud;

    let queued = spool
        .enqueue(aether_ports::CloudLinkEnqueue::new(
            CloudLinkMessageKind::TelemetryBatch,
            "batch-offline",
            format!("sha256:{}", "a".repeat(64)),
            b"{}".to_vec(),
            TimestampMs::new(1),
            None,
        ))
        .await
        .expect("durably queue while cloud is absent");
    assert!(
        cloud
            .send(CloudLinkTransportMessage::new(
                aether_ports::CloudLinkTransportRoute::TelemetryUp,
                b"{}".to_vec(),
                Some(queued.identity().clone()),
            ))
            .await
            .is_err()
    );

    let next_sample = PointSample::new(address, 44.0, TimestampMs::new(2), PointQuality::Good);
    live.write(next_sample)
        .await
        .expect("acquisition continues locally");
    assert_eq!(
        live.read(address).await.expect("local read"),
        Some(next_sample)
    );
    assert_eq!(spool.status().await.expect("status").pending_records(), 1);
}
