use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use aether_application::OutboxForwarder;
use aether_domain::TimestampMs;
use aether_ports::{
    DurableOutbox, OutboxMessage, PortError, PortErrorKind, PortResult, UplinkPublisher,
};
use aether_store_local::MemoryOutbox;
use async_trait::async_trait;

#[derive(Default)]
struct RecordingPublisher {
    fail: AtomicBool,
    published: Mutex<Vec<OutboxMessage>>,
}

#[async_trait]
impl UplinkPublisher for RecordingPublisher {
    async fn publish(&self, message: &OutboxMessage) -> PortResult<()> {
        if self.fail.load(Ordering::Relaxed) {
            return Err(PortError::new(
                PortErrorKind::Unavailable,
                "uplink is offline",
            ));
        }
        self.published
            .lock()
            .expect("publisher lock")
            .push(message.clone());
        Ok(())
    }
}

fn message(sequence: u64) -> OutboxMessage {
    OutboxMessage::new(
        "mqtt/telemetry",
        format!("payload-{sequence}").into_bytes(),
        TimestampMs::new(sequence),
    )
}

#[tokio::test]
async fn drain_publishes_in_fifo_order_and_acknowledges_each_success() {
    let outbox: Arc<dyn DurableOutbox> = Arc::new(MemoryOutbox::with_capacity(4));
    outbox.enqueue(message(1)).await.expect("enqueue first");
    outbox.enqueue(message(2)).await.expect("enqueue second");
    let publisher = Arc::new(RecordingPublisher::default());
    let forwarder = OutboxForwarder::new(Arc::clone(&outbox), publisher.clone());

    let report = forwarder.drain_once(4).await.expect("drain outbox");

    assert_eq!(report.delivered(), 2);
    assert_eq!(report.examined(), 2);
    assert!(
        outbox
            .peek(4)
            .await
            .expect("peek drained outbox")
            .is_empty()
    );
    assert_eq!(
        publisher
            .published
            .lock()
            .expect("publisher lock")
            .iter()
            .map(|item| item.payload())
            .collect::<Vec<_>>(),
        vec![b"payload-1".as_slice(), b"payload-2".as_slice()]
    );
}

#[tokio::test]
async fn retryable_publish_failure_leaves_entry_durable_for_next_tick() {
    let outbox: Arc<dyn DurableOutbox> = Arc::new(MemoryOutbox::with_capacity(2));
    let id = outbox.enqueue(message(1)).await.expect("enqueue message");
    let publisher = Arc::new(RecordingPublisher::default());
    publisher.fail.store(true, Ordering::Relaxed);
    let forwarder = OutboxForwarder::new(Arc::clone(&outbox), publisher);

    let error = forwarder
        .drain_once(2)
        .await
        .expect_err("offline publisher must fail");

    assert!(error.is_retryable());
    assert_eq!(
        outbox.peek(2).await.expect("peek retained entry")[0].id(),
        id
    );
}
