use aether_ports::{
    CloudLinkRecordIdentity, CloudLinkTransport, CloudLinkTransportEvent,
    CloudLinkTransportMessage, CloudLinkTransportRoute,
};
use aether_testkit::MemoryCloudLinkTransport;

#[tokio::test]
async fn memory_transport_conforms_without_inventing_application_ack() {
    let (edge, cloud) = MemoryCloudLinkTransport::pair(8).expect("transport pair");
    assert_eq!(
        edge.receive().await.expect("edge connected"),
        CloudLinkTransportEvent::Connected
    );
    assert_eq!(
        cloud.receive().await.expect("cloud connected"),
        CloudLinkTransportEvent::Connected
    );

    let identity = CloudLinkRecordIdentity::new("telemetry", 1, 1);
    edge.send(CloudLinkTransportMessage::new(
        CloudLinkTransportRoute::TelemetryUp,
        b"business".to_vec(),
        Some(identity.clone()),
    ))
    .await
    .expect("send");

    assert!(matches!(
        cloud.receive().await.expect("cloud inbound"),
        CloudLinkTransportEvent::Inbound(_)
    ));
    assert_eq!(
        edge.receive().await.expect("transport evidence"),
        CloudLinkTransportEvent::TransportPublished(identity)
    );
    // No further event exists until the fake cloud explicitly sends one. In
    // particular, transport publication did not fabricate a durable ACK.

    edge.disconnect().await.expect("disconnect");
    assert_eq!(
        edge.receive().await.expect("edge disconnected"),
        CloudLinkTransportEvent::Disconnected
    );
    assert_eq!(
        cloud.receive().await.expect("cloud disconnected"),
        CloudLinkTransportEvent::Disconnected
    );
}
