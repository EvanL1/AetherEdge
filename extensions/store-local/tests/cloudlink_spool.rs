use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
use std::sync::Arc;

use aether_domain::TimestampMs;
use aether_ports::{
    CloudLinkDeliveryState, CloudLinkDurableAck, CloudLinkEnqueue, CloudLinkMessageKind,
    CloudLinkSessionBinding, CloudLinkSpool, CloudLinkSpoolErrorReason, DurableAckOutcome,
};
use aether_store_local::{FileCloudLinkSpool, MemoryCloudLinkSpool};

fn enqueue(batch: &str, digest_byte: char, created_at: u64) -> CloudLinkEnqueue {
    CloudLinkEnqueue::new(
        CloudLinkMessageKind::TelemetryBatch,
        batch,
        format!("sha256:{}", digest_byte.to_string().repeat(64)),
        br#"{"samples":[]}"#.to_vec(),
        TimestampMs::new(created_at),
        None,
    )
}

fn session(id: &str, epoch: u64) -> CloudLinkSessionBinding {
    CloudLinkSessionBinding::new(id, epoch)
}

async fn publish_record(
    spool: &dyn CloudLinkSpool,
    batch: &str,
    digest_byte: char,
    current_session: &CloudLinkSessionBinding,
) -> aether_ports::CloudLinkRecord {
    let record = spool
        .enqueue(enqueue(batch, digest_byte, 100))
        .await
        .expect("enqueue");
    spool
        .mark_offered(record.identity(), current_session)
        .await
        .expect("offer");
    spool
        .mark_transport_published(record.identity(), current_session)
        .await
        .expect("transport publish");
    record
}

fn ack(
    record: &aether_ports::CloudLinkRecord,
    current_session: &CloudLinkSessionBinding,
) -> CloudLinkDurableAck {
    CloudLinkDurableAck::new(
        current_session.clone(),
        record.identity().stream_id(),
        record.identity().stream_epoch(),
        record.identity().position(),
        record.batch_id(),
        record.digest(),
        "receipt-1",
    )
}

async fn assert_spool_conformance(spool: &dyn CloudLinkSpool) {
    let current_session = session("conformance-session", 1);
    let first = spool
        .enqueue(enqueue("conformance-batch", 'd', 1))
        .await
        .expect("enqueue");
    let idempotent = spool
        .enqueue(enqueue("conformance-batch", 'd', 1))
        .await
        .expect("idempotent enqueue");
    assert_eq!(first, idempotent);
    spool
        .mark_offered(first.identity(), &current_session)
        .await
        .expect("offer");
    spool
        .mark_transport_published(first.identity(), &current_session)
        .await
        .expect("publish");
    assert_eq!(spool.status().await.expect("status").pending_records(), 1);
    assert_eq!(
        spool
            .acknowledge(&ack(&first, &current_session))
            .await
            .expect("application ACK"),
        DurableAckOutcome::Applied { removed: 1 }
    );
    assert_eq!(spool.status().await.expect("status").pending_records(), 0);
}

#[tokio::test]
async fn memory_cloudlink_spool_conforms_to_the_port_contract() {
    let spool = MemoryCloudLinkSpool::new("telemetry", 8).expect("memory spool");
    assert_spool_conformance(&spool).await;
}

#[tokio::test]
async fn file_cloudlink_spool_conforms_to_the_port_contract() {
    let root = tempfile::tempdir().expect("temp dir");
    let spool = FileCloudLinkSpool::open(root.path().join("conformance.spool"), "telemetry", 8)
        .expect("file spool");
    assert_spool_conformance(&spool).await;
}

#[tokio::test]
async fn transport_publish_without_application_ack_retains_the_record() {
    let spool = MemoryCloudLinkSpool::new("telemetry", 8).expect("memory spool");
    let current_session = session("session-1", 3);
    let record = publish_record(&spool, "batch-1", 'a', &current_session).await;

    let replay = spool
        .replay_from(record.identity().position(), 8)
        .await
        .expect("replay");
    assert_eq!(replay.records().len(), 1);
    assert_eq!(
        replay.records()[0].state(),
        CloudLinkDeliveryState::TransportPublished
    );
}

#[tokio::test]
async fn file_transport_publish_without_application_ack_survives_restart() {
    let root = tempfile::tempdir().expect("temp dir");
    let path = root.path().join("unacked.spool");
    let original;
    {
        let spool = FileCloudLinkSpool::open(&path, "telemetry", 8).expect("file spool");
        original = publish_record(&spool, "batch-1", 'a', &session("session-1", 3)).await;
    }

    let reopened = FileCloudLinkSpool::open(&path, "telemetry", 8).expect("reopen");
    let replayed = reopened
        .replay_from(original.identity().position(), 1)
        .await
        .expect("replay retained publication")
        .records()[0]
        .clone();
    assert_eq!(replayed.identity(), original.identity());
    assert_eq!(replayed.digest(), original.digest());
    assert_eq!(replayed.state(), CloudLinkDeliveryState::TransportPublished);
}

#[tokio::test]
async fn spool_rejects_unbounded_capacity_unsafe_sessions_and_receipts() {
    assert!(MemoryCloudLinkSpool::new("telemetry", 65_537).is_err());
    let spool = MemoryCloudLinkSpool::new("telemetry", 8).expect("spool");
    let record = spool
        .enqueue(enqueue("batch-1", 'a', 1))
        .await
        .expect("record");
    let unsafe_session = session("session/other", 1);
    assert!(
        spool
            .mark_offered(record.identity(), &unsafe_session)
            .await
            .is_err()
    );

    let current_session = session("session-1", 1);
    spool
        .mark_offered(record.identity(), &current_session)
        .await
        .expect("safe offer");
    let invalid_receipt = CloudLinkDurableAck::new(
        current_session,
        record.identity().stream_id(),
        record.identity().stream_epoch(),
        record.identity().position(),
        record.batch_id(),
        record.digest(),
        "receipt/unsafe",
    );
    assert!(spool.acknowledge(&invalid_receipt).await.is_err());
    assert_eq!(spool.status().await.expect("status").pending_records(), 1);
}

#[tokio::test]
async fn lost_ack_replays_the_same_identity_and_digest() {
    let spool = MemoryCloudLinkSpool::new("telemetry", 8).expect("memory spool");
    let first_session = session("session-1", 3);
    let record = publish_record(&spool, "batch-1", 'a', &first_session).await;

    let replayed = spool
        .replay_from(record.identity().position(), 8)
        .await
        .expect("replay")
        .records()[0]
        .clone();
    assert_eq!(replayed.identity(), record.identity());
    assert_eq!(replayed.batch_id(), record.batch_id());
    assert_eq!(replayed.digest(), record.digest());

    let resumed_session = session("session-2", 4);
    spool
        .mark_offered(replayed.identity(), &resumed_session)
        .await
        .expect("re-offer");
    assert_eq!(
        spool
            .acknowledge(&ack(&replayed, &resumed_session))
            .await
            .expect("durable ACK"),
        DurableAckOutcome::Applied { removed: 1 }
    );
}

#[tokio::test]
async fn duplicate_ack_is_idempotent_but_stale_or_conflicting_ack_fails_closed() {
    let spool = MemoryCloudLinkSpool::new("telemetry", 8).expect("memory spool");
    let current_session = session("session-1", 3);
    let record = publish_record(&spool, "batch-1", 'a', &current_session).await;
    let valid = ack(&record, &current_session);

    assert_eq!(
        spool.acknowledge(&valid).await.expect("first ACK"),
        DurableAckOutcome::Applied { removed: 1 }
    );
    assert_eq!(
        spool.acknowledge(&valid).await.expect("duplicate ACK"),
        DurableAckOutcome::Duplicate
    );

    let second = publish_record(&spool, "batch-2", 'b', &current_session).await;
    for invalid in [
        CloudLinkDurableAck::new(
            session("session-old", 2),
            second.identity().stream_id(),
            second.identity().stream_epoch(),
            second.identity().position(),
            second.batch_id(),
            second.digest(),
            "receipt-old",
        ),
        CloudLinkDurableAck::new(
            current_session.clone(),
            "another-stream",
            second.identity().stream_epoch(),
            second.identity().position(),
            second.batch_id(),
            second.digest(),
            "receipt-wrong-stream",
        ),
        CloudLinkDurableAck::new(
            current_session.clone(),
            second.identity().stream_id(),
            second.identity().stream_epoch(),
            second.identity().position(),
            "another-batch",
            second.digest(),
            "receipt-wrong-batch",
        ),
        CloudLinkDurableAck::new(
            current_session.clone(),
            second.identity().stream_id(),
            second.identity().stream_epoch(),
            second.identity().position(),
            second.batch_id(),
            format!("sha256:{}", "c".repeat(64)),
            "receipt-wrong-digest",
        ),
    ] {
        let error = spool.acknowledge(&invalid).await.expect_err("invalid ACK");
        assert!(matches!(
            error.reason(),
            Some(
                CloudLinkSpoolErrorReason::StaleSession
                    | CloudLinkSpoolErrorReason::WrongStream
                    | CloudLinkSpoolErrorReason::ConflictingIdentity
            )
        ));
    }

    assert_eq!(spool.status().await.expect("status").pending_records(), 1);
}

#[tokio::test]
async fn replay_gap_and_capacity_overflow_produce_explicit_loss_evidence() {
    let spool = MemoryCloudLinkSpool::new("telemetry", 2).expect("memory spool");
    let first = spool
        .enqueue(enqueue("batch-1", 'a', 1))
        .await
        .expect("first");
    spool
        .enqueue(enqueue("batch-2", 'b', 2))
        .await
        .expect("second");
    let third = spool
        .enqueue(enqueue("batch-3", 'c', 3))
        .await
        .expect("third");

    let unavailable = spool
        .replay_from(first.identity().position(), 10)
        .await
        .expect("loss window");
    assert!(unavailable.records().is_empty());
    let loss = unavailable.data_loss().expect("data-loss evidence");
    assert_eq!(loss.first_lost_position(), first.identity().position());
    assert_eq!(loss.last_lost_position(), first.identity().position());
    assert_eq!(
        loss.earliest_retained_position(),
        third.identity().position() - 1
    );

    let available = spool
        .replay_from(loss.earliest_retained_position(), 10)
        .await
        .expect("available window");
    assert_eq!(available.records().len(), 2);
}

#[tokio::test]
async fn file_spool_persists_positions_epoch_and_ack_state_across_restart() {
    let root = tempfile::tempdir().expect("temp dir");
    let path = root.path().join("cloudlink.spool");
    let first_identity;
    {
        let spool = FileCloudLinkSpool::open(&path, "telemetry", 8).expect("open");
        first_identity = spool
            .enqueue(enqueue("batch-1", 'a', 1))
            .await
            .expect("first")
            .identity()
            .clone();
    }
    {
        let spool = FileCloudLinkSpool::open(&path, "telemetry", 8).expect("reopen");
        let second = spool
            .enqueue(enqueue("batch-2", 'b', 2))
            .await
            .expect("second");
        assert_eq!(
            second.identity().stream_epoch(),
            first_identity.stream_epoch()
        );
        assert_eq!(second.identity().position(), first_identity.position() + 1);
    }
    {
        let spool = FileCloudLinkSpool::open(&path, "telemetry", 8).expect("reopen");
        let status = spool.status().await.expect("status");
        assert_eq!(status.next_position(), first_identity.position() + 2);
        assert_eq!(status.pending_records(), 2);
    }
}

#[tokio::test]
async fn file_spool_persists_application_ack_idempotency_across_restart() {
    let root = tempfile::tempdir().expect("temp dir");
    let path = root.path().join("acked.spool");
    let durable_ack;
    {
        let spool = FileCloudLinkSpool::open(&path, "telemetry", 8).expect("open");
        let current_session = session("session-1", 3);
        let record = publish_record(&spool, "batch-1", 'a', &current_session).await;
        durable_ack = ack(&record, &current_session);
        assert_eq!(
            spool
                .acknowledge(&durable_ack)
                .await
                .expect("application ACK"),
            DurableAckOutcome::Applied { removed: 1 }
        );
    }

    let reopened = FileCloudLinkSpool::open(&path, "telemetry", 8).expect("reopen");
    assert_eq!(
        reopened
            .acknowledge(&durable_ack)
            .await
            .expect("duplicate after restart"),
        DurableAckOutcome::Duplicate
    );
    assert_eq!(
        reopened
            .status()
            .await
            .expect("status")
            .last_acknowledged_position(),
        1
    );
}

#[tokio::test]
async fn file_spool_recovers_a_truncated_tail_and_fails_closed_on_mid_log_corruption() {
    let root = tempfile::tempdir().expect("temp dir");
    let tail_path = root.path().join("tail.spool");
    {
        let spool = FileCloudLinkSpool::open(&tail_path, "telemetry", 8).expect("open");
        spool
            .enqueue(enqueue("batch-1", 'a', 1))
            .await
            .expect("first");
        spool
            .enqueue(enqueue("batch-2", 'b', 2))
            .await
            .expect("second");
    }
    let length = std::fs::metadata(&tail_path).expect("metadata").len();
    OpenOptions::new()
        .write(true)
        .open(&tail_path)
        .expect("journal")
        .set_len(length - 7)
        .expect("truncate tail");
    let recovered = FileCloudLinkSpool::open(&tail_path, "telemetry", 8)
        .expect("torn final mutation is discarded");
    assert_eq!(
        recovered.status().await.expect("status").pending_records(),
        1
    );
    drop(recovered);

    let corrupt_path = root.path().join("corrupt.spool");
    {
        let spool = FileCloudLinkSpool::open(&corrupt_path, "telemetry", 8).expect("open");
        for (batch, byte) in [("batch-1", 'a'), ("batch-2", 'b'), ("batch-3", 'c')] {
            spool
                .enqueue(enqueue(batch, byte, 1))
                .await
                .expect("append");
        }
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&corrupt_path)
        .expect("journal");
    let length = file.seek(SeekFrom::End(0)).expect("length");
    file.seek(SeekFrom::Start(length / 2)).expect("middle");
    file.write_all(&[0xff]).expect("corrupt byte");
    file.sync_all().expect("sync corruption");

    let error = FileCloudLinkSpool::open(&corrupt_path, "telemetry", 8)
        .expect_err("mid-log corruption must fail closed");
    assert!(error.to_string().contains("corrupt"));
}

#[tokio::test]
async fn file_spool_compaction_preserves_live_identity_and_complete_tail_corruption_fails_closed() {
    let root = tempfile::tempdir().expect("temp dir");
    let path = root.path().join("compact.spool");
    let identity;
    {
        let spool = FileCloudLinkSpool::open(&path, "telemetry", 8).expect("open");
        let record = spool
            .enqueue(enqueue("batch-1", 'a', 1))
            .await
            .expect("enqueue");
        identity = record.identity().clone();
        for epoch in 1..=20 {
            spool
                .mark_offered(
                    record.identity(),
                    &session(&format!("session-{epoch}"), epoch),
                )
                .await
                .expect("re-offer");
        }
        let before = std::fs::metadata(&path).expect("metadata").len();
        spool.compact().expect("compact live state");
        let after = std::fs::metadata(&path).expect("metadata").len();
        assert!(after < before);
    }

    let reopened = FileCloudLinkSpool::open(&path, "telemetry", 8).expect("reopen compacted");
    let replayed = reopened
        .replay_from(identity.position(), 1)
        .await
        .expect("replay compacted record")
        .records()[0]
        .clone();
    assert_eq!(replayed.identity(), &identity);
    drop(reopened);

    let length = std::fs::metadata(&path).expect("metadata").len();
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .expect("journal");
    file.seek(SeekFrom::Start(length - 1))
        .expect("last CRC byte");
    file.write_all(&[0xff]).expect("corrupt complete record");
    file.sync_all().expect("sync corruption");

    let error = FileCloudLinkSpool::open(&path, "telemetry", 8)
        .expect_err("complete final record corruption must fail closed");
    assert_eq!(
        error.reason(),
        Some(CloudLinkSpoolErrorReason::CorruptJournal)
    );
}

#[tokio::test]
async fn epoch_rotation_is_explicit_requires_an_empty_spool_and_survives_restart() {
    let root = tempfile::tempdir().expect("temp dir");
    let path = root.path().join("rotate.spool");
    let spool = Arc::new(FileCloudLinkSpool::open(&path, "telemetry", 8).expect("open"));
    spool
        .enqueue(enqueue("batch-1", 'a', 1))
        .await
        .expect("record");
    assert!(spool.rotate_stream_epoch().await.is_err());

    let current_session = session("session-1", 1);
    let record = spool.replay_from(1, 1).await.expect("replay").records()[0].clone();
    spool
        .mark_offered(record.identity(), &current_session)
        .await
        .expect("offer");
    spool
        .acknowledge(&ack(&record, &current_session))
        .await
        .expect("ACK");
    let epoch = spool.rotate_stream_epoch().await.expect("rotate");
    assert_eq!(epoch, 2);
    drop(spool);

    let reopened = FileCloudLinkSpool::open(&path, "telemetry", 8).expect("reopen");
    let record = reopened
        .enqueue(enqueue("batch-2", 'b', 2))
        .await
        .expect("record in new epoch");
    assert_eq!(record.identity().stream_epoch(), 2);
    assert_eq!(record.identity().position(), 1);
}
