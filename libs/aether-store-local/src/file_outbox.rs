//! Crash-recoverable local outbox backed by an append-only journal.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use aether_domain::TimestampMs;
use aether_ports::{
    DurableOutbox, OutboxEntry, OutboxId, OutboxMessage, PortError, PortErrorKind, PortResult,
};
use async_trait::async_trait;
use fs2::FileExt;
use sha2::{Digest as _, Sha256};
use tokio::sync::oneshot;

const FILE_MAGIC: &[u8; 8] = b"AETHOBX\0";
const FILE_HEADER_LEN: usize = 16;
const RECORD_MAGIC: u32 = 0x5842_4F41;
const RECORD_HEADER_LEN: usize = 12;
const MAX_RECORD_LEN: usize = 16 * 1024 * 1024;
const MAX_CAPACITY: usize = (MAX_RECORD_LEN - 5) / std::mem::size_of::<u64>();
const REQUEST_QUEUE_CAPACITY: usize = 256;
const JOURNAL_HIGH_WATER_MULTIPLIER: u64 = 2;
const JOURNAL_FIXED_OVERHEAD: u64 = (FILE_HEADER_LEN + RECORD_HEADER_LEN + 9) as u64;

/// Default live-byte ceiling used by [`FileOutbox::open`]. Deployments may pass
/// their configured limit explicitly through [`FileOutbox::open_with_limits`].
pub const DEFAULT_OUTBOX_MAX_LIVE_BYTES: u64 = 256 * 1024 * 1024;

const OP_ENQUEUE: u8 = 1;
const OP_ACKNOWLEDGE: u8 = 2;
const OP_CHECKPOINT: u8 = 3;
const OP_KEYED_ENQUEUE: u8 = 4;
const OP_RELEASE_KEY: u8 = 5;
const OP_KEYED_RECEIPT: u8 = 6;
const MAX_IDEMPOTENCY_KEY_BYTES: usize = 256;

/// A bounded, crash-recoverable outbox that requires no external service.
///
/// Operations are serialized on a dedicated worker thread. An enqueue or
/// acknowledgement is reported as successful only after its journal record
/// has been synchronized to disk. The journal path is exclusively locked for
/// the lifetime of this value, including across cloned handles.
#[derive(Clone)]
pub struct FileOutbox {
    worker: Arc<Worker>,
}

impl std::fmt::Debug for FileOutbox {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FileOutbox")
            .field("path", &self.worker.path)
            .finish_non_exhaustive()
    }
}

impl FileOutbox {
    /// Opens or creates a journal that stores at most `capacity` live entries.
    ///
    /// Opening performs synchronous recovery and should normally happen during
    /// gateway startup, before latency-sensitive tasks are launched.
    pub fn open(path: impl AsRef<Path>, capacity: usize) -> PortResult<Self> {
        Self::open_with_limits(path, capacity, DEFAULT_OUTBOX_MAX_LIVE_BYTES)
    }

    /// Opens or creates a journal with entry and live-byte limits.
    ///
    /// `max_live_bytes` accounts for the complete on-disk ENQUEUE record for
    /// every pending entry: record framing, message metadata, destination, and
    /// payload. Acknowledgement releases the corresponding live bytes even
    /// though the append-only journal is not physically compacted yet.
    pub fn open_with_limits(
        path: impl AsRef<Path>,
        capacity: usize,
        max_live_bytes: u64,
    ) -> PortResult<Self> {
        if capacity == 0 {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "file outbox capacity must be greater than zero",
            ));
        }
        if capacity > MAX_CAPACITY {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                format!("file outbox capacity exceeds maximum {MAX_CAPACITY}"),
            ));
        }
        if max_live_bytes == 0 {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "file outbox live-byte limit must be greater than zero",
            ));
        }

        let path = path.as_ref().to_path_buf();
        let journal = Journal::open(path.clone(), capacity, max_live_bytes)?;
        let (sender, receiver) = sync_channel(REQUEST_QUEUE_CAPACITY);
        let handle = thread::Builder::new()
            .name("aether-file-outbox".to_string())
            .spawn(move || run_worker(journal, receiver))
            .map_err(|error| io_error("spawn file outbox worker", error))?;

        Ok(Self {
            worker: Arc::new(Worker {
                path,
                sender,
                join: Mutex::new(Some(handle)),
            }),
        })
    }

    /// Atomically rewrites the journal with only live entries.
    ///
    /// The checkpoint also persists the next identifier, so compaction never
    /// permits an acknowledged identifier to be reused after restart.
    pub async fn compact(&self) -> PortResult<()> {
        let (reply, response) = oneshot::channel();
        self.submit(Request::Compact { reply })?;
        await_response(response).await
    }

    /// Returns an O(1) snapshot of pending entries and live-byte quota usage.
    pub async fn stats(&self) -> PortResult<FileOutboxStats> {
        let (reply, response) = oneshot::channel();
        self.submit(Request::Stats { reply })?;
        await_response(response).await
    }

    /// Atomically admits a message and its durable idempotency receipt in one
    /// synchronized journal mutation.
    pub async fn enqueue_keyed(
        &self,
        key: String,
        message: OutboxMessage,
    ) -> PortResult<KeyedEnqueueOutcome> {
        let (reply, response) = oneshot::channel();
        self.submit(Request::EnqueueKeyed {
            key,
            message,
            reply,
        })?;
        await_response(response).await
    }

    /// Releases a temporary idempotency receipt after a durable external
    /// ledger has committed the returned outbox identifier.
    pub async fn release_keyed_receipt(
        &self,
        key: String,
        expected_id: OutboxId,
        expected_digest: [u8; 32],
    ) -> PortResult<bool> {
        let (reply, response) = oneshot::channel();
        self.submit(Request::ReleaseKeyedReceipt {
            key,
            expected_id,
            expected_digest,
            reply,
        })?;
        await_response(response).await
    }

    /// Lists the temporary receipts needed to reconcile an interrupted
    /// cross-store admission at startup.
    pub async fn keyed_receipts(&self) -> PortResult<Vec<KeyedReceipt>> {
        let (reply, response) = oneshot::channel();
        self.submit(Request::KeyedReceipts { reply })?;
        await_response(response).await
    }

    fn submit(&self, request: Request) -> PortResult<()> {
        self.worker
            .sender
            .try_send(request)
            .map_err(|error| match error {
                TrySendError::Full(_) => PortError::new(
                    PortErrorKind::Unavailable,
                    "file outbox worker queue is full",
                ),
                TrySendError::Disconnected(_) => PortError::new(
                    PortErrorKind::Permanent,
                    "file outbox worker stopped unexpectedly",
                ),
            })
    }
}

/// Current durable outbox quota usage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileOutboxStats {
    pub pending_entries: usize,
    pub max_entries: usize,
    pub current_live_bytes: u64,
    pub max_live_bytes: u64,
    pub journal_bytes: u64,
    pub max_journal_bytes: u64,
    pub quota_rejections: u64,
    pub entry_quota_rejections: u64,
    pub byte_quota_rejections: u64,
    pub physical_quota_rejections: u64,
    pub keyed_receipts: usize,
    pub keyed_receipt_bytes: u64,
}

/// Result of one keyed durable admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyedEnqueueOutcome {
    Enqueued(OutboxId),
    Existing(OutboxId),
}

impl KeyedEnqueueOutcome {
    #[must_use]
    pub const fn id(self) -> OutboxId {
        match self {
            Self::Enqueued(id) | Self::Existing(id) => id,
        }
    }

    #[must_use]
    pub const fn is_new(self) -> bool {
        matches!(self, Self::Enqueued(_))
    }
}

/// Temporary durable receipt for a cross-store keyed admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyedReceipt {
    pub key: String,
    pub outbox_id: OutboxId,
    pub message_digest: [u8; 32],
}

/// Stable digest bound to a keyed outbox admission.
#[must_use]
pub fn outbox_message_digest(message: &OutboxMessage) -> [u8; 32] {
    outbox_message_digest_parts(message.destination(), message.payload())
}

fn outbox_message_digest_parts(destination: &str, payload: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update((destination.len() as u64).to_le_bytes());
    digest.update(destination.as_bytes());
    digest.update((payload.len() as u64).to_le_bytes());
    digest.update(payload);
    digest.finalize().into()
}

#[async_trait]
impl DurableOutbox for FileOutbox {
    async fn enqueue(&self, message: OutboxMessage) -> PortResult<OutboxId> {
        let (reply, response) = oneshot::channel();
        self.submit(Request::Enqueue { message, reply })?;
        await_response(response).await
    }

    async fn peek(&self, limit: usize) -> PortResult<Vec<OutboxEntry>> {
        let (reply, response) = oneshot::channel();
        self.submit(Request::Peek { limit, reply })?;
        await_response(response).await
    }

    async fn acknowledge(&self, ids: &[OutboxId]) -> PortResult<usize> {
        let (reply, response) = oneshot::channel();
        self.submit(Request::Acknowledge {
            ids: ids.to_vec(),
            reply,
        })?;
        await_response(response).await
    }
}

async fn await_response<T>(response: oneshot::Receiver<PortResult<T>>) -> PortResult<T> {
    response.await.map_err(|_| {
        PortError::new(
            PortErrorKind::Permanent,
            "file outbox worker dropped a response",
        )
    })?
}

struct Worker {
    path: PathBuf,
    sender: SyncSender<Request>,
    join: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        // A blocking send is intentional during final ownership release: it
        // drains already-accepted requests before the worker releases the file
        // lock. No async lock is held here.
        let _ = self.sender.send(Request::Shutdown);
        if let Ok(mut slot) = self.join.lock()
            && let Some(handle) = slot.take()
        {
            let _ = handle.join();
        }
    }
}

enum Request {
    Enqueue {
        message: OutboxMessage,
        reply: oneshot::Sender<PortResult<OutboxId>>,
    },
    EnqueueKeyed {
        key: String,
        message: OutboxMessage,
        reply: oneshot::Sender<PortResult<KeyedEnqueueOutcome>>,
    },
    Peek {
        limit: usize,
        reply: oneshot::Sender<PortResult<Vec<OutboxEntry>>>,
    },
    Acknowledge {
        ids: Vec<OutboxId>,
        reply: oneshot::Sender<PortResult<usize>>,
    },
    Compact {
        reply: oneshot::Sender<PortResult<()>>,
    },
    Stats {
        reply: oneshot::Sender<PortResult<FileOutboxStats>>,
    },
    ReleaseKeyedReceipt {
        key: String,
        expected_id: OutboxId,
        expected_digest: [u8; 32],
        reply: oneshot::Sender<PortResult<bool>>,
    },
    KeyedReceipts {
        reply: oneshot::Sender<PortResult<Vec<KeyedReceipt>>>,
    },
    Shutdown,
}

fn run_worker(mut journal: Journal, receiver: Receiver<Request>) {
    while let Ok(request) = receiver.recv() {
        match request {
            Request::Enqueue { message, reply } => {
                let _ = reply.send(journal.enqueue(message));
            },
            Request::EnqueueKeyed {
                key,
                message,
                reply,
            } => {
                let _ = reply.send(journal.enqueue_keyed(key, message));
            },
            Request::Peek { limit, reply } => {
                let _ = reply.send(journal.peek(limit));
            },
            Request::Acknowledge { ids, reply } => {
                let _ = reply.send(journal.acknowledge(&ids));
            },
            Request::Compact { reply } => {
                let _ = reply.send(journal.compact());
            },
            Request::Stats { reply } => {
                let _ = reply.send(journal.stats());
            },
            Request::ReleaseKeyedReceipt {
                key,
                expected_id,
                expected_digest,
                reply,
            } => {
                let _ =
                    reply.send(journal.release_keyed_receipt(&key, expected_id, expected_digest));
            },
            Request::KeyedReceipts { reply } => {
                let _ = reply.send(journal.keyed_receipts());
            },
            Request::Shutdown => break,
        }
    }
}

struct Journal {
    path: PathBuf,
    capacity: usize,
    max_live_bytes: u64,
    max_journal_bytes: u64,
    current_live_bytes: u64,
    journal_bytes: u64,
    entry_quota_rejections: u64,
    byte_quota_rejections: u64,
    physical_quota_rejections: u64,
    file: File,
    file_identity: JournalFileIdentity,
    _lock_file: File,
    next_id: u64,
    entries: BTreeMap<OutboxId, OutboxEntry>,
    receipts: BTreeMap<String, ReceiptState>,
    receipt_bytes: u64,
    poisoned: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReceiptState {
    outbox_id: OutboxId,
    message_digest: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct JournalFileIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl JournalFileIdentity {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        }
    }
}

impl Journal {
    fn open(path: PathBuf, capacity: usize, max_live_bytes: u64) -> PortResult<Self> {
        Self::open_with_parent_sync(path, capacity, max_live_bytes, sync_parent_directory)
    }

    fn open_with_parent_sync<SyncParent>(
        path: PathBuf,
        capacity: usize,
        max_live_bytes: u64,
        sync_parent: SyncParent,
    ) -> PortResult<Self>
    where
        SyncParent: FnOnce(&Path) -> PortResult<()>,
    {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .map_err(|error| io_error("create outbox directory", error))?;
        }

        let lock_path = sibling_path(&path, ".lock");
        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|error| io_error("open outbox lock file", error))?;
        FileExt::try_lock_exclusive(&lock_file).map_err(|error| {
            if error.kind() == std::io::ErrorKind::WouldBlock {
                PortError::new(
                    PortErrorKind::Conflict,
                    format!("outbox journal is already open: {}", path.display()),
                )
            } else {
                io_error("lock outbox journal", error)
            }
        })?;

        let stale_compaction = sibling_path(&path, ".compact.tmp");
        match std::fs::remove_file(&stale_compaction) {
            Ok(()) => {},
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(io_error("remove stale outbox compaction", error)),
        }

        let (mut file, created) = open_or_create_journal_file(&path)?;
        let recovered = recover(&mut file, capacity, max_live_bytes)?;
        // A newly created journal is not crash-durable until its directory entry
        // is synchronized. Re-syncing the parent on every open also makes a
        // retry after an ambiguous directory-sync failure safe. This guarantees
        // the journal entry inside an existing parent directory; callers that
        // allow `create_dir_all` to build a new ancestor chain must persist that
        // installation layout separately.
        sync_parent(&path).map_err(|error| {
            let state = if created { "newly created" } else { "existing" };
            PortError::new(
                error.kind(),
                format!("sync parent directory for {state} outbox journal: {error}"),
            )
        })?;
        let journal_bytes = file
            .seek(SeekFrom::End(0))
            .map_err(|error| io_error("seek recovered outbox journal", error))?;
        let file_identity = JournalFileIdentity::from_metadata(
            &file
                .metadata()
                .map_err(|error| io_error("stat recovered outbox journal", error))?,
        );
        let max_journal_bytes = max_live_bytes
            .checked_mul(JOURNAL_HIGH_WATER_MULTIPLIER)
            .and_then(|bytes| bytes.checked_add(JOURNAL_FIXED_OVERHEAD))
            .ok_or_else(|| {
                PortError::new(
                    PortErrorKind::InvalidData,
                    "file outbox physical journal limit overflow",
                )
            })?;

        let mut journal = Self {
            path,
            capacity,
            max_live_bytes,
            max_journal_bytes,
            current_live_bytes: recovered.live_bytes,
            journal_bytes,
            entry_quota_rejections: 0,
            byte_quota_rejections: 0,
            physical_quota_rejections: 0,
            file,
            file_identity,
            _lock_file: lock_file,
            next_id: recovered.next_id,
            entries: recovered.entries,
            receipts: recovered.receipts,
            receipt_bytes: recovered.receipt_bytes,
            poisoned: None,
        };
        journal.verify_journal_length()?;
        if journal.journal_bytes > journal.max_journal_bytes {
            journal.compact()?;
        }
        Ok(journal)
    }

    fn enqueue(&mut self, message: OutboxMessage) -> PortResult<OutboxId> {
        self.ensure_healthy()?;
        self.verify_journal_length()?;
        if self.entries.len() >= self.capacity {
            self.entry_quota_rejections = self.entry_quota_rejections.saturating_add(1);
            return Err(PortError::new(
                PortErrorKind::Unavailable,
                format!("file outbox capacity {} reached", self.capacity),
            ));
        }

        let id = OutboxId::new(self.next_id);
        let next_id = self.next_id.checked_add(1).ok_or_else(|| {
            PortError::new(PortErrorKind::Permanent, "outbox identifier exhausted")
        })?;
        let entry = OutboxEntry::new(id, message, 0);
        let payload = encode_enqueue(&entry)?;
        let record_bytes = encoded_record_bytes(payload.len())?;
        let next_live_bytes = self
            .current_live_bytes
            .checked_add(record_bytes)
            .ok_or_else(|| {
                PortError::new(
                    PortErrorKind::InvalidData,
                    "file outbox live-byte accounting overflow",
                )
            })?;
        if next_live_bytes > self.max_live_bytes {
            self.byte_quota_rejections = self.byte_quota_rejections.saturating_add(1);
            return Err(PortError::new(
                PortErrorKind::Unavailable,
                format!(
                    "file outbox live-byte limit {} reached (current {}, message {})",
                    self.max_live_bytes, self.current_live_bytes, record_bytes
                ),
            ));
        }
        self.compact_before_append(record_bytes)?;
        append_record(&mut self.file, &payload)?;
        self.journal_bytes = self
            .journal_bytes
            .checked_add(record_bytes)
            .ok_or_else(|| corrupt("outbox physical journal accounting overflow"))?;

        self.entries.insert(id, entry);
        self.current_live_bytes = next_live_bytes;
        self.next_id = next_id;
        Ok(id)
    }

    fn enqueue_keyed(
        &mut self,
        key: String,
        message: OutboxMessage,
    ) -> PortResult<KeyedEnqueueOutcome> {
        self.ensure_healthy()?;
        self.verify_journal_length()?;
        validate_idempotency_key(&key)?;

        let message_digest = outbox_message_digest(&message);
        if let Some(receipt) = self.receipts.get(&key) {
            if receipt.message_digest != message_digest {
                return Err(PortError::new(
                    PortErrorKind::Conflict,
                    "idempotency key is already bound to a different outbox message",
                ));
            }
            return Ok(KeyedEnqueueOutcome::Existing(receipt.outbox_id));
        }
        if self.entries.len() >= self.capacity {
            self.entry_quota_rejections = self.entry_quota_rejections.saturating_add(1);
            return Err(PortError::new(
                PortErrorKind::Unavailable,
                format!("file outbox capacity {} reached", self.capacity),
            ));
        }

        let id = OutboxId::new(self.next_id);
        let next_id = self.next_id.checked_add(1).ok_or_else(|| {
            PortError::new(PortErrorKind::Permanent, "outbox identifier exhausted")
        })?;
        let entry = OutboxEntry::new(id, message, 0);
        let entry_bytes = encoded_enqueue_record_bytes(&entry)?;
        let receipt_bytes = encoded_keyed_receipt_record_bytes(&key)?;
        let admission_bytes = entry_bytes
            .checked_add(receipt_bytes)
            .ok_or_else(|| corrupt("keyed outbox live-byte accounting overflow"))?;
        let next_live_bytes = self
            .current_live_bytes
            .checked_add(admission_bytes)
            .ok_or_else(|| corrupt("file outbox live-byte accounting overflow"))?;
        if next_live_bytes > self.max_live_bytes {
            self.byte_quota_rejections = self.byte_quota_rejections.saturating_add(1);
            return Err(PortError::new(
                PortErrorKind::Unavailable,
                format!(
                    "file outbox live-byte limit {} reached (current {}, keyed admission {})",
                    self.max_live_bytes, self.current_live_bytes, admission_bytes
                ),
            ));
        }

        let payload = encode_keyed_enqueue(&key, &entry, message_digest)?;
        let physical_bytes = encoded_record_bytes(payload.len())?;
        self.compact_before_append(physical_bytes)?;
        append_record(&mut self.file, &payload)?;
        self.journal_bytes = self
            .journal_bytes
            .checked_add(physical_bytes)
            .ok_or_else(|| corrupt("outbox physical journal accounting overflow"))?;

        self.entries.insert(id, entry);
        self.receipts.insert(
            key,
            ReceiptState {
                outbox_id: id,
                message_digest,
            },
        );
        self.current_live_bytes = next_live_bytes;
        self.receipt_bytes = self
            .receipt_bytes
            .checked_add(receipt_bytes)
            .ok_or_else(|| corrupt("keyed receipt byte accounting overflow"))?;
        self.next_id = next_id;
        Ok(KeyedEnqueueOutcome::Enqueued(id))
    }

    fn release_keyed_receipt(
        &mut self,
        key: &str,
        expected_id: OutboxId,
        expected_digest: [u8; 32],
    ) -> PortResult<bool> {
        self.ensure_healthy()?;
        self.verify_journal_length()?;
        validate_idempotency_key(key)?;
        let Some(receipt) = self.receipts.get(key).copied() else {
            return Ok(false);
        };
        if receipt.outbox_id != expected_id || receipt.message_digest != expected_digest {
            return Err(PortError::new(
                PortErrorKind::Conflict,
                "idempotency receipt does not match the expected admission",
            ));
        }

        let payload = encode_release_key(key, receipt)?;
        let record_bytes = encoded_record_bytes(payload.len())?;
        self.compact_before_append(record_bytes)?;
        append_record(&mut self.file, &payload)?;
        self.journal_bytes = self
            .journal_bytes
            .checked_add(record_bytes)
            .ok_or_else(|| corrupt("outbox physical journal accounting overflow"))?;

        self.receipts.remove(key);
        let receipt_bytes = encoded_keyed_receipt_record_bytes(key)?;
        self.current_live_bytes = self
            .current_live_bytes
            .checked_sub(receipt_bytes)
            .ok_or_else(|| corrupt("keyed receipt live-byte accounting underflow"))?;
        self.receipt_bytes = self
            .receipt_bytes
            .checked_sub(receipt_bytes)
            .ok_or_else(|| corrupt("keyed receipt byte accounting underflow"))?;
        Ok(true)
    }

    fn keyed_receipts(&mut self) -> PortResult<Vec<KeyedReceipt>> {
        self.ensure_healthy()?;
        self.verify_journal_length()?;
        Ok(self
            .receipts
            .iter()
            .map(|(key, receipt)| KeyedReceipt {
                key: key.clone(),
                outbox_id: receipt.outbox_id,
                message_digest: receipt.message_digest,
            })
            .collect())
    }

    fn peek(&mut self, limit: usize) -> PortResult<Vec<OutboxEntry>> {
        self.ensure_healthy()?;
        self.verify_journal_length()?;
        Ok(self.entries.values().take(limit).cloned().collect())
    }

    fn acknowledge(&mut self, ids: &[OutboxId]) -> PortResult<usize> {
        self.ensure_healthy()?;
        self.verify_journal_length()?;
        let existing = ids
            .iter()
            .copied()
            .filter(|id| self.entries.contains_key(id))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        if existing.is_empty() {
            return Ok(0);
        }

        let payload = encode_acknowledge(&existing)?;
        let record_bytes = encoded_record_bytes(payload.len())?;
        self.compact_before_append(record_bytes)?;
        append_record(&mut self.file, &payload)?;
        self.journal_bytes = self
            .journal_bytes
            .checked_add(record_bytes)
            .ok_or_else(|| corrupt("outbox physical journal accounting overflow"))?;
        for id in &existing {
            let Some(entry) = self.entries.remove(id) else {
                self.poisoned = Some("acknowledgement live-byte accounting mismatch".to_owned());
                return Err(PortError::new(
                    PortErrorKind::Permanent,
                    "file outbox acknowledgement accounting mismatch",
                ));
            };
            let entry_bytes = encoded_enqueue_record_bytes(&entry)?;
            self.current_live_bytes = self
                .current_live_bytes
                .checked_sub(entry_bytes)
                .ok_or_else(|| {
                    self.poisoned = Some("live-byte accounting underflow".to_owned());
                    PortError::new(
                        PortErrorKind::Permanent,
                        "file outbox live-byte accounting underflow",
                    )
                })?;
        }
        Ok(existing.len())
    }

    fn compact(&mut self) -> PortResult<()> {
        self.compact_with_hooks(open_compacted_journal_file, sync_parent_directory)
    }

    fn compact_with_hooks<OpenReplacement, SyncParent>(
        &mut self,
        open_replacement: OpenReplacement,
        sync_parent: SyncParent,
    ) -> PortResult<()>
    where
        OpenReplacement: FnOnce(&Path) -> PortResult<File>,
        SyncParent: FnOnce(&Path) -> PortResult<()>,
    {
        self.ensure_healthy()?;
        self.verify_journal_length()?;
        let temp_path = sibling_path(&self.path, ".compact.tmp");
        let mut temp = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&temp_path)
            .map_err(|error| io_error("create outbox compaction file", error))?;
        write_file_header(&mut temp)?;
        write_record(&mut temp, &encode_checkpoint(self.next_id))?;
        for entry in self.entries.values() {
            write_record(&mut temp, &encode_enqueue(entry)?)?;
        }
        for (key, receipt) in &self.receipts {
            write_record(&mut temp, &encode_keyed_receipt(key, *receipt)?)?;
        }
        temp.sync_all()
            .map_err(|error| io_error("sync outbox compaction file", error))?;

        std::fs::rename(&temp_path, &self.path)
            .map_err(|error| io_error("commit outbox compaction", error))?;

        match open_replacement(&self.path) {
            Ok(file) => {
                self.file_identity = match file.metadata() {
                    Ok(metadata) => JournalFileIdentity::from_metadata(&metadata),
                    Err(error) => {
                        let error = io_error("stat compacted outbox journal", error);
                        return self.poison_after_compaction_commit(error);
                    },
                };
                self.file = file;
            },
            Err(error) => {
                return self.poison_after_compaction_commit(error);
            },
        }

        if let Err(error) = sync_parent(&self.path) {
            return self.poison_after_compaction_commit(error);
        }
        self.journal_bytes = match self.file.metadata() {
            Ok(metadata) => metadata.len(),
            Err(error) => {
                let error = io_error("stat compacted outbox journal", error);
                return self.poison_after_compaction_commit(error);
            },
        };
        if self.journal_bytes > self.max_journal_bytes {
            self.poisoned = Some(format!(
                "compacted journal size {} exceeds physical limit {}",
                self.journal_bytes, self.max_journal_bytes
            ));
            return Err(PortError::new(
                PortErrorKind::Permanent,
                "compacted file outbox cannot satisfy its physical journal limit",
            ));
        }
        self.verify_journal_length()?;
        Ok(())
    }

    fn poison_after_compaction_commit<T>(&mut self, error: PortError) -> PortResult<T> {
        self.poisoned = Some(format!(
            "compaction rename committed but replacement durability was not confirmed: {error}"
        ));
        Err(error)
    }

    fn ensure_healthy(&self) -> PortResult<()> {
        if let Some(reason) = &self.poisoned {
            return Err(PortError::new(
                PortErrorKind::Permanent,
                format!("file outbox is unavailable after a fatal journal error: {reason}"),
            ));
        }
        Ok(())
    }

    fn stats(&mut self) -> PortResult<FileOutboxStats> {
        self.ensure_healthy()?;
        self.verify_journal_length()?;
        let quota_rejections = self
            .entry_quota_rejections
            .saturating_add(self.byte_quota_rejections)
            .saturating_add(self.physical_quota_rejections);
        Ok(FileOutboxStats {
            pending_entries: self.entries.len(),
            max_entries: self.capacity,
            current_live_bytes: self.current_live_bytes,
            max_live_bytes: self.max_live_bytes,
            journal_bytes: self.journal_bytes,
            max_journal_bytes: self.max_journal_bytes,
            quota_rejections,
            entry_quota_rejections: self.entry_quota_rejections,
            byte_quota_rejections: self.byte_quota_rejections,
            physical_quota_rejections: self.physical_quota_rejections,
            keyed_receipts: self.receipts.len(),
            keyed_receipt_bytes: self.receipt_bytes,
        })
    }

    fn compact_before_append(&mut self, append_bytes: u64) -> PortResult<()> {
        let projected = self
            .journal_bytes
            .checked_add(append_bytes)
            .ok_or_else(|| corrupt("outbox physical journal accounting overflow"))?;
        if projected > self.max_journal_bytes {
            self.compact()?;
        }
        let projected = self
            .journal_bytes
            .checked_add(append_bytes)
            .ok_or_else(|| corrupt("outbox physical journal accounting overflow"))?;
        if projected > self.max_journal_bytes {
            self.physical_quota_rejections = self.physical_quota_rejections.saturating_add(1);
            return Err(PortError::new(
                PortErrorKind::Unavailable,
                format!(
                    "file outbox physical journal limit {} cannot fit append of {} bytes",
                    self.max_journal_bytes, append_bytes
                ),
            ));
        }
        Ok(())
    }

    fn verify_journal_length(&mut self) -> PortResult<()> {
        let active_metadata = match self.file.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                self.poisoned = Some(format!("active journal cannot be inspected: {error}"));
                return Err(PortError::new(
                    PortErrorKind::Permanent,
                    format!("file outbox active journal cannot be inspected: {error}"),
                ));
            },
        };
        let path_metadata = match std::fs::metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) => {
                self.poisoned = Some(format!(
                    "canonical journal path cannot be inspected: {error}"
                ));
                return Err(PortError::new(
                    PortErrorKind::Permanent,
                    format!("file outbox canonical journal path cannot be inspected: {error}"),
                ));
            },
        };
        let active_identity = JournalFileIdentity::from_metadata(&active_metadata);
        let path_identity = JournalFileIdentity::from_metadata(&path_metadata);
        if active_identity != self.file_identity || path_identity != self.file_identity {
            self.poisoned = Some(
                "canonical journal path no longer identifies the active journal file".to_owned(),
            );
            return Err(PortError::new(
                PortErrorKind::Permanent,
                "file outbox canonical journal path was replaced outside its owner",
            ));
        }
        let actual = active_metadata.len();
        if actual != self.journal_bytes {
            self.poisoned = Some(format!(
                "journal changed outside its owner: expected {} bytes, found {actual}",
                self.journal_bytes
            ));
            return Err(PortError::new(
                PortErrorKind::Permanent,
                format!(
                    "file outbox journal changed outside its owner (expected {} bytes, found {actual})",
                    self.journal_bytes
                ),
            ));
        }
        Ok(())
    }
}

struct Recovered {
    next_id: u64,
    entries: BTreeMap<OutboxId, OutboxEntry>,
    receipts: BTreeMap<String, ReceiptState>,
    live_bytes: u64,
    receipt_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct IndexedEntry {
    record_offset: u64,
    payload_len: usize,
    live_bytes: u64,
}

struct RecoveryIndex {
    next_id: u64,
    entries: BTreeMap<OutboxId, IndexedEntry>,
    receipts: BTreeMap<String, ReceiptState>,
    live_bytes: u64,
    receipt_bytes: u64,
}

fn recover(file: &mut File, capacity: usize, max_live_bytes: u64) -> PortResult<Recovered> {
    let indexed = index_journal(file)?;
    // Check the final survivor set before allocating any destination or message
    // payload. Historical peaks may exceed the current configuration as long as
    // later durable ACK/release records bring the final state back under it.
    if indexed.live_bytes > max_live_bytes {
        return Err(PortError::new(
            PortErrorKind::InvalidData,
            format!(
                "recovered outbox live bytes {} exceed configured limit {max_live_bytes}",
                indexed.live_bytes
            ),
        ));
    }
    if indexed.entries.len() > capacity {
        return Err(PortError::new(
            PortErrorKind::InvalidData,
            format!(
                "recovered outbox entries {} exceed configured capacity {capacity}",
                indexed.entries.len()
            ),
        ));
    }

    let entries = materialize_indexed_entries(file, &indexed.entries)?;
    Ok(Recovered {
        next_id: indexed.next_id,
        entries,
        receipts: indexed.receipts,
        live_bytes: indexed.live_bytes,
        receipt_bytes: indexed.receipt_bytes,
    })
}

fn index_journal(file: &mut File) -> PortResult<RecoveryIndex> {
    let file_len = file
        .metadata()
        .map_err(|error| io_error("stat outbox journal", error))?
        .len();
    if file_len == 0 {
        write_file_header(file)?;
        return Ok(RecoveryIndex {
            next_id: 1,
            entries: BTreeMap::new(),
            receipts: BTreeMap::new(),
            live_bytes: 0,
            receipt_bytes: 0,
        });
    }
    if file_len < FILE_HEADER_LEN as u64 {
        return Err(corrupt("outbox journal has an incomplete file header"));
    }

    file.seek(SeekFrom::Start(0))
        .map_err(|error| io_error("seek outbox journal header", error))?;
    let mut file_header = [0_u8; FILE_HEADER_LEN];
    file.read_exact(&mut file_header)
        .map_err(|error| io_error("read outbox journal header", error))?;
    if &file_header[..8] != FILE_MAGIC {
        return Err(corrupt("outbox journal magic does not match"));
    }
    let reserved = u32::from_le_bytes(
        file_header[8..12]
            .try_into()
            .map_err(|_| corrupt("outbox journal reserved field is malformed"))?,
    );
    if reserved != 0 {
        return Err(corrupt("outbox journal reserved field must be zero"));
    }

    let mut recovered = RecoveryIndex {
        next_id: 1,
        entries: BTreeMap::new(),
        receipts: BTreeMap::new(),
        live_bytes: 0,
        receipt_bytes: 0,
    };
    let mut offset = FILE_HEADER_LEN as u64;
    while offset < file_len {
        let remaining = file_len - offset;
        if remaining < RECORD_HEADER_LEN as u64 {
            truncate_tail(file, offset)?;
            break;
        }

        file.seek(SeekFrom::Start(offset))
            .map_err(|error| io_error("seek outbox record", error))?;
        let mut record_header = [0_u8; RECORD_HEADER_LEN];
        file.read_exact(&mut record_header)
            .map_err(|error| io_error("read outbox record header", error))?;
        let magic = u32::from_le_bytes(
            record_header[..4]
                .try_into()
                .map_err(|_| corrupt("outbox record magic is malformed"))?,
        );
        if magic != RECORD_MAGIC {
            return Err(corrupt(format!(
                "outbox record at offset {offset} has invalid magic"
            )));
        }
        let payload_len = u32::from_le_bytes(
            record_header[4..8]
                .try_into()
                .map_err(|_| corrupt("outbox record length is malformed"))?,
        ) as usize;
        if payload_len == 0 || payload_len > MAX_RECORD_LEN {
            return Err(corrupt(format!(
                "outbox record at offset {offset} has invalid length {payload_len}"
            )));
        }
        let expected_checksum = u32::from_le_bytes(
            record_header[8..12]
                .try_into()
                .map_err(|_| corrupt("outbox record checksum is malformed"))?,
        );
        let record_end = offset
            .checked_add(RECORD_HEADER_LEN as u64)
            .and_then(|value| value.checked_add(payload_len as u64))
            .ok_or_else(|| corrupt("outbox record offset overflow"))?;
        if record_end > file_len {
            truncate_tail(file, offset)?;
            break;
        }

        let mut payload = vec![0_u8; payload_len];
        file.read_exact(&mut payload)
            .map_err(|error| io_error("read outbox record payload", error))?;
        if checksum(&payload) != expected_checksum {
            if record_end == file_len {
                truncate_tail(file, offset)?;
                break;
            }
            return Err(corrupt(format!(
                "outbox record at offset {offset} failed checksum"
            )));
        }
        apply_index_record(&payload, offset, &mut recovered)?;
        offset = record_end;
    }

    Ok(recovered)
}

fn apply_index_record(
    payload: &[u8],
    record_offset: u64,
    recovered: &mut RecoveryIndex,
) -> PortResult<()> {
    let operation = *payload
        .first()
        .ok_or_else(|| corrupt("outbox journal record is empty"))?;
    match operation {
        OP_ENQUEUE => {
            let fields = decode_enqueue_fields(payload)?;
            if recovered.entries.contains_key(&fields.id) {
                return Err(corrupt(format!(
                    "duplicate outbox enqueue identifier {}",
                    fields.id.get()
                )));
            }
            let next_id = fields
                .id
                .get()
                .checked_add(1)
                .ok_or_else(|| corrupt("outbox identifier exhausted in journal"))?;
            recovered.next_id = recovered.next_id.max(next_id);
            let record_bytes = encoded_record_bytes(payload.len())?;
            recovered.live_bytes = recovered
                .live_bytes
                .checked_add(record_bytes)
                .ok_or_else(|| corrupt("outbox live-byte accounting overflow"))?;
            recovered.entries.insert(
                fields.id,
                IndexedEntry {
                    record_offset,
                    payload_len: payload.len(),
                    live_bytes: record_bytes,
                },
            );
        },
        OP_ACKNOWLEDGE => {
            let mut cursor = ByteCursor::new(payload);
            let _operation = cursor.read_u8()?;
            let count = cursor.read_u32()? as usize;
            for _ in 0..count {
                if let Some(entry) = recovered.entries.remove(&OutboxId::new(cursor.read_u64()?)) {
                    recovered.live_bytes = recovered
                        .live_bytes
                        .checked_sub(entry.live_bytes)
                        .ok_or_else(|| corrupt("outbox live-byte accounting underflow"))?;
                }
            }
            cursor.finish()?;
        },
        OP_CHECKPOINT => {
            let mut cursor = ByteCursor::new(payload);
            let _operation = cursor.read_u8()?;
            let next_id = cursor.read_u64()?;
            cursor.finish()?;
            if next_id == 0 {
                return Err(corrupt("outbox checkpoint contains identifier zero"));
            }
            recovered.next_id = recovered.next_id.max(next_id);
        },
        OP_KEYED_ENQUEUE => {
            let fields = decode_keyed_enqueue_fields(payload)?;
            if recovered.entries.contains_key(&fields.entry.id) {
                return Err(corrupt(format!(
                    "duplicate keyed outbox enqueue identifier {}",
                    fields.entry.id.get()
                )));
            }
            if recovered.receipts.contains_key(fields.key) {
                return Err(corrupt("duplicate live outbox idempotency receipt"));
            }
            let next_id = fields
                .entry
                .id
                .get()
                .checked_add(1)
                .ok_or_else(|| corrupt("outbox identifier exhausted in journal"))?;
            recovered.next_id = recovered.next_id.max(next_id);
            let entry_bytes = encoded_enqueue_record_bytes_for_lengths(
                fields.entry.destination.len(),
                fields.entry.message_payload.len(),
            )?;
            let receipt_bytes = encoded_keyed_receipt_record_bytes(fields.key)?;
            recovered.live_bytes = recovered
                .live_bytes
                .checked_add(entry_bytes)
                .and_then(|bytes| bytes.checked_add(receipt_bytes))
                .ok_or_else(|| corrupt("keyed outbox live-byte accounting overflow"))?;
            recovered.receipt_bytes = recovered
                .receipt_bytes
                .checked_add(receipt_bytes)
                .ok_or_else(|| corrupt("keyed receipt byte accounting overflow"))?;
            recovered.entries.insert(
                fields.entry.id,
                IndexedEntry {
                    record_offset,
                    payload_len: payload.len(),
                    live_bytes: entry_bytes,
                },
            );
            recovered.receipts.insert(
                fields.key.to_owned(),
                ReceiptState {
                    outbox_id: fields.entry.id,
                    message_digest: fields.message_digest,
                },
            );
        },
        OP_RELEASE_KEY => {
            let mut cursor = ByteCursor::new(payload);
            let _operation = cursor.read_u8()?;
            let outbox_id = OutboxId::new(cursor.read_u64()?);
            let message_digest = read_digest(&mut cursor)?;
            let key_len = cursor.read_u32()? as usize;
            let key = std::str::from_utf8(cursor.read_bytes(key_len)?)
                .map_err(|_| corrupt("outbox idempotency key is not UTF-8"))?;
            cursor.finish()?;
            validate_idempotency_key(key)?;

            let Some(receipt) = recovered.receipts.remove(key) else {
                return Err(corrupt("release references a missing idempotency receipt"));
            };
            if receipt.outbox_id != outbox_id || receipt.message_digest != message_digest {
                return Err(corrupt("release does not match its idempotency receipt"));
            }
            let receipt_bytes = encoded_keyed_receipt_record_bytes(key)?;
            recovered.live_bytes = recovered
                .live_bytes
                .checked_sub(receipt_bytes)
                .ok_or_else(|| corrupt("keyed receipt live-byte accounting underflow"))?;
            recovered.receipt_bytes = recovered
                .receipt_bytes
                .checked_sub(receipt_bytes)
                .ok_or_else(|| corrupt("keyed receipt byte accounting underflow"))?;
        },
        OP_KEYED_RECEIPT => {
            let mut cursor = ByteCursor::new(payload);
            let _operation = cursor.read_u8()?;
            let outbox_id = OutboxId::new(cursor.read_u64()?);
            let message_digest = read_digest(&mut cursor)?;
            let key_len = cursor.read_u32()? as usize;
            let key = std::str::from_utf8(cursor.read_bytes(key_len)?)
                .map_err(|_| corrupt("outbox idempotency key is not UTF-8"))?;
            cursor.finish()?;
            validate_idempotency_key(key)?;

            if recovered
                .receipts
                .insert(
                    key.to_owned(),
                    ReceiptState {
                        outbox_id,
                        message_digest,
                    },
                )
                .is_some()
            {
                return Err(corrupt("duplicate compacted outbox idempotency receipt"));
            }
            let receipt_bytes = encoded_keyed_receipt_record_bytes(key)?;
            recovered.live_bytes = recovered
                .live_bytes
                .checked_add(receipt_bytes)
                .ok_or_else(|| corrupt("keyed receipt live-byte accounting overflow"))?;
            recovered.receipt_bytes = recovered
                .receipt_bytes
                .checked_add(receipt_bytes)
                .ok_or_else(|| corrupt("keyed receipt byte accounting overflow"))?;
        },
        operation => {
            return Err(corrupt(format!(
                "unknown outbox journal operation {operation}"
            )));
        },
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct EnqueueFields<'a> {
    id: OutboxId,
    created_at: TimestampMs,
    attempts: u32,
    destination: &'a str,
    message_payload: &'a [u8],
}

struct KeyedEnqueueFields<'a> {
    entry: EnqueueFields<'a>,
    key: &'a str,
    message_digest: [u8; 32],
}

fn decode_enqueue_fields(payload: &[u8]) -> PortResult<EnqueueFields<'_>> {
    let mut cursor = ByteCursor::new(payload);
    if cursor.read_u8()? != OP_ENQUEUE {
        return Err(corrupt("indexed outbox entry is not an enqueue record"));
    }
    let id = OutboxId::new(cursor.read_u64()?);
    let created_at = TimestampMs::new(cursor.read_u64()?);
    let attempts = cursor.read_u32()?;
    let destination_len = cursor.read_u32()? as usize;
    let payload_len = cursor.read_u32()? as usize;
    let destination = std::str::from_utf8(cursor.read_bytes(destination_len)?)
        .map_err(|_| corrupt("outbox destination is not UTF-8"))?;
    let message_payload = cursor.read_bytes(payload_len)?;
    cursor.finish()?;
    Ok(EnqueueFields {
        id,
        created_at,
        attempts,
        destination,
        message_payload,
    })
}

fn decode_keyed_enqueue_fields(payload: &[u8]) -> PortResult<KeyedEnqueueFields<'_>> {
    let mut cursor = ByteCursor::new(payload);
    if cursor.read_u8()? != OP_KEYED_ENQUEUE {
        return Err(corrupt("indexed keyed entry is not a keyed enqueue record"));
    }
    let id = OutboxId::new(cursor.read_u64()?);
    let created_at = TimestampMs::new(cursor.read_u64()?);
    let attempts = cursor.read_u32()?;
    let destination_len = cursor.read_u32()? as usize;
    let payload_len = cursor.read_u32()? as usize;
    let key_len = cursor.read_u32()? as usize;
    let message_digest = read_digest(&mut cursor)?;
    let destination = std::str::from_utf8(cursor.read_bytes(destination_len)?)
        .map_err(|_| corrupt("outbox destination is not UTF-8"))?;
    let message_payload = cursor.read_bytes(payload_len)?;
    let key = std::str::from_utf8(cursor.read_bytes(key_len)?)
        .map_err(|_| corrupt("outbox idempotency key is not UTF-8"))?;
    cursor.finish()?;
    validate_idempotency_key(key)?;
    if outbox_message_digest_parts(destination, message_payload) != message_digest {
        return Err(corrupt(
            "keyed outbox message digest does not match its payload",
        ));
    }
    Ok(KeyedEnqueueFields {
        entry: EnqueueFields {
            id,
            created_at,
            attempts,
            destination,
            message_payload,
        },
        key,
        message_digest,
    })
}

fn materialize_indexed_entries(
    file: &mut File,
    indexed: &BTreeMap<OutboxId, IndexedEntry>,
) -> PortResult<BTreeMap<OutboxId, OutboxEntry>> {
    let mut survivors = indexed
        .iter()
        .map(|(id, entry)| (*id, *entry))
        .collect::<Vec<_>>();
    survivors.sort_unstable_by_key(|(_, entry)| entry.record_offset);

    let mut entries = BTreeMap::new();
    for (expected_id, indexed_entry) in survivors {
        let payload = read_indexed_record(file, indexed_entry)?;
        let fields = match payload.first().copied() {
            Some(OP_ENQUEUE) => decode_enqueue_fields(&payload)?,
            Some(OP_KEYED_ENQUEUE) => decode_keyed_enqueue_fields(&payload)?.entry,
            _ => return Err(corrupt("indexed outbox survivor is not an enqueue record")),
        };
        if fields.id != expected_id {
            return Err(corrupt("indexed outbox survivor identifier changed"));
        }
        let entry = OutboxEntry::new(
            fields.id,
            OutboxMessage::new(
                fields.destination.to_owned(),
                fields.message_payload.to_vec(),
                fields.created_at,
            ),
            fields.attempts,
        );
        entries.insert(fields.id, entry);
    }
    Ok(entries)
}

fn read_indexed_record(file: &mut File, indexed: IndexedEntry) -> PortResult<Vec<u8>> {
    file.seek(SeekFrom::Start(indexed.record_offset))
        .map_err(|error| io_error("seek indexed outbox record", error))?;
    let mut header = [0_u8; RECORD_HEADER_LEN];
    file.read_exact(&mut header)
        .map_err(|error| io_error("read indexed outbox record header", error))?;
    let magic = u32::from_le_bytes(
        header[..4]
            .try_into()
            .map_err(|_| corrupt("indexed outbox record magic is malformed"))?,
    );
    let payload_len = u32::from_le_bytes(
        header[4..8]
            .try_into()
            .map_err(|_| corrupt("indexed outbox record length is malformed"))?,
    ) as usize;
    let expected_checksum = u32::from_le_bytes(
        header[8..12]
            .try_into()
            .map_err(|_| corrupt("indexed outbox record checksum is malformed"))?,
    );
    if magic != RECORD_MAGIC || payload_len != indexed.payload_len {
        return Err(corrupt("indexed outbox record changed during recovery"));
    }
    let mut payload = vec![0_u8; payload_len];
    file.read_exact(&mut payload)
        .map_err(|error| io_error("read indexed outbox record payload", error))?;
    if checksum(&payload) != expected_checksum {
        return Err(corrupt("indexed outbox record changed during recovery"));
    }
    Ok(payload)
}

fn encode_enqueue(entry: &OutboxEntry) -> PortResult<Vec<u8>> {
    let destination = entry.message().destination().as_bytes();
    let message_payload = entry.message().payload();
    let destination_len =
        u32::try_from(destination.len()).map_err(|_| corrupt("outbox destination is too large"))?;
    let payload_len =
        u32::try_from(message_payload.len()).map_err(|_| corrupt("outbox payload is too large"))?;

    let mut bytes = Vec::with_capacity(29 + destination.len() + message_payload.len());
    bytes.push(OP_ENQUEUE);
    bytes.extend_from_slice(&entry.id().get().to_le_bytes());
    bytes.extend_from_slice(&entry.message().created_at().get().to_le_bytes());
    bytes.extend_from_slice(&entry.attempts().to_le_bytes());
    bytes.extend_from_slice(&destination_len.to_le_bytes());
    bytes.extend_from_slice(&payload_len.to_le_bytes());
    bytes.extend_from_slice(destination);
    bytes.extend_from_slice(message_payload);
    validate_record_size(&bytes)?;
    Ok(bytes)
}

fn encode_keyed_enqueue(
    key: &str,
    entry: &OutboxEntry,
    message_digest: [u8; 32],
) -> PortResult<Vec<u8>> {
    validate_idempotency_key(key)?;
    let destination = entry.message().destination().as_bytes();
    let message_payload = entry.message().payload();
    let destination_len =
        u32::try_from(destination.len()).map_err(|_| corrupt("outbox destination is too large"))?;
    let payload_len =
        u32::try_from(message_payload.len()).map_err(|_| corrupt("outbox payload is too large"))?;
    let key_len = u32::try_from(key.len()).map_err(|_| corrupt("idempotency key is too large"))?;
    let capacity = 65_usize
        .checked_add(destination.len())
        .and_then(|bytes| bytes.checked_add(message_payload.len()))
        .and_then(|bytes| bytes.checked_add(key.len()))
        .ok_or_else(|| corrupt("keyed enqueue record size overflow"))?;

    let mut bytes = Vec::with_capacity(capacity);
    bytes.push(OP_KEYED_ENQUEUE);
    bytes.extend_from_slice(&entry.id().get().to_le_bytes());
    bytes.extend_from_slice(&entry.message().created_at().get().to_le_bytes());
    bytes.extend_from_slice(&entry.attempts().to_le_bytes());
    bytes.extend_from_slice(&destination_len.to_le_bytes());
    bytes.extend_from_slice(&payload_len.to_le_bytes());
    bytes.extend_from_slice(&key_len.to_le_bytes());
    bytes.extend_from_slice(&message_digest);
    bytes.extend_from_slice(destination);
    bytes.extend_from_slice(message_payload);
    bytes.extend_from_slice(key.as_bytes());
    validate_record_size(&bytes)?;
    Ok(bytes)
}

fn encode_release_key(key: &str, receipt: ReceiptState) -> PortResult<Vec<u8>> {
    encode_receipt_operation(OP_RELEASE_KEY, key, receipt)
}

fn encode_keyed_receipt(key: &str, receipt: ReceiptState) -> PortResult<Vec<u8>> {
    encode_receipt_operation(OP_KEYED_RECEIPT, key, receipt)
}

fn encode_receipt_operation(
    operation: u8,
    key: &str,
    receipt: ReceiptState,
) -> PortResult<Vec<u8>> {
    validate_idempotency_key(key)?;
    let key_len = u32::try_from(key.len()).map_err(|_| corrupt("idempotency key is too large"))?;
    let capacity = 45_usize
        .checked_add(key.len())
        .ok_or_else(|| corrupt("idempotency receipt record size overflow"))?;
    let mut bytes = Vec::with_capacity(capacity);
    bytes.push(operation);
    bytes.extend_from_slice(&receipt.outbox_id.get().to_le_bytes());
    bytes.extend_from_slice(&receipt.message_digest);
    bytes.extend_from_slice(&key_len.to_le_bytes());
    bytes.extend_from_slice(key.as_bytes());
    validate_record_size(&bytes)?;
    Ok(bytes)
}

fn encoded_keyed_receipt_record_bytes(key: &str) -> PortResult<u64> {
    validate_idempotency_key(key)?;
    let payload_bytes = 45_usize
        .checked_add(key.len())
        .ok_or_else(|| corrupt("idempotency receipt record size overflow"))?;
    encoded_record_bytes(payload_bytes)
}

fn validate_idempotency_key(key: &str) -> PortResult<()> {
    if key.is_empty() {
        return Err(PortError::new(
            PortErrorKind::InvalidData,
            "outbox idempotency key must not be empty",
        ));
    }
    if key.len() > MAX_IDEMPOTENCY_KEY_BYTES {
        return Err(PortError::new(
            PortErrorKind::InvalidData,
            format!("outbox idempotency key exceeds {MAX_IDEMPOTENCY_KEY_BYTES} bytes"),
        ));
    }
    Ok(())
}

fn read_digest(cursor: &mut ByteCursor<'_>) -> PortResult<[u8; 32]> {
    cursor
        .read_bytes(32)?
        .try_into()
        .map_err(|_| corrupt("outbox message digest is malformed"))
}

fn encoded_enqueue_record_bytes(entry: &OutboxEntry) -> PortResult<u64> {
    encoded_enqueue_record_bytes_for_lengths(
        entry.message().destination().len(),
        entry.message().payload().len(),
    )
}

fn encoded_enqueue_record_bytes_for_lengths(
    destination_len: usize,
    payload_len: usize,
) -> PortResult<u64> {
    let payload_bytes = 29_usize
        .checked_add(destination_len)
        .and_then(|bytes| bytes.checked_add(payload_len))
        .ok_or_else(|| corrupt("outbox enqueue record size overflow"))?;
    validate_record_size_len(payload_bytes)?;
    encoded_record_bytes(payload_bytes)
}

fn encoded_record_bytes(payload_len: usize) -> PortResult<u64> {
    let total = RECORD_HEADER_LEN
        .checked_add(payload_len)
        .ok_or_else(|| corrupt("outbox encoded record size overflow"))?;
    u64::try_from(total).map_err(|_| corrupt("outbox encoded record size cannot be represented"))
}

fn encode_acknowledge(ids: &[OutboxId]) -> PortResult<Vec<u8>> {
    let count =
        u32::try_from(ids.len()).map_err(|_| corrupt("too many outbox acknowledgements"))?;
    let mut bytes = Vec::with_capacity(5 + ids.len() * std::mem::size_of::<u64>());
    bytes.push(OP_ACKNOWLEDGE);
    bytes.extend_from_slice(&count.to_le_bytes());
    for id in ids {
        bytes.extend_from_slice(&id.get().to_le_bytes());
    }
    validate_record_size(&bytes)?;
    Ok(bytes)
}

fn encode_checkpoint(next_id: u64) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(9);
    bytes.push(OP_CHECKPOINT);
    bytes.extend_from_slice(&next_id.to_le_bytes());
    bytes
}

fn validate_record_size(payload: &[u8]) -> PortResult<()> {
    validate_record_size_len(payload.len())
}

fn validate_record_size_len(payload_len: usize) -> PortResult<()> {
    if payload_len == 0 || payload_len > MAX_RECORD_LEN {
        return Err(PortError::new(
            PortErrorKind::InvalidData,
            format!("outbox journal record size {payload_len} exceeds maximum {MAX_RECORD_LEN}",),
        ));
    }
    Ok(())
}

fn append_record(file: &mut File, payload: &[u8]) -> PortResult<()> {
    validate_record_size(payload)?;
    let start = file
        .seek(SeekFrom::End(0))
        .map_err(|error| io_error("seek outbox append position", error))?;
    let result = write_record(file, payload).and_then(|()| {
        file.sync_data()
            .map_err(|error| io_error("sync outbox journal record", error))
    });
    if result.is_err() {
        let _ = file.set_len(start);
        let _ = file.seek(SeekFrom::End(0));
        let _ = file.sync_data();
    }
    result
}

fn write_record(file: &mut File, payload: &[u8]) -> PortResult<()> {
    validate_record_size(payload)?;
    let payload_len = u32::try_from(payload.len())
        .map_err(|_| corrupt("outbox record length cannot be represented"))?;
    let mut header = [0_u8; RECORD_HEADER_LEN];
    header[..4].copy_from_slice(&RECORD_MAGIC.to_le_bytes());
    header[4..8].copy_from_slice(&payload_len.to_le_bytes());
    header[8..12].copy_from_slice(&checksum(payload).to_le_bytes());
    file.write_all(&header)
        .and_then(|()| file.write_all(payload))
        .map_err(|error| io_error("write outbox journal record", error))
}

fn write_file_header(file: &mut File) -> PortResult<()> {
    file.seek(SeekFrom::Start(0))
        .and_then(|_| file.set_len(0))
        .map_err(|error| io_error("reset outbox journal", error))?;
    let mut header = [0_u8; FILE_HEADER_LEN];
    header[..8].copy_from_slice(FILE_MAGIC);
    file.write_all(&header)
        .and_then(|()| file.sync_all())
        .map_err(|error| io_error("initialize outbox journal", error))
}

fn truncate_tail(file: &mut File, valid_len: u64) -> PortResult<()> {
    file.set_len(valid_len)
        .and_then(|()| file.sync_data())
        .and_then(|()| file.seek(SeekFrom::End(0)).map(|_| ()))
        .map_err(|error| io_error("truncate incomplete outbox journal tail", error))
}

fn open_or_create_journal_file(path: &Path) -> PortResult<(File, bool)> {
    let create_result = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(path);
    match create_result {
        Ok(file) => Ok((file, true)),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            open_existing_journal_file(path).map(|file| (file, false))
        },
        Err(error) => Err(io_error("create outbox journal", error)),
    }
}

fn open_existing_journal_file(path: &Path) -> PortResult<File> {
    OpenOptions::new()
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| io_error("open outbox journal", error))
}

fn open_compacted_journal_file(path: &Path) -> PortResult<File> {
    let mut replacement = open_existing_journal_file(path)?;
    replacement
        .seek(SeekFrom::End(0))
        .map_err(|error| io_error("seek compacted outbox journal", error))?;
    Ok(replacement)
}

fn sibling_path(path: &Path, suffix: &str) -> PathBuf {
    let mut file_name = path
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("outbox"))
        .to_os_string();
    file_name.push(suffix);
    path.with_file_name(file_name)
}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> PortResult<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| io_error("sync outbox parent directory", error))
}

#[cfg(not(unix))]
fn sync_parent_directory(_path: &Path) -> PortResult<()> {
    Ok(())
}

fn checksum(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

struct ByteCursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> ByteCursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn read_u8(&mut self) -> PortResult<u8> {
        Ok(self.read_bytes(1)?[0])
    }

    fn read_u32(&mut self) -> PortResult<u32> {
        let bytes = self.read_bytes(4)?;
        Ok(u32::from_le_bytes(
            bytes
                .try_into()
                .map_err(|_| corrupt("outbox u32 field is malformed"))?,
        ))
    }

    fn read_u64(&mut self) -> PortResult<u64> {
        let bytes = self.read_bytes(8)?;
        Ok(u64::from_le_bytes(
            bytes
                .try_into()
                .map_err(|_| corrupt("outbox u64 field is malformed"))?,
        ))
    }

    fn read_bytes(&mut self, length: usize) -> PortResult<&'a [u8]> {
        let end = self
            .position
            .checked_add(length)
            .ok_or_else(|| corrupt("outbox record offset overflow"))?;
        let bytes = self
            .bytes
            .get(self.position..end)
            .ok_or_else(|| corrupt("outbox record ended before all fields were decoded"))?;
        self.position = end;
        Ok(bytes)
    }

    fn finish(self) -> PortResult<()> {
        if self.position != self.bytes.len() {
            return Err(corrupt("outbox record contains trailing bytes"));
        }
        Ok(())
    }
}

fn corrupt(message: impl Into<String>) -> PortError {
    PortError::new(PortErrorKind::InvalidData, message)
}

fn io_error(context: &str, error: std::io::Error) -> PortError {
    let kind = match error.kind() {
        std::io::ErrorKind::PermissionDenied
        | std::io::ErrorKind::InvalidInput
        | std::io::ErrorKind::InvalidData
        | std::io::ErrorKind::Unsupported => PortErrorKind::Permanent,
        std::io::ErrorKind::AlreadyExists => PortErrorKind::Conflict,
        _ => PortErrorKind::Unavailable,
    };
    PortError::new(kind, format!("{context}: {error}"))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    fn test_message(sequence: u64) -> OutboxMessage {
        OutboxMessage::new(
            "test/topic",
            format!("payload-{sequence}").into_bytes(),
            TimestampMs::new(sequence),
        )
    }

    fn injected_failure(message: &str) -> PortError {
        PortError::new(PortErrorKind::Unavailable, message)
    }

    #[test]
    fn open_synchronizes_the_parent_for_new_and_existing_journals() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("parent-sync.outbox");
        let sync_calls = Cell::new(0_u32);
        let journal = Journal::open_with_parent_sync(path.clone(), 8, 1024, |synced_path| {
            assert_eq!(synced_path, path);
            sync_calls.set(sync_calls.get() + 1);
            Ok(())
        })
        .expect("open newly created journal");
        assert_eq!(sync_calls.get(), 1);
        drop(journal);

        let journal = Journal::open_with_parent_sync(path.clone(), 8, 1024, |synced_path| {
            assert_eq!(synced_path, path);
            sync_calls.set(sync_calls.get() + 1);
            Ok(())
        })
        .expect("reopen existing journal");
        assert_eq!(sync_calls.get(), 2);
        drop(journal);

        let failed_path = directory.path().join("failed-parent-sync.outbox");
        let Err(error) = Journal::open_with_parent_sync(failed_path, 8, 1024, |_| {
            Err(injected_failure("injected parent sync failure"))
        }) else {
            panic!("a parent-directory sync failure must fail journal open");
        };
        assert_eq!(error.kind(), PortErrorKind::Unavailable);
    }

    #[test]
    fn replacement_open_failure_after_compaction_rename_poisoned_the_journal() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("replacement-open-failure.outbox");
        let mut journal = Journal::open(path, 8, 1024).expect("open journal");
        journal.enqueue(test_message(1)).expect("enqueue entry");

        let error = journal
            .compact_with_hooks(
                |_| Err(injected_failure("injected replacement open failure")),
                |_| Ok(()),
            )
            .expect_err("replacement open failure must fail compaction");
        assert_eq!(error.kind(), PortErrorKind::Unavailable);
        assert_eq!(
            journal
                .enqueue(test_message(2))
                .expect_err("post-rename failure must poison future mutations")
                .kind(),
            PortErrorKind::Permanent
        );
    }

    #[test]
    fn parent_sync_failure_after_compaction_rename_poisoned_the_journal() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory
            .path()
            .join("compaction-parent-sync-failure.outbox");
        let mut journal = Journal::open(path, 8, 1024).expect("open journal");
        journal.enqueue(test_message(1)).expect("enqueue entry");

        let error = journal
            .compact_with_hooks(open_compacted_journal_file, |_| {
                Err(injected_failure("injected parent sync failure"))
            })
            .expect_err("parent sync failure must fail compaction");
        assert_eq!(error.kind(), PortErrorKind::Unavailable);
        assert_eq!(
            journal
                .enqueue(test_message(2))
                .expect_err("ambiguous replacement must poison future mutations")
                .kind(),
            PortErrorKind::Permanent
        );
    }
}
