use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};

use aether_domain::TimestampMs;
use aether_ports::{DurableOutbox, OutboxId, OutboxMessage, PortErrorKind};
use aether_store_local::FileOutbox;

fn message(sequence: u64) -> OutboxMessage {
    OutboxMessage::new(
        "telemetry/site-a",
        format!("payload-{sequence}").into_bytes(),
        TimestampMs::new(sequence),
    )
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
