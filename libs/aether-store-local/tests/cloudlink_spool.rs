use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
use std::sync::Arc;

use aether_domain::TimestampMs;
use aether_ports::{
    CloudLinkDeliveryState, CloudLinkDurableAck, CloudLinkEnqueue, CloudLinkMessageKind,
    CloudLinkReceiptRetention, CloudLinkSessionBinding, CloudLinkSpool, CloudLinkSpoolErrorReason,
    DurableAckOutcome,
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

fn enqueue_with_payload(
    kind: CloudLinkMessageKind,
    batch: &str,
    digest_byte: char,
    payload_bytes: usize,
    created_at: u64,
) -> CloudLinkEnqueue {
    CloudLinkEnqueue::new(
        kind,
        batch,
        format!("sha256:{}", digest_byte.to_string().repeat(64)),
        vec![b'x'; payload_bytes],
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

async fn offer_retained_through(
    spool: &dyn CloudLinkSpool,
    terminal: &aether_ports::CloudLinkRecord,
    current_session: &CloudLinkSessionBinding,
) {
    let status = spool
        .status()
        .await
        .expect("status before cumulative offer");
    let retained = spool
        .replay_from(
            status.earliest_retained_position(),
            status.pending_records(),
        )
        .await
        .expect("retained cumulative prefix");
    for record in retained
        .records()
        .iter()
        .take_while(|record| record.identity().position() <= terminal.identity().position())
    {
        spool
            .mark_offered(record.identity(), current_session)
            .await
            .expect("offer cumulative prefix");
    }
}

async fn assert_spool_conformance(spool: &dyn CloudLinkSpool) {
    let current_session = session("conformance-session", 1);
    let lossless = CloudLinkEnqueue::new(
        CloudLinkMessageKind::AlarmEvent,
        "conformance-batch",
        format!("sha256:{}", "d".repeat(64)),
        br#"{"type":"alarm"}"#.to_vec(),
        TimestampMs::new(1),
        None,
    );
    let first = spool
        .admit_lossless(
            lossless.clone(),
            CloudLinkReceiptRetention::RetainForIdempotency,
        )
        .await
        .expect("lossless admission")
        .pending_record()
        .expect("new pending record")
        .clone();
    let idempotent = spool
        .admit_lossless(lossless, CloudLinkReceiptRetention::RetainForIdempotency)
        .await
        .expect("idempotent lossless admission");
    assert_eq!(idempotent.pending_record(), Some(&first));
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

    let duplicate = spool
        .admit_lossless(
            CloudLinkEnqueue::new(
                CloudLinkMessageKind::AlarmEvent,
                "conformance-batch",
                format!("sha256:{}", "d".repeat(64)),
                br#"{"type":"alarm"}"#.to_vec(),
                TimestampMs::new(999),
                None,
            ),
            CloudLinkReceiptRetention::RetainForIdempotency,
        )
        .await
        .expect("lossless retry after ACK");
    assert!(duplicate.duplicate());
    assert!(duplicate.acknowledged_duplicate());
    assert!(duplicate.pending_record().is_none());
    assert_eq!(duplicate.identity(), first.identity());
    assert_eq!(
        spool
            .admit_lossless(
                CloudLinkEnqueue::new(
                    CloudLinkMessageKind::AlarmEvent,
                    "conformance-batch",
                    format!("sha256:{}", "e".repeat(64)),
                    br#"{"type":"alarm"}"#.to_vec(),
                    TimestampMs::new(999),
                    None,
                ),
                CloudLinkReceiptRetention::RetainForIdempotency,
            )
            .await
            .expect_err("conflicting retry after ACK")
            .reason(),
        Some(CloudLinkSpoolErrorReason::ConflictingIdentity)
    );
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

#[test]
fn retired_numbered_spool_magic_is_rejected_without_rewriting_the_file() {
    let root = tempfile::tempdir().expect("temp dir");
    let path = root.path().join("retired.spool");
    let retired = b"AETHCL2\nretired-format";
    std::fs::write(&path, retired).expect("retired spool fixture");

    let error = FileCloudLinkSpool::open(&path, "telemetry", 8)
        .expect_err("retired numbered format must fail closed");
    assert_eq!(
        error.reason(),
        Some(CloudLinkSpoolErrorReason::CorruptJournal)
    );
    assert_eq!(std::fs::read(&path).expect("unchanged fixture"), retired);
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
async fn cumulative_ack_never_removes_an_unoffered_earlier_record() {
    let spool = MemoryCloudLinkSpool::new("telemetry", 8).expect("memory spool");
    let current_session = session("session-1", 3);
    let earlier = spool
        .admit_lossless(
            enqueue("ordinary-before-terminal", 'a', 1),
            CloudLinkReceiptRetention::DiscardAfterAck,
        )
        .await
        .expect("earlier admission")
        .pending_record()
        .expect("earlier record")
        .clone();
    let terminal = spool
        .admit_lossless(
            enqueue_with_payload(
                CloudLinkMessageKind::DataLoss,
                "terminal-report",
                'b',
                16,
                2,
            ),
            CloudLinkReceiptRetention::DiscardAfterAck,
        )
        .await
        .expect("terminal admission")
        .pending_record()
        .expect("terminal record")
        .clone();
    spool
        .mark_offered(terminal.identity(), &current_session)
        .await
        .expect("offer only terminal");

    let rejected = spool
        .acknowledge(&ack(&terminal, &current_session))
        .await
        .expect_err("terminal-only ACK must not delete an unoffered prefix");
    assert_eq!(
        rejected.reason(),
        Some(CloudLinkSpoolErrorReason::StaleSession)
    );
    let status = spool.status().await.expect("unchanged status");
    assert_eq!(status.last_acknowledged_position(), 0);
    assert_eq!(status.pending_records(), 2);
    let retained = spool.replay_from(1, 2).await.expect("retained prefix");
    assert_eq!(retained.records()[0].identity(), earlier.identity());
    assert_eq!(retained.records()[1].identity(), terminal.identity());

    spool
        .mark_offered(earlier.identity(), &current_session)
        .await
        .expect("offer prefix");
    assert_eq!(
        spool
            .acknowledge(&ack(&terminal, &current_session))
            .await
            .expect("fully offered cumulative ACK"),
        DurableAckOutcome::Applied { removed: 2 }
    );
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
        let record = spool
            .admit_lossless(
                enqueue("batch-1", 'a', 1),
                CloudLinkReceiptRetention::RetainForIdempotency,
            )
            .await
            .expect("lossless admission")
            .pending_record()
            .expect("pending record")
            .clone();
        spool
            .mark_offered(record.identity(), &current_session)
            .await
            .expect("offer");
        spool
            .mark_transport_published(record.identity(), &current_session)
            .await
            .expect("transport publish");
        durable_ack = ack(&record, &current_session);
        assert_eq!(
            spool
                .acknowledge(&durable_ack)
                .await
                .expect("application ACK"),
            DurableAckOutcome::Applied { removed: 1 }
        );
        spool.compact().expect("compact protected receipt");
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
    let duplicate = reopened
        .admit_lossless(
            enqueue("batch-1", 'a', 999),
            CloudLinkReceiptRetention::RetainForIdempotency,
        )
        .await
        .expect("business retry after restart");
    assert!(duplicate.duplicate());
    assert!(duplicate.acknowledged_duplicate());
    assert_eq!(duplicate.identity().position(), 1);
    assert_eq!(
        reopened.status().await.expect("status").pending_records(),
        0
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

#[tokio::test]
async fn live_byte_quota_fails_closed_before_record_count_and_survives_reopen() {
    let root = tempfile::tempdir().expect("temp dir");
    let path = root.path().join("byte-quota.spool");
    let first;
    {
        let spool =
            FileCloudLinkSpool::open_with_limits(&path, "telemetry", 8, 8, 64 * 1024, 128 * 1024)
                .expect("bounded spool");
        first = spool
            .admit_lossless(
                enqueue_with_payload(
                    CloudLinkMessageKind::AlarmEvent,
                    "large-alarm",
                    'a',
                    5 * 1024,
                    1,
                ),
                CloudLinkReceiptRetention::RetainForIdempotency,
            )
            .await
            .expect("first admission")
            .pending_record()
            .expect("pending record")
            .clone();
        let rejected = spool
            .admit_lossless(
                enqueue_with_payload(
                    CloudLinkMessageKind::TelemetryBatch,
                    "second-large-record",
                    'b',
                    5 * 1024,
                    2,
                ),
                CloudLinkReceiptRetention::DiscardAfterAck,
            )
            .await
            .expect_err("byte ceiling must reject before count capacity");
        assert_eq!(
            rejected.reason(),
            Some(CloudLinkSpoolErrorReason::CapacityExceeded)
        );
        let duplicate = spool
            .admit_lossless(
                enqueue_with_payload(
                    CloudLinkMessageKind::AlarmEvent,
                    "large-alarm",
                    'a',
                    5 * 1024,
                    99,
                ),
                CloudLinkReceiptRetention::RetainForIdempotency,
            )
            .await
            .expect("exact duplicate at byte ceiling");
        assert!(duplicate.duplicate());
        let status = spool.status().await.expect("status");
        assert_eq!(status.ordinary_pending_records(), 1);
        assert_eq!(status.record_capacity(), 8);
        assert!(status.current_live_bytes() <= status.ordinary_max_live_bytes());
        assert_eq!(status.quota_rejections(), 1);
    }

    let reopened =
        FileCloudLinkSpool::open_with_limits(&path, "telemetry", 8, 8, 64 * 1024, 128 * 1024)
            .expect("reopen bounded spool");
    let before_ack = reopened.status().await.expect("reopened status");
    assert_eq!(before_ack.ordinary_pending_records(), 1);
    assert!(before_ack.current_live_bytes() > 5 * 1024);
    let current_session = session("byte-quota-session", 1);
    reopened
        .mark_offered(first.identity(), &current_session)
        .await
        .expect("offer alarm");
    reopened
        .acknowledge(&ack(&first, &current_session))
        .await
        .expect("ack alarm");
    let after_ack = reopened.status().await.expect("post-ACK status");
    assert_eq!(after_ack.pending_records(), 0);
    assert_eq!(after_ack.acknowledged_receipts(), 1);
    assert!(after_ack.current_live_bytes() < before_ack.current_live_bytes());
    assert!(after_ack.current_live_bytes() > 0);
}

#[tokio::test]
async fn reopening_with_a_smaller_limit_cannot_consume_the_data_loss_reserve() {
    let root = tempfile::tempdir().expect("temp dir");
    let path = root.path().join("shrink-reserve.spool");
    {
        let spool =
            FileCloudLinkSpool::open_with_limits(&path, "telemetry", 8, 8, 96 * 1024, 160 * 1024)
                .expect("larger limit");
        spool
            .admit_lossless(
                enqueue_with_payload(
                    CloudLinkMessageKind::TelemetryBatch,
                    "uses-future-reserve",
                    'a',
                    10 * 1024,
                    1,
                ),
                CloudLinkReceiptRetention::DiscardAfterAck,
            )
            .await
            .expect("admission below original ordinary limit");
        let status = spool.status().await.expect("status");
        assert!(status.current_live_bytes() > 32 * 1024);
        assert!(status.current_live_bytes() < 64 * 1024);
    }

    let error =
        FileCloudLinkSpool::open_with_limits(&path, "telemetry", 8, 8, 64 * 1024, 128 * 1024)
            .expect_err("shrinking may not consume the system reserve");
    assert_eq!(error.reason(), Some(CloudLinkSpoolErrorReason::InvalidData));
}

#[tokio::test]
async fn data_loss_system_slot_survives_reopen_and_clears_only_the_matching_range() {
    let root = tempfile::tempdir().expect("temp dir");
    let path = root.path().join("data-loss.spool");
    let first_report;
    {
        let spool = FileCloudLinkSpool::open(&path, "telemetry", 2).expect("spool");
        for (batch, byte) in [("one", 'a'), ("two", 'b'), ("three", 'c')] {
            spool
                .enqueue(enqueue(batch, byte, 1))
                .await
                .expect("enqueue");
        }
        let evidence = spool
            .status()
            .await
            .expect("status")
            .data_loss()
            .expect("overflow evidence")
            .clone();
        first_report = spool
            .admit_data_loss(
                enqueue_with_payload(CloudLinkMessageKind::DataLoss, "loss-1-1", 'd', 16, 2),
                &evidence,
            )
            .await
            .expect("reserved report admission")
            .pending_record()
            .expect("pending report")
            .clone();
        let status = spool.status().await.expect("status");
        assert_eq!(status.ordinary_pending_records(), 2);
        assert_eq!(status.system_pending_records(), 1);
        assert_eq!(status.pending_records(), 3);
    }

    let reopened = FileCloudLinkSpool::open(&path, "telemetry", 2).expect("reopen");
    assert_eq!(
        reopened
            .status()
            .await
            .expect("reopened status")
            .system_pending_records(),
        1
    );
    reopened
        .enqueue(enqueue("four", 'e', 3))
        .await
        .expect("extend loss range");
    let extended = reopened
        .status()
        .await
        .expect("extended status")
        .data_loss()
        .expect("extended evidence")
        .clone();
    assert_eq!(extended.last_lost_position(), 2);

    let current_session = session("loss-session", 1);
    offer_retained_through(&reopened, &first_report, &current_session).await;
    reopened
        .acknowledge(&ack(&first_report, &current_session))
        .await
        .expect("ack stale report");
    let retained = reopened.status().await.expect("retained evidence");
    assert_eq!(retained.system_pending_records(), 0);
    assert_eq!(retained.data_loss(), Some(&extended));

    reopened
        .enqueue(enqueue("five", '0', 4))
        .await
        .expect("fill the ordinary window after the stale report ACK");
    let before_gap = reopened.status().await.expect("pre-gap status");
    let rejected = reopened
        .enqueue(enqueue("six", '1', 5))
        .await
        .expect_err("a non-contiguous loss must not replace unresolved evidence");
    assert_eq!(
        rejected.reason(),
        Some(CloudLinkSpoolErrorReason::CapacityExceeded)
    );
    let after_gap = reopened.status().await.expect("post-gap status");
    assert_eq!(after_gap.data_loss(), Some(&extended));
    assert_eq!(after_gap.next_position(), before_gap.next_position());
    assert_eq!(
        after_gap.ordinary_pending_records(),
        before_gap.ordinary_pending_records()
    );

    let second_report = reopened
        .admit_data_loss(
            enqueue_with_payload(CloudLinkMessageKind::DataLoss, "loss-1-2", 'f', 16, 4),
            &extended,
        )
        .await
        .expect("extended report admission")
        .pending_record()
        .expect("second report")
        .clone();
    offer_retained_through(&reopened, &second_report, &current_session).await;
    reopened
        .acknowledge(&CloudLinkDurableAck::new(
            current_session,
            second_report.identity().stream_id(),
            second_report.identity().stream_epoch(),
            second_report.identity().position(),
            second_report.batch_id(),
            second_report.digest(),
            "receipt-current-loss",
        ))
        .await
        .expect("ack current report");
    assert!(
        reopened
            .status()
            .await
            .expect("cleared status")
            .data_loss()
            .is_none()
    );
}

#[tokio::test]
async fn physical_journal_stays_bounded_under_repeated_delivery_state_mutations() {
    let root = tempfile::tempdir().expect("temp dir");
    let path = root.path().join("physical-quota.spool");
    let spool =
        FileCloudLinkSpool::open_with_limits(&path, "telemetry", 8, 8, 64 * 1024, 128 * 1024)
            .expect("bounded spool");
    let record = spool
        .admit_lossless(
            enqueue("delivery-state", 'a', 1),
            CloudLinkReceiptRetention::DiscardAfterAck,
        )
        .await
        .expect("record")
        .pending_record()
        .expect("pending")
        .clone();
    for epoch in 1..=1_000 {
        spool
            .mark_offered(
                record.identity(),
                &session(&format!("session-{epoch}"), epoch),
            )
            .await
            .expect("delivery mutation");
    }
    let status = spool.status().await.expect("status");
    assert!(status.journal_bytes() <= status.max_journal_bytes());
    assert_eq!(status.max_journal_bytes(), 128 * 1024);
    assert_eq!(status.quota_rejections(), 0);
    assert_eq!(
        std::fs::metadata(&path).expect("metadata").len(),
        status.journal_bytes()
    );
}

#[tokio::test]
async fn journal_and_stable_lock_path_replacement_poison_the_live_owner() {
    let root = tempfile::tempdir().expect("temp dir");
    let path = root.path().join("identity.spool");
    let spool = FileCloudLinkSpool::open(&path, "telemetry", 8).expect("spool");
    assert!(FileCloudLinkSpool::open(&path, "telemetry", 8).is_err());

    let replacement = root.path().join("same-length-replacement");
    std::fs::copy(&path, &replacement).expect("copy same-length journal");
    std::fs::rename(&replacement, &path).expect("replace journal path");
    let error = spool
        .status()
        .await
        .expect_err("replacement must poison owner");
    assert_eq!(
        error.reason(),
        Some(CloudLinkSpoolErrorReason::CorruptJournal)
    );
    assert!(spool.status().await.is_err());

    let lock_path = root.path().join("lock-identity.spool");
    let lock_spool = FileCloudLinkSpool::open(&lock_path, "telemetry", 8).expect("lock spool");
    let stable_lock = root.path().join("lock-identity.spool.lock");
    std::fs::remove_file(&stable_lock).expect("unlink stable lock");
    std::fs::write(&stable_lock, []).expect("replace stable lock");
    let error = lock_spool
        .status()
        .await
        .expect_err("stable lock replacement must poison owner");
    assert_eq!(
        error.reason(),
        Some(CloudLinkSpoolErrorReason::CorruptJournal)
    );
}

#[test]
fn unknown_checkpoint_field_is_rejected_without_rewriting_the_journal() {
    fn checksum(bytes: &[u8]) -> u32 {
        let mut crc = u32::MAX;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                let mask = 0_u32.wrapping_sub(crc & 1);
                crc = (crc >> 1) ^ (0xedb8_8320 & mask);
            }
        }
        !crc
    }

    let root = tempfile::tempdir().expect("temp dir");
    let path = root.path().join("unknown-field.spool");
    drop(FileCloudLinkSpool::open(&path, "telemetry", 8).expect("spool"));
    let original = std::fs::read(&path).expect("journal");
    let payload_len = u32::from_le_bytes(original[8..12].try_into().expect("length")) as usize;
    let payload = &original[12..12 + payload_len];
    let mut value: serde_json::Value = serde_json::from_slice(payload).expect("checkpoint JSON");
    value
        .as_object_mut()
        .expect("checkpoint object")
        .insert("unexpected".to_owned(), serde_json::Value::Bool(true));
    let payload = serde_json::to_vec(&value).expect("mutated checkpoint");
    let mut mutated = Vec::new();
    mutated.extend_from_slice(&original[..8]);
    mutated.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    mutated.extend_from_slice(&payload);
    mutated.extend_from_slice(&checksum(&payload).to_le_bytes());
    std::fs::write(&path, &mutated).expect("write unknown field fixture");

    let error = FileCloudLinkSpool::open(&path, "telemetry", 8)
        .expect_err("unknown journal field must fail closed");
    assert_eq!(
        error.reason(),
        Some(CloudLinkSpoolErrorReason::CorruptJournal)
    );
    assert_eq!(std::fs::read(&path).expect("unchanged journal"), mutated);
}
