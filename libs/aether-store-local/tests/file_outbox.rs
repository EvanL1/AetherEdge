use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};

use aether_domain::TimestampMs;
use aether_ports::{DurableOutbox, OutboxId, OutboxMessage, PortErrorKind};
use aether_store_local::{FileOutbox, KeyedEnqueueOutcome, outbox_message_digest};

const TEST_FILE_MAGIC: &[u8; 8] = b"AETHOBX\0";

fn message(sequence: u64) -> OutboxMessage {
    OutboxMessage::new(
        "telemetry/site-a",
        format!("payload-{sequence}").into_bytes(),
        TimestampMs::new(sequence),
    )
}

fn accounted_message_bytes(message: &OutboxMessage) -> u64 {
    // 12-byte journal record header + 29-byte enqueue metadata + message data.
    41 + message.destination().len() as u64 + message.payload().len() as u64
}

fn accounted_receipt_bytes(key: &str) -> u64 {
    // 12-byte journal record header + operation/id/digest/key length metadata.
    57 + key.len() as u64
}

fn write_manual_header(path: &std::path::Path, magic: &[u8; 8], reserved: u32) {
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .expect("create manual outbox journal");
    let mut header = [0_u8; 16];
    header[..8].copy_from_slice(magic);
    header[8..12].copy_from_slice(&reserved.to_le_bytes());
    file.write_all(&header).expect("write manual file header");
    file.sync_all().expect("sync manual outbox journal");
}

#[test]
fn a_nonzero_reserved_field_is_rejected_without_rewrite() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("reserved.outbox");
    write_manual_header(&path, TEST_FILE_MAGIC, 1);
    let before = std::fs::read(&path).expect("read invalid journal");

    let error = FileOutbox::open(&path, 8).expect_err("reserved field must be rejected");
    assert_eq!(error.kind(), PortErrorKind::InvalidData);
    assert!(error.to_string().contains("reserved field must be zero"));
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn a_numbered_old_magic_is_rejected_without_rewrite() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("old-magic.outbox");
    write_manual_header(&path, b"AETHOBX1", 0);
    let before = std::fs::read(&path).expect("read invalid journal");

    let error = FileOutbox::open(&path, 8).expect_err("old magic must be rejected");
    assert_eq!(error.kind(), PortErrorKind::InvalidData);
    assert!(error.to_string().contains("journal magic does not match"));
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[tokio::test]
async fn keyed_enqueue_is_atomic_idempotent_and_reclaims_receipt_bytes() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("keyed.outbox");
    let original = message(1);
    let key = "alarm:event-1";
    let limit = accounted_message_bytes(&original) + accounted_receipt_bytes(key);
    let outbox = FileOutbox::open_with_limits(&path, 8, limit).expect("open keyed outbox");

    let first = outbox
        .enqueue_keyed(key.to_string(), original.clone())
        .await
        .expect("first keyed enqueue");
    let KeyedEnqueueOutcome::Enqueued(id) = first else {
        panic!("first admission must enqueue");
    };
    let retry = OutboxMessage::new(
        original.destination(),
        original.payload(),
        TimestampMs::new(999),
    );
    assert_eq!(
        outbox
            .enqueue_keyed(key.to_string(), retry)
            .await
            .expect("idempotent keyed retry"),
        KeyedEnqueueOutcome::Existing(id)
    );
    assert_eq!(outbox.peek(8).await.expect("peek").len(), 1);
    let stats = outbox.stats().await.expect("keyed stats");
    assert_eq!(stats.current_live_bytes, limit);
    assert_eq!(stats.keyed_receipts, 1);
    assert_eq!(stats.keyed_receipt_bytes, accounted_receipt_bytes(key));

    let conflicting = OutboxMessage::new(
        original.destination(),
        b"different".to_vec(),
        TimestampMs::new(1),
    );
    assert_eq!(
        outbox
            .enqueue_keyed(key.to_string(), conflicting)
            .await
            .expect_err("a key cannot be rebound")
            .kind(),
        PortErrorKind::Conflict
    );

    assert!(
        outbox
            .release_keyed_receipt(key.to_string(), id, outbox_message_digest(&original))
            .await
            .expect("release receipt")
    );
    let stats = outbox.stats().await.expect("released stats");
    assert_eq!(stats.current_live_bytes, accounted_message_bytes(&original));
    assert_eq!(stats.keyed_receipts, 0);
    assert_eq!(stats.keyed_receipt_bytes, 0);
}

#[tokio::test]
async fn keyed_receipt_survives_restart_and_ack_until_explicit_release() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("keyed-restart.outbox");
    let original = message(2);
    let digest = outbox_message_digest(&original);
    let key = "alarm:event-2";
    let limit = accounted_message_bytes(&original) + accounted_receipt_bytes(key);
    let outbox = FileOutbox::open_with_limits(&path, 8, limit).expect("open keyed outbox");
    let id = outbox
        .enqueue_keyed(key.to_string(), original)
        .await
        .expect("keyed enqueue")
        .id();
    outbox.acknowledge(&[id]).await.expect("ack entry");
    outbox.compact().await.expect("compact receipt-only state");
    drop(outbox);

    let reopened = FileOutbox::open_with_limits(&path, 8, limit).expect("reopen keyed outbox");
    assert!(reopened.peek(8).await.expect("peek").is_empty());
    assert_eq!(
        reopened.keyed_receipts().await.expect("recovered receipts"),
        vec![aether_store_local::KeyedReceipt {
            key: key.to_string(),
            outbox_id: id,
            message_digest: digest,
        }]
    );
    assert!(
        reopened
            .release_keyed_receipt(key.to_string(), id, digest)
            .await
            .expect("release recovered receipt")
    );
    assert_eq!(
        reopened
            .stats()
            .await
            .expect("receipt released stats")
            .current_live_bytes,
        0
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_keyed_retries_create_one_entry() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("keyed-concurrent.outbox");
    let outbox = FileOutbox::open(&path, 16).expect("open keyed outbox");
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..16 {
        let producer = outbox.clone();
        tasks.spawn(async move {
            producer
                .enqueue_keyed("alarm:shared-event".to_string(), message(1))
                .await
                .expect("concurrent keyed retry")
        });
    }
    let mut ids = Vec::new();
    while let Some(result) = tasks.join_next().await {
        ids.push(result.expect("keyed producer").id());
    }
    assert!(ids.iter().all(|id| *id == ids[0]));
    assert_eq!(outbox.peek(16).await.expect("peek keyed entries").len(), 1);
    assert_eq!(outbox.stats().await.expect("keyed stats").keyed_receipts, 1);
}

#[tokio::test]
async fn pending_entries_and_next_id_survive_reopen() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("uplink.outbox");

    let outbox = FileOutbox::open(&path, 8).expect("open file outbox");
    let first = outbox.enqueue(message(1)).await.expect("enqueue first");
    let second = outbox.enqueue(message(2)).await.expect("enqueue second");
    drop(outbox);

    let reopened = FileOutbox::open(&path, 8).expect("reopen file outbox");
    let pending = reopened.peek(8).await.expect("peek recovered entries");
    assert_eq!(
        pending.iter().map(|entry| entry.id()).collect::<Vec<_>>(),
        vec![first, second]
    );
    assert_eq!(pending[0].message().destination(), "telemetry/site-a");
    assert_eq!(pending[1].message().payload(), b"payload-2");

    assert_eq!(
        reopened
            .acknowledge(&[first])
            .await
            .expect("acknowledge first"),
        1
    );
    drop(reopened);

    let reopened = FileOutbox::open(&path, 8).expect("reopen after ack");
    let third = reopened.enqueue(message(3)).await.expect("enqueue third");
    assert!(third > second, "durable IDs must never be reused");
    assert_eq!(
        reopened
            .peek(8)
            .await
            .expect("peek after second reopen")
            .iter()
            .map(|entry| entry.id())
            .collect::<Vec<_>>(),
        vec![second, third]
    );
}

#[tokio::test]
async fn incomplete_tail_is_truncated_without_losing_committed_entries() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("torn-tail.outbox");

    let outbox = FileOutbox::open(&path, 8).expect("open file outbox");
    let first = outbox.enqueue(message(1)).await.expect("enqueue first");
    drop(outbox);

    let committed_len = std::fs::metadata(&path).expect("journal metadata").len();
    let mut file = OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("open journal for crash simulation");
    // Simulate a crash after only the record magic and part of its length
    // reached disk. This is a valid prefix of the on-disk record header.
    let mut partial_header = 0x5842_4F41_u32.to_le_bytes().to_vec();
    partial_header.extend_from_slice(&[0x20, 0x00]);
    file.write_all(&partial_header)
        .expect("append incomplete record");
    file.sync_all().expect("sync incomplete record");
    drop(file);

    let recovered = FileOutbox::open(&path, 8).expect("recover torn journal tail");
    let pending = recovered.peek(8).await.expect("peek recovered entry");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id(), first);
    assert_eq!(
        std::fs::metadata(&path)
            .expect("recovered journal metadata")
            .len(),
        committed_len
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_producers_are_serialized_with_unique_fifo_ids() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("concurrent.outbox");
    let outbox = FileOutbox::open(&path, 32).expect("open file outbox");

    let mut tasks = tokio::task::JoinSet::new();
    for sequence in 0..16 {
        let producer = outbox.clone();
        tasks.spawn(async move {
            producer
                .enqueue(message(sequence))
                .await
                .expect("concurrent enqueue")
        });
    }

    let mut ids = Vec::new();
    while let Some(result) = tasks.join_next().await {
        ids.push(result.expect("producer task"));
    }
    ids.sort_unstable();
    assert_eq!(ids, (1..=16).map(OutboxId::new).collect::<Vec<OutboxId>>());

    let visible = outbox.peek(32).await.expect("peek concurrent entries");
    assert_eq!(
        visible.iter().map(|entry| entry.id()).collect::<Vec<_>>(),
        ids
    );
}

#[test]
fn a_second_writer_for_the_same_journal_is_rejected() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("exclusive.outbox");
    let first = FileOutbox::open(&path, 8).expect("open first writer");

    let Err(error) = FileOutbox::open(&path, 8) else {
        panic!("second writer must be rejected");
    };
    assert_eq!(error.kind(), PortErrorKind::Conflict);

    drop(first);
    FileOutbox::open(&path, 8).expect("lock must be released when outbox drops");
}

#[tokio::test]
async fn compaction_preserves_live_entries_and_monotonic_ids() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("compact.outbox");
    let outbox = FileOutbox::open(&path, 16).expect("open file outbox");

    let mut ids = Vec::new();
    for sequence in 0..10 {
        ids.push(
            outbox
                .enqueue(message(sequence))
                .await
                .expect("enqueue before compaction"),
        );
    }
    outbox
        .acknowledge(&ids[..8])
        .await
        .expect("acknowledge before compaction");
    let before = std::fs::metadata(&path).expect("journal metadata").len();

    outbox.compact().await.expect("compact journal");
    let after = std::fs::metadata(&path)
        .expect("compacted journal metadata")
        .len();
    assert!(
        after < before,
        "compaction should reclaim acknowledged data"
    );
    drop(outbox);

    let reopened = FileOutbox::open(&path, 16).expect("reopen compacted journal");
    assert_eq!(
        reopened
            .peek(16)
            .await
            .expect("peek compacted journal")
            .iter()
            .map(|entry| entry.id())
            .collect::<Vec<_>>(),
        ids[8..]
    );
    assert!(
        reopened
            .enqueue(message(99))
            .await
            .expect("enqueue after compaction")
            > ids[9]
    );
}

#[tokio::test]
async fn duplicate_acknowledgement_ids_count_each_entry_only_once() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("duplicate-ack.outbox");
    let outbox = FileOutbox::open(&path, 4).expect("open file outbox");
    let id = outbox.enqueue(message(1)).await.expect("enqueue message");

    assert_eq!(
        outbox
            .acknowledge(&[id, id])
            .await
            .expect("acknowledge duplicate IDs"),
        1
    );
    assert!(outbox.peek(4).await.expect("peek outbox").is_empty());
}

#[test]
fn corruption_before_a_later_committed_record_is_not_silently_truncated() {
    let runtime = tokio::runtime::Runtime::new().expect("Tokio runtime");
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("corrupt-middle.outbox");
    let outbox = FileOutbox::open(&path, 4).expect("open file outbox");
    runtime.block_on(async {
        outbox.enqueue(message(1)).await.expect("enqueue first");
        outbox.enqueue(message(2)).await.expect("enqueue second");
    });
    drop(outbox);

    // File header is 16 bytes and record header is 12 bytes. Flip the first
    // record's operation byte while leaving its checksum unchanged. Because a
    // second committed record follows, recovery must flag corruption.
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .expect("open journal for corruption simulation");
    file.seek(SeekFrom::Start(28))
        .expect("seek first record payload");
    let mut operation = [0_u8; 1];
    file.read_exact(&mut operation)
        .expect("read operation byte");
    operation[0] ^= 0xFF;
    file.seek(SeekFrom::Start(28))
        .expect("seek first record payload again");
    file.write_all(&operation).expect("corrupt operation byte");
    file.sync_all().expect("sync corruption simulation");
    drop(file);

    let Err(error) = FileOutbox::open(&path, 4) else {
        panic!("middle corruption must fail recovery");
    };
    assert_eq!(error.kind(), PortErrorKind::InvalidData);
}

#[tokio::test]
async fn live_byte_limit_is_exact_and_acknowledgement_reclaims_capacity() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("byte-limit.outbox");
    let first_message = message(1);
    let exact_limit = accounted_message_bytes(&first_message);
    let outbox = FileOutbox::open_with_limits(&path, 8, exact_limit).expect("open bounded outbox");

    let first = outbox
        .enqueue(first_message)
        .await
        .expect("exact-boundary message fits");
    let full = outbox
        .enqueue(message(2))
        .await
        .expect_err("one byte-quota-sized message fills the outbox");
    assert_eq!(full.kind(), PortErrorKind::Unavailable);

    let stats = outbox.stats().await.expect("quota stats");
    assert_eq!(stats.current_live_bytes, exact_limit);
    assert_eq!(stats.max_live_bytes, exact_limit);
    assert_eq!(stats.byte_quota_rejections, 1);
    assert_eq!(stats.quota_rejections, 1);

    assert_eq!(outbox.acknowledge(&[first]).await.expect("ack first"), 1);
    assert_eq!(
        outbox
            .stats()
            .await
            .expect("reclaimed stats")
            .current_live_bytes,
        0
    );
    outbox
        .enqueue(message(3))
        .await
        .expect("acknowledgement releases byte quota");
}

#[test]
fn zero_overflowing_and_single_record_too_large_limits_are_rejected() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("invalid-limits.outbox");

    let zero = FileOutbox::open_with_limits(&path, 8, 0).expect_err("zero limit is invalid");
    assert_eq!(zero.kind(), PortErrorKind::InvalidData);
    let overflow = FileOutbox::open_with_limits(&path, 8, u64::MAX)
        .expect_err("physical high-water computation must not overflow");
    assert_eq!(overflow.kind(), PortErrorKind::InvalidData);

    let runtime = tokio::runtime::Runtime::new().expect("Tokio runtime");
    let candidate = message(1);
    let too_small = accounted_message_bytes(&candidate) - 1;
    let outbox = FileOutbox::open_with_limits(&path, 8, too_small).expect("valid small limit");
    let error = runtime
        .block_on(outbox.enqueue(candidate))
        .expect_err("a single oversized live record is rejected");
    assert_eq!(error.kind(), PortErrorKind::Unavailable);
}

#[tokio::test]
async fn restart_rebuilds_live_bytes_and_rejects_a_smaller_limit() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("restart-quota.outbox");
    let per_message = accounted_message_bytes(&message(1));
    let limit = per_message * 2;

    let outbox = FileOutbox::open_with_limits(&path, 8, limit).expect("open bounded outbox");
    outbox.enqueue(message(1)).await.expect("enqueue first");
    outbox.enqueue(message(2)).await.expect("enqueue second");
    drop(outbox);

    let reopened = FileOutbox::open_with_limits(&path, 8, limit).expect("reopen at same limit");
    assert_eq!(
        reopened
            .stats()
            .await
            .expect("recovered stats")
            .current_live_bytes,
        limit
    );
    drop(reopened);

    let too_small = FileOutbox::open_with_limits(&path, 8, limit - 1)
        .expect_err("startup fails closed when retained state exceeds the new limit");
    assert_eq!(too_small.kind(), PortErrorKind::InvalidData);
}

#[tokio::test]
async fn recovery_applies_final_limits_after_a_historical_peak() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("historical-peak.outbox");
    let per_message = accounted_message_bytes(&message(1));
    let outbox = FileOutbox::open_with_limits(&path, 8, per_message * 8).expect("open outbox");
    let mut ids = Vec::new();
    for sequence in 0..8 {
        ids.push(
            outbox
                .enqueue(message(sequence))
                .await
                .expect("enqueue historical peak"),
        );
    }
    outbox
        .acknowledge(&ids[..7])
        .await
        .expect("reduce final survivor set");
    drop(outbox);

    // Recovery must index the historical peak without retaining all historical
    // payloads, then apply the smaller limits to the one final survivor.
    let reopened = FileOutbox::open_with_limits(&path, 1, per_message)
        .expect("final state fits reduced limits");
    let pending = reopened.peek(1).await.expect("peek reduced recovery");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id(), ids[7]);
    assert_eq!(
        reopened
            .stats()
            .await
            .expect("recovered reduced statistics")
            .current_live_bytes,
        per_message
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_pushes_cannot_race_past_the_byte_limit() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("concurrent-quota.outbox");
    let per_message = accounted_message_bytes(&message(1));
    let outbox =
        FileOutbox::open_with_limits(&path, 32, per_message * 4).expect("open bounded outbox");

    let mut tasks = tokio::task::JoinSet::new();
    for sequence in 0..16 {
        let producer = outbox.clone();
        tasks.spawn(async move { producer.enqueue(message(sequence)).await });
    }
    let mut accepted = 0;
    let mut rejected = 0;
    while let Some(result) = tasks.join_next().await {
        match result.expect("producer task") {
            Ok(_) => accepted += 1,
            Err(error) => {
                assert_eq!(error.kind(), PortErrorKind::Unavailable);
                rejected += 1;
            },
        }
    }

    assert_eq!(accepted, 4);
    assert_eq!(rejected, 12);
    let stats = outbox.stats().await.expect("concurrent quota stats");
    assert_eq!(stats.current_live_bytes, per_message * 4);
    assert_eq!(stats.byte_quota_rejections, 12);
}

#[tokio::test]
async fn external_length_change_poisoned_the_live_owner_fail_closed() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("external-change.outbox");
    let outbox = FileOutbox::open_with_limits(&path, 8, 1024).expect("open outbox");
    outbox.enqueue(message(1)).await.expect("enqueue first");

    let mut outsider = OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("simulate an uncooperative external writer");
    outsider
        .write_all(b"unexpected")
        .expect("append external data");
    outsider.sync_all().expect("sync external data");
    drop(outsider);

    let detected = outbox
        .enqueue(message(2))
        .await
        .expect_err("external file growth must be detected before append");
    assert_eq!(detected.kind(), PortErrorKind::Permanent);
    let poisoned = outbox
        .peek(8)
        .await
        .expect_err("detected external mutation permanently poisons the owner");
    assert_eq!(poisoned.kind(), PortErrorKind::Permanent);
}

#[cfg(unix)]
#[tokio::test]
async fn same_length_inode_replacement_poisoned_the_live_owner_fail_closed() {
    use std::os::unix::fs::MetadataExt;

    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("external-replacement.outbox");
    let replacement = dir.path().join("external-replacement.staged");
    let outbox = FileOutbox::open_with_limits(&path, 8, 1024).expect("open outbox");
    outbox.enqueue(message(1)).await.expect("enqueue first");

    std::fs::copy(&path, &replacement).expect("stage same-length replacement");
    let original_metadata = std::fs::metadata(&path).expect("original metadata");
    let replacement_metadata = std::fs::metadata(&replacement).expect("replacement metadata");
    assert_eq!(original_metadata.len(), replacement_metadata.len());
    assert_ne!(
        (original_metadata.dev(), original_metadata.ino()),
        (replacement_metadata.dev(), replacement_metadata.ino())
    );
    std::fs::rename(&replacement, &path).expect("replace canonical journal path");

    let detected = outbox
        .stats()
        .await
        .expect_err("same-length inode replacement must be detected");
    assert_eq!(detected.kind(), PortErrorKind::Permanent);
    let poisoned = outbox
        .enqueue(message(2))
        .await
        .expect_err("detected path replacement permanently poisons the owner");
    assert_eq!(poisoned.kind(), PortErrorKind::Permanent);
}

#[tokio::test]
async fn high_churn_compacts_before_the_physical_journal_high_watermark() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("high-churn.outbox");
    let per_message = accounted_message_bytes(&message(1));
    let outbox =
        FileOutbox::open_with_limits(&path, 8, per_message * 2).expect("open bounded outbox");

    for sequence in 0..500 {
        let id = outbox
            .enqueue(message(sequence))
            .await
            .expect("enqueue churn message");
        outbox
            .acknowledge(&[id])
            .await
            .expect("acknowledge churn message");
        let stats = outbox.stats().await.expect("churn stats");
        assert!(stats.journal_bytes <= stats.max_journal_bytes);
    }

    let stats = outbox.stats().await.expect("final churn stats");
    assert_eq!(stats.current_live_bytes, 0);
    assert!(std::fs::metadata(&path).expect("journal metadata").len() <= stats.max_journal_bytes);
    drop(outbox);

    let reopened = FileOutbox::open_with_limits(&path, 8, per_message * 2)
        .expect("reopen bounded high-churn journal");
    assert_eq!(
        reopened
            .stats()
            .await
            .expect("reopened stats")
            .current_live_bytes,
        0
    );
}
