use aether_domain::{
    InstanceId, PointAddress, PointId, PointKind, PointQuality, PointSample, TimestampMs,
};
use aether_ports::{
    AuditOutcome, AuditRecord, AuditSink, DurableOutbox, HistorySink, LiveState, LiveStateWriter,
};
#[cfg(feature = "sqlite-audit")]
use aether_store_local::SqliteAuditSink;
use aether_store_local::{MemoryAuditSink, MemoryHistorySink, MemoryLiveState, MemoryOutbox};
use aether_testkit::{assert_live_state_round_trip, assert_outbox_fifo};

fn address(point_id: u32) -> PointAddress {
    PointAddress::new(
        InstanceId::new(5),
        PointKind::Telemetry,
        PointId::new(point_id),
    )
}

fn sample(point_id: u32, value: f64) -> PointSample {
    PointSample::new(
        address(point_id),
        value,
        TimestampMs::new(1_000 + u64::from(point_id)),
        PointQuality::Good,
    )
}

#[tokio::test]
async fn memory_live_state_round_trips_samples_and_preserves_batch_order() {
    let state = MemoryLiveState::new();
    state.write(sample(1, 10.0)).await.unwrap();
    state.write(sample(2, 20.0)).await.unwrap();

    assert_eq!(state.read(address(1)).await.unwrap(), Some(sample(1, 10.0)));
    assert_eq!(state.read(address(99)).await.unwrap(), None);
    assert_eq!(
        state
            .read_many(&[address(2), address(99), address(1)])
            .await
            .unwrap(),
        vec![Some(sample(2, 20.0)), None, Some(sample(1, 10.0))]
    );
}

#[tokio::test]
async fn memory_history_and_audit_keep_append_order() {
    let history = MemoryHistorySink::new();
    assert_eq!(
        history
            .append(&[sample(1, 10.0), sample(2, 20.0)])
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        history.samples().unwrap(),
        vec![sample(1, 10.0), sample(2, 20.0)]
    );

    let audit = MemoryAuditSink::new();
    let record = AuditRecord::new(
        "request-1",
        "agent-1",
        "device.write_point",
        AuditOutcome::Succeeded,
        TimestampMs::new(2_000),
        None,
    );
    audit.record(record.clone()).await.unwrap();
    assert_eq!(audit.records().unwrap(), vec![record]);
}

#[cfg(feature = "sqlite-audit")]
#[tokio::test]
async fn sqlite_audit_sink_initializes_and_persists_ordered_events() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let audit = SqliteAuditSink::initialize(pool.clone()).await.unwrap();
    for outcome in [AuditOutcome::Attempted, AuditOutcome::Succeeded] {
        audit
            .record(AuditRecord::new(
                "request-sqlite",
                "local:test",
                "device.write_point",
                outcome,
                TimestampMs::new(2_000),
                None,
            ))
            .await
            .unwrap();
    }

    let outcomes: Vec<String> = sqlx::query_scalar(
        "SELECT outcome FROM command_audit_events WHERE request_id = ? ORDER BY id",
    )
    .bind("request-sqlite")
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(outcomes, vec!["attempted", "succeeded"]);
}

#[tokio::test]
async fn bounded_memory_outbox_is_fifo_and_acknowledges_by_id() {
    let outbox = MemoryOutbox::with_capacity(2);
    let first = outbox
        .enqueue(aether_ports::OutboxMessage::new(
            "telemetry/site-a",
            b"one".to_vec(),
            TimestampMs::new(1),
        ))
        .await
        .unwrap();
    let second = outbox
        .enqueue(aether_ports::OutboxMessage::new(
            "telemetry/site-a",
            b"two".to_vec(),
            TimestampMs::new(2),
        ))
        .await
        .unwrap();

    let full = outbox
        .enqueue(aether_ports::OutboxMessage::new(
            "telemetry/site-a",
            b"three".to_vec(),
            TimestampMs::new(3),
        ))
        .await
        .expect_err("bounded queue rejects overflow");
    assert!(full.is_retryable());

    let pending = outbox.peek(10).await.unwrap();
    assert_eq!(
        pending.iter().map(|entry| entry.id()).collect::<Vec<_>>(),
        vec![first, second]
    );
    assert_eq!(pending[0].message().payload(), b"one");

    assert_eq!(outbox.acknowledge(&[first]).await.unwrap(), 1);
    let remaining = outbox.peek(10).await.unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id(), second);
}

#[tokio::test]
async fn local_adapters_pass_the_public_extension_conformance_suite() {
    let state = MemoryLiveState::new();
    assert_live_state_round_trip(&state, &state, sample(7, 70.0), sample(8, 80.0))
        .await
        .unwrap();

    assert_outbox_fifo(
        &MemoryOutbox::with_capacity(2),
        aether_ports::OutboxMessage::new("uplink", b"first".to_vec(), TimestampMs::new(1)),
        aether_ports::OutboxMessage::new("uplink", b"second".to_vec(), TimestampMs::new(2)),
    )
    .await
    .unwrap();
}
