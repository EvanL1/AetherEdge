//! Crash-recoverable file implementation of the dedicated CloudLink spool.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use aether_ports::{
    CloudLinkAdmission, CloudLinkDataLossEvidence, CloudLinkDeliveryState, CloudLinkDurableAck,
    CloudLinkEnqueue, CloudLinkReceiptRetention, CloudLinkRecord, CloudLinkRecordIdentity,
    CloudLinkReplayWindow, CloudLinkSessionBinding, CloudLinkSpool, CloudLinkSpoolError,
    CloudLinkSpoolErrorReason, CloudLinkSpoolStatus, DurableAckOutcome,
};
use async_trait::async_trait;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

use crate::cloudlink_spool::{
    CloudLinkAcknowledgedReceipt, CloudLinkSpoolState, DEFAULT_CLOUDLINK_SPOOL_MAX_LIVE_BYTES,
    MAX_ACKNOWLEDGED_RECEIPT_CAPACITY, MAX_SPOOL_PAYLOAD_BYTES, error, receipt_live_bytes,
    record_live_bytes,
};

const MAGIC: &[u8; 8] = b"AETHCLD\n";
const MAX_JOURNAL_RECORD_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_ACKNOWLEDGED_RECEIPT_CAPACITY: usize = 100_000;
const MIN_COMPACTION_RECLAIM_BYTES: u64 = 1024 * 1024;
pub const DEFAULT_CLOUDLINK_SPOOL_MAX_JOURNAL_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_CLOUDLINK_SPOOL_MAX_JOURNAL_BYTES: u64 = 32 * 1024 * 1024 * 1024;
pub const CLOUDLINK_SPOOL_MIN_JOURNAL_HEADROOM_BYTES: u64 = 64 * 1024;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "kebab-case", deny_unknown_fields)]
enum JournalEntry {
    Checkpoint {
        stream_id: String,
        stream_epoch: u64,
        next_position: u64,
        capacity: usize,
        acknowledged_receipt_capacity: usize,
        max_live_bytes: u64,
        last_ack: Option<CloudLinkDurableAck>,
        last_acknowledged_position: u64,
        data_loss: Option<CloudLinkDataLossEvidence>,
    },
    Receipt {
        receipt: CloudLinkAcknowledgedReceipt,
    },
    Record {
        record: CloudLinkRecord,
        data_loss_report: Option<CloudLinkDataLossEvidence>,
    },
    Enqueued {
        record: CloudLinkRecord,
        next_position: u64,
        data_loss: Option<CloudLinkDataLossEvidence>,
        data_loss_report: Option<CloudLinkDataLossEvidence>,
    },
    Offered {
        identity: CloudLinkRecordIdentity,
        session: CloudLinkSessionBinding,
    },
    TransportPublished {
        identity: CloudLinkRecordIdentity,
        session: CloudLinkSessionBinding,
    },
    Acknowledged {
        ack: CloudLinkDurableAck,
    },
    Rotated {
        stream_epoch: u64,
    },
    Capacity {
        capacity: usize,
        acknowledged_receipt_capacity: usize,
        max_live_bytes: u64,
    },
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

struct FileState {
    file: File,
    state: CloudLinkSpoolState,
    file_identity: JournalFileIdentity,
    journal_bytes: u64,
    max_journal_bytes: u64,
    physical_quota_rejections: u64,
    poisoned: Option<String>,
    lock_path: PathBuf,
    lock_identity: JournalFileIdentity,
    _lock_file: File,
}

/// Incremental-journal-backed CloudLink spool with exclusive process ownership.
pub struct FileCloudLinkSpool {
    path: PathBuf,
    inner: Arc<Mutex<FileState>>,
    operation_gate: Arc<Semaphore>,
}

impl std::fmt::Debug for FileCloudLinkSpool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FileCloudLinkSpool")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl FileCloudLinkSpool {
    /// Opens or creates a process-exclusive crash-recoverable stream journal.
    pub fn open(
        path: impl AsRef<Path>,
        stream_id: &str,
        capacity: usize,
    ) -> Result<Self, CloudLinkSpoolError> {
        Self::open_with_limits(
            path,
            stream_id,
            capacity,
            DEFAULT_ACKNOWLEDGED_RECEIPT_CAPACITY,
            DEFAULT_CLOUDLINK_SPOOL_MAX_LIVE_BYTES,
            DEFAULT_CLOUDLINK_SPOOL_MAX_JOURNAL_BYTES,
        )
    }

    /// Opens one stream with explicit pending-record and protected-receipt bounds.
    pub fn open_with_receipt_capacity(
        path: impl AsRef<Path>,
        stream_id: &str,
        capacity: usize,
        acknowledged_receipt_capacity: usize,
    ) -> Result<Self, CloudLinkSpoolError> {
        Self::open_with_limits(
            path,
            stream_id,
            capacity,
            acknowledged_receipt_capacity,
            DEFAULT_CLOUDLINK_SPOOL_MAX_LIVE_BYTES,
            DEFAULT_CLOUDLINK_SPOOL_MAX_JOURNAL_BYTES,
        )
    }

    /// Opens one stream with explicit count, receipt, live-byte, and physical bounds.
    #[allow(clippy::too_many_arguments)]
    pub fn open_with_limits(
        path: impl AsRef<Path>,
        stream_id: &str,
        capacity: usize,
        acknowledged_receipt_capacity: usize,
        max_live_bytes: u64,
        max_journal_bytes: u64,
    ) -> Result<Self, CloudLinkSpoolError> {
        if !(crate::cloudlink_spool::MIN_CLOUDLINK_SPOOL_MAX_LIVE_BYTES
            ..=crate::cloudlink_spool::MAX_CLOUDLINK_SPOOL_MAX_LIVE_BYTES)
            .contains(&max_live_bytes)
        {
            return Err(error(
                CloudLinkSpoolErrorReason::InvalidData,
                "CloudLink spool live-byte limit is outside its supported range",
            ));
        }
        let minimum_journal_bytes = max_live_bytes
            .checked_add(CLOUDLINK_SPOOL_MIN_JOURNAL_HEADROOM_BYTES)
            .ok_or_else(|| {
                error(
                    CloudLinkSpoolErrorReason::InvalidData,
                    "CloudLink physical journal limit overflow",
                )
            })?;
        if max_journal_bytes < minimum_journal_bytes
            || max_journal_bytes > MAX_CLOUDLINK_SPOOL_MAX_JOURNAL_BYTES
        {
            return Err(error(
                CloudLinkSpoolErrorReason::InvalidData,
                format!(
                    "CloudLink physical journal limit must be between {minimum_journal_bytes} and {MAX_CLOUDLINK_SPOOL_MAX_JOURNAL_BYTES} bytes"
                ),
            ));
        }

        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .map_err(|source| storage_error(&path, "create parent directory", source))?;
        }
        let lock_path = sibling_path(&path, ".lock");
        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|source| storage_error(&lock_path, "open stable lock", source))?;
        lock_file.try_lock_exclusive().map_err(|source| {
            storage_error(&lock_path, "acquire exclusive process lock", source)
        })?;
        let lock_identity =
            JournalFileIdentity::from_metadata(&lock_file.metadata().map_err(|source| {
                storage_error(&lock_path, "read stable lock identity", source)
            })?);
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|source| storage_error(&path, "open", source))?;

        let length = file
            .metadata()
            .map_err(|source| storage_error(&path, "read metadata", source))?
            .len();
        let (mut state, created) = if length == 0 {
            let state = CloudLinkSpoolState::new_with_limits(
                stream_id,
                capacity,
                acknowledged_receipt_capacity,
                max_live_bytes,
            )?;
            write_compacted_journal(&path, &mut file, &state)?;
            (state, true)
        } else {
            (recover(&path, &mut file)?.0, false)
        };

        let limits_changed = state.validate_open(
            stream_id,
            capacity,
            acknowledged_receipt_capacity,
            max_live_bytes,
        )?;
        let journal_bytes = file
            .seek(SeekFrom::End(0))
            .map_err(|source| storage_error(&path, "seek append position", source))?;
        let file_identity = JournalFileIdentity::from_metadata(
            &file
                .metadata()
                .map_err(|source| storage_error(&path, "read active journal identity", source))?,
        );

        let spool = Self {
            path: path.clone(),
            inner: Arc::new(Mutex::new(FileState {
                file,
                state,
                file_identity,
                journal_bytes,
                max_journal_bytes,
                physical_quota_rejections: 0,
                poisoned: None,
                lock_path,
                lock_identity,
                _lock_file: lock_file,
            })),
            operation_gate: Arc::new(Semaphore::new(1)),
        };
        {
            let mut guard = spool.lock()?;
            verify_journal_identity(&path, &mut guard)?;
            if limits_changed {
                let next = guard.state.clone();
                persist_mutation(
                    &path,
                    &mut guard,
                    &next,
                    &JournalEntry::Capacity {
                        capacity,
                        acknowledged_receipt_capacity,
                        max_live_bytes,
                    },
                )?;
            }
            if guard.journal_bytes > max_journal_bytes {
                let state = guard.state.clone();
                compact_locked_to(&path, &mut guard, &state)?;
            }
        }
        if created {
            sync_parent_directory(&path)?;
        }
        Ok(spool)
    }

    /// Atomically rewrites the journal with only live records and cursor metadata.
    pub fn compact(&self) -> Result<(), CloudLinkSpoolError> {
        let mut guard = self.lock()?;
        let state = guard.state.clone();
        compact_locked_to(&self.path, &mut guard, &state)
    }

    /// Returns current stream metadata without requiring an async composition step.
    ///
    /// This is intended for fail-fast process composition before tasks are
    /// spawned. Runtime code should continue to use the port method.
    pub fn current_status(&self) -> Result<CloudLinkSpoolStatus, CloudLinkSpoolError> {
        let mut guard = self.lock()?;
        verify_journal_identity(&self.path, &mut guard)?;
        Ok(guard.state.status_with_journal(
            guard.journal_bytes,
            guard.max_journal_bytes,
            guard.physical_quota_rejections,
        ))
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, FileState>, CloudLinkSpoolError> {
        self.inner.lock().map_err(|_| {
            error(
                CloudLinkSpoolErrorReason::Storage,
                "CloudLink file spool lock was poisoned",
            )
        })
    }

    async fn run_blocking<T, Operation>(
        &self,
        operation: Operation,
    ) -> Result<T, CloudLinkSpoolError>
    where
        T: Send + 'static,
        Operation: FnOnce(&Path, &mut FileState) -> Result<T, CloudLinkSpoolError> + Send + 'static,
    {
        let permit = Arc::clone(&self.operation_gate)
            .acquire_owned()
            .await
            .map_err(|_| {
                error(
                    CloudLinkSpoolErrorReason::Storage,
                    "CloudLink file spool operation gate closed",
                )
            })?;
        let inner = Arc::clone(&self.inner);
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut guard = inner.lock().map_err(|_| {
                error(
                    CloudLinkSpoolErrorReason::Storage,
                    "CloudLink file spool lock was poisoned",
                )
            })?;
            operation(&path, &mut guard)
        })
        .await
        .map_err(|join_error| {
            error(
                CloudLinkSpoolErrorReason::Storage,
                format!("CloudLink file spool blocking operation failed: {join_error}"),
            )
        })?
    }

    async fn mutate<T, Operation>(&self, operation: Operation) -> Result<T, CloudLinkSpoolError>
    where
        T: Send + 'static,
        Operation: FnOnce(&mut CloudLinkSpoolState) -> Result<(T, JournalEntry), CloudLinkSpoolError>
            + Send
            + 'static,
    {
        self.run_blocking(move |path, guard| {
            ensure_healthy(guard)?;
            verify_journal_identity(path, guard)?;
            let mut next = guard.state.clone();
            let outcome = operation(&mut next);
            let (result, entry) = match outcome {
                Ok(value) => value,
                Err(error) => {
                    guard.state.quota_rejections =
                        guard.state.quota_rejections.max(next.quota_rejections);
                    return Err(error);
                },
            };
            if next != guard.state {
                persist_mutation(path, guard, &next, &entry)?;
                guard.state = next;
            }
            Ok(result)
        })
        .await
    }
}

#[async_trait]
impl CloudLinkSpool for FileCloudLinkSpool {
    async fn enqueue(
        &self,
        input: CloudLinkEnqueue,
    ) -> Result<CloudLinkRecord, CloudLinkSpoolError> {
        self.mutate(move |state| {
            let record = state.enqueue(input)?;
            let data_loss_report = state
                .data_loss_reports
                .get(&record.identity().position())
                .cloned();
            let entry = JournalEntry::Enqueued {
                record: record.clone(),
                next_position: state.next_position,
                data_loss: state.data_loss.clone(),
                data_loss_report,
            };
            Ok((record, entry))
        })
        .await
    }

    async fn admit_lossless(
        &self,
        input: CloudLinkEnqueue,
        receipt_retention: CloudLinkReceiptRetention,
    ) -> Result<CloudLinkAdmission, CloudLinkSpoolError> {
        self.mutate(move |state| {
            let admission = state.admit_lossless(input, receipt_retention)?;
            let entry = match admission.pending_record() {
                Some(record) => JournalEntry::Enqueued {
                    record: record.clone(),
                    next_position: state.next_position,
                    data_loss: state.data_loss.clone(),
                    data_loss_report: state
                        .data_loss_reports
                        .get(&record.identity().position())
                        .cloned(),
                },
                None => checkpoint(state),
            };
            Ok((admission, entry))
        })
        .await
    }

    async fn admit_data_loss(
        &self,
        input: CloudLinkEnqueue,
        evidence: &CloudLinkDataLossEvidence,
    ) -> Result<CloudLinkAdmission, CloudLinkSpoolError> {
        let evidence = evidence.clone();
        self.mutate(move |state| {
            let admission = state.admit_data_loss(input, &evidence)?;
            let entry = match admission.pending_record() {
                Some(record) => JournalEntry::Enqueued {
                    record: record.clone(),
                    next_position: state.next_position,
                    data_loss: state.data_loss.clone(),
                    data_loss_report: state
                        .data_loss_reports
                        .get(&record.identity().position())
                        .cloned(),
                },
                None => checkpoint(state),
            };
            Ok((admission, entry))
        })
        .await
    }

    async fn replay_from(
        &self,
        requested_position: u64,
        limit: usize,
    ) -> Result<CloudLinkReplayWindow, CloudLinkSpoolError> {
        self.run_blocking(move |path, guard| {
            ensure_healthy(guard)?;
            verify_journal_identity(path, guard)?;
            guard.state.replay_from(requested_position, limit)
        })
        .await
    }

    async fn mark_offered(
        &self,
        identity: &CloudLinkRecordIdentity,
        session: &CloudLinkSessionBinding,
    ) -> Result<(), CloudLinkSpoolError> {
        let identity = identity.clone();
        let session = session.clone();
        self.mutate(move |state| {
            state.mark_offered(&identity, &session)?;
            Ok(((), JournalEntry::Offered { identity, session }))
        })
        .await
    }

    async fn mark_transport_published(
        &self,
        identity: &CloudLinkRecordIdentity,
        session: &CloudLinkSessionBinding,
    ) -> Result<(), CloudLinkSpoolError> {
        let identity = identity.clone();
        let session = session.clone();
        self.mutate(move |state| {
            state.mark_transport_published(&identity, &session)?;
            Ok(((), JournalEntry::TransportPublished { identity, session }))
        })
        .await
    }

    async fn acknowledge(
        &self,
        ack: &CloudLinkDurableAck,
    ) -> Result<DurableAckOutcome, CloudLinkSpoolError> {
        let ack = ack.clone();
        self.mutate(move |state| {
            let outcome = state.acknowledge(&ack)?;
            Ok((outcome, JournalEntry::Acknowledged { ack }))
        })
        .await
    }

    async fn status(&self) -> Result<CloudLinkSpoolStatus, CloudLinkSpoolError> {
        self.run_blocking(move |path, guard| {
            ensure_healthy(guard)?;
            verify_journal_identity(path, guard)?;
            Ok(guard.state.status_with_journal(
                guard.journal_bytes,
                guard.max_journal_bytes,
                guard.physical_quota_rejections,
            ))
        })
        .await
    }

    async fn rotate_stream_epoch(&self) -> Result<u64, CloudLinkSpoolError> {
        self.mutate(move |state| {
            let stream_epoch = state.rotate_stream_epoch()?;
            Ok((stream_epoch, JournalEntry::Rotated { stream_epoch }))
        })
        .await
    }
}

fn write_compacted_journal(
    path: &Path,
    file: &mut File,
    state: &CloudLinkSpoolState,
) -> Result<(), CloudLinkSpoolError> {
    file.set_len(0)
        .map_err(|source| storage_error(path, "truncate compaction journal", source))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|source| storage_error(path, "seek compaction journal", source))?;
    file.write_all(MAGIC)
        .map_err(|source| storage_error(path, "write header", source))?;
    write_entry(file, &checkpoint(state))?;
    for receipt in state.acknowledged_receipts.values() {
        write_entry(
            file,
            &JournalEntry::Receipt {
                receipt: receipt.clone(),
            },
        )?;
    }
    for record in state.records.values() {
        let data_loss_report = state
            .data_loss_reports
            .get(&record.identity().position())
            .cloned();
        write_entry(
            file,
            &JournalEntry::Record {
                record: record.clone(),
                data_loss_report,
            },
        )?;
    }
    file.sync_all()
        .map_err(|source| storage_error(path, "sync compacted journal", source))
}

fn checkpoint(state: &CloudLinkSpoolState) -> JournalEntry {
    JournalEntry::Checkpoint {
        stream_id: state.stream_id.clone(),
        stream_epoch: state.stream_epoch,
        next_position: state.next_position,
        capacity: state.capacity,
        acknowledged_receipt_capacity: state.acknowledged_receipt_capacity,
        max_live_bytes: state.max_live_bytes,
        last_ack: state.last_ack.clone(),
        last_acknowledged_position: state.last_acknowledged_position,
        data_loss: state.data_loss.clone(),
    }
}

fn persist_mutation(
    path: &Path,
    guard: &mut FileState,
    next: &CloudLinkSpoolState,
    entry: &JournalEntry,
) -> Result<(), CloudLinkSpoolError> {
    let payload = encode_entry(entry)?;
    let append_bytes = encoded_entry_bytes(payload.len())?;
    let projected = guard
        .journal_bytes
        .checked_add(append_bytes)
        .ok_or_else(|| corrupt(path, "physical journal accounting overflow"))?;
    let compacted_upper_bound = compacted_journal_upper_bound(next)?;
    let reclaimable = guard.journal_bytes.saturating_sub(compacted_upper_bound);
    let ratio_compaction = reclaimable >= MIN_COMPACTION_RECLAIM_BYTES
        && guard.journal_bytes > compacted_upper_bound.saturating_mul(2);
    if projected > guard.max_journal_bytes || ratio_compaction {
        compact_locked_to(path, guard, next)?;
        return Ok(());
    }
    append_payload(path, guard, &payload, append_bytes)
}

fn compacted_journal_upper_bound(state: &CloudLinkSpoolState) -> Result<u64, CloudLinkSpoolError> {
    let checkpoint_bytes = encoded_entry_bytes(encode_entry(&checkpoint(state))?.len())?;
    (MAGIC.len() as u64)
        .checked_add(checkpoint_bytes)
        .and_then(|bytes| bytes.checked_add(state.current_live_bytes))
        .ok_or_else(|| {
            error(
                CloudLinkSpoolErrorReason::Storage,
                "CloudLink compacted journal accounting overflow",
            )
        })
}

fn compact_locked_to(
    path: &Path,
    guard: &mut FileState,
    state: &CloudLinkSpoolState,
) -> Result<(), CloudLinkSpoolError> {
    compact_locked_to_with_parent_sync(path, guard, state, sync_parent_directory)
}

fn compact_locked_to_with_parent_sync<SyncParent>(
    path: &Path,
    guard: &mut FileState,
    state: &CloudLinkSpoolState,
    sync_parent: SyncParent,
) -> Result<(), CloudLinkSpoolError>
where
    SyncParent: FnOnce(&Path) -> Result<(), CloudLinkSpoolError>,
{
    ensure_healthy(guard)?;
    verify_journal_identity(path, guard)?;
    let temp_path = compaction_path(path);
    let mut temp = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&temp_path)
        .map_err(|source| storage_error(&temp_path, "create compaction journal", source))?;
    if let Err(error) = write_compacted_journal(&temp_path, &mut temp, state) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(error);
    }
    let compacted_bytes = temp
        .metadata()
        .map_err(|source| storage_error(&temp_path, "stat compaction journal", source))?
        .len();
    if compacted_bytes > guard.max_journal_bytes {
        guard.physical_quota_rejections = guard.physical_quota_rejections.saturating_add(1);
        let _ = std::fs::remove_file(&temp_path);
        return Err(error(
            CloudLinkSpoolErrorReason::CapacityExceeded,
            format!(
                "CloudLink compacted journal requires {compacted_bytes} bytes, exceeding physical limit {}",
                guard.max_journal_bytes
            ),
        ));
    }
    if let Err(source) = std::fs::rename(&temp_path, path) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(storage_error(path, "commit compacted journal", source));
    }
    let replacement_identity = match temp.metadata() {
        Ok(metadata) => JournalFileIdentity::from_metadata(&metadata),
        Err(source) => {
            return poison_after_compaction_commit(
                guard,
                storage_error(path, "stat compacted journal", source),
            );
        },
    };
    if let Err(source) = temp.seek(SeekFrom::End(0)) {
        return poison_after_compaction_commit(
            guard,
            storage_error(path, "seek compacted journal", source),
        );
    }
    if let Err(error) = sync_parent(path) {
        return poison_after_compaction_commit(guard, error);
    }
    guard.file = temp;
    guard.file_identity = replacement_identity;
    guard.journal_bytes = compacted_bytes;
    if let Err(error) = verify_journal_identity(path, guard) {
        return poison_after_compaction_commit(guard, error);
    }
    Ok(())
}

fn compaction_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_else(|| "cloudlink-spool".into());
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    path.with_file_name(format!(
        ".{name}.compact.{}.{}.tmp",
        std::process::id(),
        sequence
    ))
}

fn sync_parent_directory(path: &Path) -> Result<(), CloudLinkSpoolError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| storage_error(parent, "sync parent directory", source))
}

fn append_payload(
    path: &Path,
    guard: &mut FileState,
    payload: &[u8],
    append_bytes: u64,
) -> Result<(), CloudLinkSpoolError> {
    append_payload_with_hooks(
        path,
        guard,
        payload,
        append_bytes,
        File::sync_data,
        rollback_append,
    )
}

fn append_payload_with_hooks<SyncRecord, Rollback>(
    path: &Path,
    guard: &mut FileState,
    payload: &[u8],
    append_bytes: u64,
    sync_record: SyncRecord,
    rollback: Rollback,
) -> Result<(), CloudLinkSpoolError>
where
    SyncRecord: FnOnce(&File) -> std::io::Result<()>,
    Rollback: FnOnce(&mut File, u64) -> std::io::Result<()>,
{
    let start = guard.journal_bytes;
    let result = guard
        .file
        .seek(SeekFrom::End(0))
        .map_err(|source| storage_error(path, "seek append position", source))
        .and_then(|actual| {
            if actual == start {
                Ok(())
            } else {
                Err(corrupt(
                    path,
                    "journal append position changed outside its owner",
                ))
            }
        })
        .and_then(|()| write_payload(&mut guard.file, payload))
        .and_then(|()| {
            sync_record(&guard.file)
                .map_err(|source| storage_error(path, "sync journal mutation", source))
        });
    if let Err(original) = result {
        if let Err(rollback_error) = rollback(&mut guard.file, start) {
            guard.poisoned = Some(format!(
                "append failed ({original}) and rollback failed ({rollback_error})"
            ));
            return Err(error(
                CloudLinkSpoolErrorReason::CorruptJournal,
                "CloudLink journal append failed and could not be rolled back; spool poisoned",
            ));
        }
        if original.reason() == Some(CloudLinkSpoolErrorReason::CorruptJournal) {
            guard.poisoned = Some(format!(
                "journal ownership changed during append: {original}"
            ));
            return Err(original);
        }
        verify_journal_identity(path, guard)?;
        return Err(original);
    }
    guard.journal_bytes = start
        .checked_add(append_bytes)
        .ok_or_else(|| corrupt(path, "physical journal accounting overflow"))?;
    verify_journal_identity(path, guard)
}

fn rollback_append(file: &mut File, start: u64) -> std::io::Result<()> {
    file.set_len(start)?;
    file.seek(SeekFrom::End(0))?;
    file.sync_data()
}

fn write_entry(file: &mut File, entry: &JournalEntry) -> Result<(), CloudLinkSpoolError> {
    let payload = encode_entry(entry)?;
    write_payload(file, &payload)
}

fn encode_entry(entry: &JournalEntry) -> Result<Vec<u8>, CloudLinkSpoolError> {
    let payload = serde_json::to_vec(entry).map_err(|source| {
        error(
            CloudLinkSpoolErrorReason::Storage,
            format!("cannot encode CloudLink spool journal mutation: {source}"),
        )
    })?;
    if payload.is_empty() || payload.len() > MAX_JOURNAL_RECORD_BYTES {
        return Err(error(
            CloudLinkSpoolErrorReason::Storage,
            "CloudLink spool journal mutation exceeds the 8 MiB safety bound",
        ));
    }
    Ok(payload)
}

fn encoded_entry_bytes(payload_len: usize) -> Result<u64, CloudLinkSpoolError> {
    u64::try_from(payload_len)
        .ok()
        .and_then(|bytes| bytes.checked_add(8))
        .ok_or_else(|| {
            error(
                CloudLinkSpoolErrorReason::Storage,
                "CloudLink encoded journal mutation size overflow",
            )
        })
}

fn write_payload(file: &mut File, payload: &[u8]) -> Result<(), CloudLinkSpoolError> {
    let length = u32::try_from(payload.len()).map_err(|_| {
        error(
            CloudLinkSpoolErrorReason::Storage,
            "CloudLink spool journal mutation length exceeds uint32",
        )
    })?;
    file.write_all(&length.to_le_bytes())
        .and_then(|()| file.write_all(payload))
        .and_then(|()| file.write_all(&crc32(payload).to_le_bytes()))
        .map_err(|source| {
            error(
                CloudLinkSpoolErrorReason::Storage,
                format!("cannot write CloudLink spool journal mutation: {source}"),
            )
        })
}

fn ensure_healthy(guard: &FileState) -> Result<(), CloudLinkSpoolError> {
    if let Some(reason) = &guard.poisoned {
        return Err(error(
            CloudLinkSpoolErrorReason::CorruptJournal,
            format!("CloudLink file spool is poisoned after a fatal journal error: {reason}"),
        ));
    }
    Ok(())
}

fn poison_after_compaction_commit<T>(
    guard: &mut FileState,
    failure: CloudLinkSpoolError,
) -> Result<T, CloudLinkSpoolError> {
    guard.poisoned = Some(format!(
        "compaction rename committed but replacement durability was not confirmed: {failure}"
    ));
    Err(failure)
}

fn verify_journal_identity(path: &Path, guard: &mut FileState) -> Result<(), CloudLinkSpoolError> {
    ensure_healthy(guard)?;
    let lock_active = guard._lock_file.metadata().map_err(|source| {
        guard.poisoned = Some(format!("stable lock file cannot be inspected: {source}"));
        storage_error(&guard.lock_path, "inspect active stable lock", source)
    })?;
    let lock_canonical = std::fs::metadata(&guard.lock_path).map_err(|source| {
        guard.poisoned = Some(format!("stable lock path cannot be inspected: {source}"));
        storage_error(&guard.lock_path, "inspect canonical stable lock", source)
    })?;
    if JournalFileIdentity::from_metadata(&lock_active) != guard.lock_identity
        || JournalFileIdentity::from_metadata(&lock_canonical) != guard.lock_identity
    {
        guard.poisoned = Some("stable lock path was replaced outside its owner".to_owned());
        return Err(error(
            CloudLinkSpoolErrorReason::CorruptJournal,
            "CloudLink stable lock path was replaced outside its owner",
        ));
    }
    let active = guard.file.metadata().map_err(|source| {
        guard.poisoned = Some(format!("active journal cannot be inspected: {source}"));
        storage_error(path, "inspect active journal", source)
    })?;
    let canonical = std::fs::metadata(path).map_err(|source| {
        guard.poisoned = Some(format!("canonical journal cannot be inspected: {source}"));
        storage_error(path, "inspect canonical journal", source)
    })?;
    let active_identity = JournalFileIdentity::from_metadata(&active);
    let canonical_identity = JournalFileIdentity::from_metadata(&canonical);
    if active_identity != guard.file_identity || canonical_identity != guard.file_identity {
        guard.poisoned =
            Some("canonical journal path no longer identifies the active journal file".to_owned());
        return Err(error(
            CloudLinkSpoolErrorReason::CorruptJournal,
            "CloudLink journal path was replaced outside its owner",
        ));
    }
    if active.len() != guard.journal_bytes {
        guard.poisoned = Some(format!(
            "journal changed outside its owner: expected {} bytes, found {}",
            guard.journal_bytes,
            active.len()
        ));
        return Err(error(
            CloudLinkSpoolErrorReason::CorruptJournal,
            "CloudLink journal length changed outside its owner",
        ));
    }
    Ok(())
}

fn sibling_path(path: &Path, suffix: &str) -> PathBuf {
    let mut file_name = path
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("cloudlink-spool"))
        .to_os_string();
    file_name.push(suffix);
    path.with_file_name(file_name)
}

fn recover(
    path: &Path,
    file: &mut File,
) -> Result<(CloudLinkSpoolState, usize), CloudLinkSpoolError> {
    let file_len = file
        .metadata()
        .map_err(|source| storage_error(path, "read journal metadata", source))?
        .len();
    file.seek(SeekFrom::Start(0))
        .map_err(|source| storage_error(path, "seek journal start", source))?;
    let mut magic = [0_u8; MAGIC.len()];
    file.read_exact(&mut magic)
        .map_err(|source| storage_error(path, "read journal header", source))?;
    if &magic != MAGIC {
        return Err(corrupt(path, "invalid CloudLink spool journal header"));
    }

    let mut offset = MAGIC.len() as u64;
    let mut state = None;
    let mut mutations = 0_usize;
    let mut mutations_started = false;
    while offset < file_len {
        let record_start = offset;
        if file_len - offset < 4 {
            truncate_torn_tail(path, file, record_start)?;
            break;
        }

        let mut length_bytes = [0_u8; 4];
        file.read_exact(&mut length_bytes)
            .map_err(|source| storage_error(path, "read mutation length", source))?;
        let length = u32::from_le_bytes(length_bytes) as usize;
        offset += 4;
        if length == 0 || length > MAX_JOURNAL_RECORD_BYTES {
            return Err(corrupt(path, "journal mutation exceeds safety bound"));
        }
        let required = u64::try_from(length)
            .ok()
            .and_then(|value| value.checked_add(4))
            .ok_or_else(|| corrupt(path, "journal mutation length overflow"))?;
        if file_len - offset < required {
            truncate_torn_tail(path, file, record_start)?;
            break;
        }

        let mut payload = vec![0_u8; length];
        file.read_exact(&mut payload)
            .map_err(|source| storage_error(path, "read mutation payload", source))?;
        let mut crc_bytes = [0_u8; 4];
        file.read_exact(&mut crc_bytes)
            .map_err(|source| storage_error(path, "read mutation checksum", source))?;
        offset += required;
        if crc32(&payload) != u32::from_le_bytes(crc_bytes) {
            return Err(corrupt(path, "journal mutation checksum mismatch"));
        }
        let entry: JournalEntry = serde_json::from_slice(&payload).map_err(|source| {
            corrupt(
                path,
                &format!("invalid CloudLink journal mutation: {source}"),
            )
        })?;
        apply_entry(
            path,
            entry,
            &mut state,
            &mut mutations,
            &mut mutations_started,
        )?;
    }

    let state = state.ok_or_else(|| corrupt(path, "journal contains no checkpoint"))?;
    validate_recovered_state(path, &state)?;
    Ok((state, mutations))
}

fn apply_entry(
    path: &Path,
    entry: JournalEntry,
    state: &mut Option<CloudLinkSpoolState>,
    mutations: &mut usize,
    mutations_started: &mut bool,
) -> Result<(), CloudLinkSpoolError> {
    match entry {
        JournalEntry::Checkpoint {
            stream_id,
            stream_epoch,
            next_position,
            capacity,
            acknowledged_receipt_capacity,
            max_live_bytes,
            last_ack,
            last_acknowledged_position,
            data_loss,
        } => {
            if state.is_some() {
                return Err(corrupt(path, "journal contains more than one checkpoint"));
            }
            let mut recovered = CloudLinkSpoolState::new_with_limits(
                stream_id,
                capacity,
                acknowledged_receipt_capacity,
                max_live_bytes,
            )
            .map_err(|error| corrupt(path, &error.to_string()))?;
            recovered.stream_epoch = stream_epoch;
            recovered.next_position = next_position;
            recovered.last_ack = last_ack;
            recovered.last_acknowledged_position = last_acknowledged_position;
            recovered.data_loss = data_loss;
            *state = Some(recovered);
        },
        JournalEntry::Receipt { receipt } => {
            if *mutations_started {
                return Err(corrupt(path, "checkpoint receipt appears after a mutation"));
            }
            restore_checkpoint_receipt(path, active_state(path, state)?, receipt)?;
        },
        JournalEntry::Record {
            record,
            data_loss_report,
        } => {
            if *mutations_started {
                return Err(corrupt(path, "checkpoint record appears after a mutation"));
            }
            restore_checkpoint_record(path, active_state(path, state)?, record, data_loss_report)?;
        },
        JournalEntry::Enqueued {
            record,
            next_position,
            data_loss,
            data_loss_report,
        } => {
            *mutations_started = true;
            apply_enqueued(
                path,
                active_state(path, state)?,
                record,
                next_position,
                data_loss,
                data_loss_report,
            )?;
            *mutations = mutations.saturating_add(1);
        },
        JournalEntry::Offered { identity, session } => {
            *mutations_started = true;
            active_state(path, state)?
                .mark_offered(&identity, &session)
                .map_err(|error| corrupt(path, &error.to_string()))?;
            *mutations = mutations.saturating_add(1);
        },
        JournalEntry::TransportPublished { identity, session } => {
            *mutations_started = true;
            active_state(path, state)?
                .mark_transport_published(&identity, &session)
                .map_err(|error| corrupt(path, &error.to_string()))?;
            *mutations = mutations.saturating_add(1);
        },
        JournalEntry::Acknowledged { ack } => {
            *mutations_started = true;
            active_state(path, state)?
                .acknowledge(&ack)
                .map_err(|error| corrupt(path, &error.to_string()))?;
            *mutations = mutations.saturating_add(1);
        },
        JournalEntry::Rotated { stream_epoch } => {
            *mutations_started = true;
            let recovered_epoch = active_state(path, state)?
                .rotate_stream_epoch()
                .map_err(|error| corrupt(path, &error.to_string()))?;
            if recovered_epoch != stream_epoch {
                return Err(corrupt(path, "stream epoch mutation is inconsistent"));
            }
            *mutations = mutations.saturating_add(1);
        },
        JournalEntry::Capacity {
            capacity,
            acknowledged_receipt_capacity,
            max_live_bytes,
        } => {
            *mutations_started = true;
            let recovered = active_state(path, state)?;
            let stream_id = recovered.stream_id.clone();
            recovered
                .validate_open(
                    &stream_id,
                    capacity,
                    acknowledged_receipt_capacity,
                    max_live_bytes,
                )
                .map_err(|error| corrupt(path, &error.to_string()))?;
            *mutations = mutations.saturating_add(1);
        },
    }
    Ok(())
}

fn active_state<'a>(
    path: &Path,
    state: &'a mut Option<CloudLinkSpoolState>,
) -> Result<&'a mut CloudLinkSpoolState, CloudLinkSpoolError> {
    state
        .as_mut()
        .ok_or_else(|| corrupt(path, "journal mutation precedes checkpoint"))
}

fn restore_checkpoint_record(
    path: &Path,
    state: &mut CloudLinkSpoolState,
    record: CloudLinkRecord,
    data_loss_report: Option<CloudLinkDataLossEvidence>,
) -> Result<(), CloudLinkSpoolError> {
    validate_persisted_record(path, state, &record)?;
    let position = record.identity().position();
    if position >= state.next_position || state.records.contains_key(&position) {
        return Err(corrupt(path, "checkpoint record position is inconsistent"));
    }
    if let Some(evidence) = data_loss_report.as_ref() {
        validate_persisted_data_loss_report(path, state, &record, evidence)?;
        if !state.data_loss_reports.is_empty() {
            return Err(corrupt(
                path,
                "checkpoint contains more than one pending data-loss report",
            ));
        }
    }
    let live_bytes = record_live_bytes(&record, data_loss_report.as_ref())
        .map_err(|error| corrupt(path, &error.to_string()))?;
    state.current_live_bytes = state
        .current_live_bytes
        .checked_add(live_bytes)
        .ok_or_else(|| corrupt(path, "checkpoint live-byte accounting overflow"))?;
    state.records.insert(position, record);
    if let Some(evidence) = data_loss_report {
        state.data_loss_reports.insert(position, evidence);
    }
    let ordinary_records = state
        .records
        .len()
        .saturating_sub(state.data_loss_reports.len());
    if ordinary_records > state.capacity || state.data_loss_reports.len() > 1 {
        return Err(corrupt(path, "checkpoint exceeds configured capacity"));
    }
    Ok(())
}

fn restore_checkpoint_receipt(
    path: &Path,
    state: &mut CloudLinkSpoolState,
    receipt: CloudLinkAcknowledgedReceipt,
) -> Result<(), CloudLinkSpoolError> {
    let batch_id = receipt.batch_id.clone();
    if state.acknowledged_receipts.contains_key(&batch_id) {
        return Err(corrupt(
            path,
            "checkpoint contains a duplicate receipt identity",
        ));
    }
    let live_bytes =
        receipt_live_bytes(&receipt).map_err(|error| corrupt(path, &error.to_string()))?;
    state.current_live_bytes = state
        .current_live_bytes
        .checked_add(live_bytes)
        .ok_or_else(|| corrupt(path, "checkpoint receipt byte accounting overflow"))?;
    state.acknowledged_receipts.insert(batch_id, receipt);
    if state.acknowledged_receipts.len() > state.acknowledged_receipt_capacity {
        return Err(corrupt(
            path,
            "checkpoint exceeds configured acknowledged-receipt capacity",
        ));
    }
    Ok(())
}

fn apply_enqueued(
    path: &Path,
    state: &mut CloudLinkSpoolState,
    record: CloudLinkRecord,
    next_position: u64,
    data_loss: Option<CloudLinkDataLossEvidence>,
    data_loss_report: Option<CloudLinkDataLossEvidence>,
) -> Result<(), CloudLinkSpoolError> {
    validate_persisted_record(path, state, &record)?;
    if record.state() != CloudLinkDeliveryState::Queued
        || record.offered_session().is_some()
        || record.identity().position() != state.next_position
        || next_position != state.next_position.checked_add(1).unwrap_or(0)
    {
        return Err(corrupt(
            path,
            "enqueue mutation position or state is inconsistent",
        ));
    }
    let input = CloudLinkEnqueue::new(
        record.message_kind(),
        record.batch_id(),
        record.digest(),
        record.payload().to_vec(),
        record.created_at(),
        record.expires_at(),
    );
    let recovered = match data_loss_report.as_ref() {
        Some(evidence) => state
            .admit_data_loss(input, evidence)
            .map(|admission| admission.pending_record().cloned()),
        None if record.is_lossless_admission() => state
            .admit_lossless(
                input,
                if record.retains_acknowledged_receipt() {
                    CloudLinkReceiptRetention::RetainForIdempotency
                } else {
                    CloudLinkReceiptRetention::DiscardAfterAck
                },
            )
            .map(|admission| admission.pending_record().cloned()),
        None => state.enqueue(input).map(Some),
    }
    .map_err(|error| corrupt(path, &error.to_string()))?
    .ok_or_else(|| corrupt(path, "enqueue mutation resolved to an acknowledged receipt"))?;
    if recovered != record || state.next_position != next_position || state.data_loss != data_loss {
        return Err(corrupt(path, "enqueue mutation state is inconsistent"));
    }
    Ok(())
}

fn validate_persisted_data_loss_report(
    path: &Path,
    state: &CloudLinkSpoolState,
    record: &CloudLinkRecord,
    evidence: &CloudLinkDataLossEvidence,
) -> Result<(), CloudLinkSpoolError> {
    if record.message_kind() != aether_ports::CloudLinkMessageKind::DataLoss
        || !record.is_lossless_admission()
        || record.retains_acknowledged_receipt()
        || evidence.stream_id() != state.stream_id
        || evidence.stream_epoch() != state.stream_epoch
        || evidence.first_lost_position() == 0
        || evidence.first_lost_position() > evidence.last_lost_position()
        || evidence.earliest_retained_position() <= evidence.last_lost_position()
    {
        return Err(corrupt(
            path,
            "persisted data-loss report association is invalid",
        ));
    }
    Ok(())
}

fn validate_persisted_record(
    path: &Path,
    state: &CloudLinkSpoolState,
    record: &CloudLinkRecord,
) -> Result<(), CloudLinkSpoolError> {
    let identity = record.identity();
    let digest = record.digest();
    let valid = identity.stream_id() == state.stream_id
        && identity.stream_epoch() == state.stream_epoch
        && identity.position() > 0
        && !record.batch_id().is_empty()
        && record.batch_id().len() <= 128
        && digest.len() == 71
        && digest.starts_with("sha256:")
        && digest[7..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        && !record.payload().is_empty()
        && record.payload().len() <= MAX_SPOOL_PAYLOAD_BYTES
        && (!record.retains_acknowledged_receipt() || record.is_lossless_admission())
        && record
            .expires_at()
            .is_none_or(|expiry| expiry.get() > record.created_at().get());
    if valid {
        Ok(())
    } else {
        Err(corrupt(path, "persisted CloudLink record is invalid"))
    }
}

fn validate_recovered_state(
    path: &Path,
    state: &CloudLinkSpoolState,
) -> Result<(), CloudLinkSpoolError> {
    let ordinary_records = state
        .records
        .len()
        .saturating_sub(state.data_loss_reports.len());
    if state.stream_epoch == 0
        || state.next_position == 0
        || state.capacity == 0
        || ordinary_records > state.capacity
        || state.data_loss_reports.len() > 1
        || state.last_acknowledged_position >= state.next_position
        || state.acknowledged_receipt_capacity == 0
        || state.acknowledged_receipt_capacity > MAX_ACKNOWLEDGED_RECEIPT_CAPACITY
        || state.protected_receipt_slots() > state.acknowledged_receipt_capacity
    {
        return Err(corrupt(
            path,
            "recovered CloudLink cursor metadata is invalid",
        ));
    }
    let mut receipt_batches = std::collections::HashSet::new();
    for (batch_id, receipt) in &state.acknowledged_receipts {
        let identity = &receipt.identity;
        let digest = &receipt.digest;
        if identity.stream_id() != state.stream_id
            || identity.stream_epoch() == 0
            || identity.stream_epoch() > state.stream_epoch
            || identity.position() == 0
            || (identity.stream_epoch() == state.stream_epoch
                && identity.position() > state.last_acknowledged_position)
            || batch_id != &receipt.batch_id
            || !receipt_batches.insert(receipt.batch_id.as_str())
            || !valid_persisted_identifier(&receipt.batch_id, 128)
            || digest.len() != 71
            || !digest.starts_with("sha256:")
            || !digest[7..]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(corrupt(
                path,
                "recovered acknowledged receipt ledger is invalid",
            ));
        }
    }
    if state
        .records
        .values()
        .any(|record| receipt_batches.contains(record.batch_id()))
    {
        return Err(corrupt(
            path,
            "pending record conflicts with an acknowledged receipt",
        ));
    }
    for (position, evidence) in &state.data_loss_reports {
        let Some(record) = state.records.get(position) else {
            return Err(corrupt(
                path,
                "data-loss report association has no retained record",
            ));
        };
        validate_persisted_data_loss_report(path, state, record, evidence)?;
    }
    match &state.last_ack {
        Some(ack)
            if ack.stream_id() == state.stream_id
                && ack.stream_epoch() == state.stream_epoch
                && ack.acknowledged_position() == state.last_acknowledged_position => {},
        None if state.last_acknowledged_position == 0 => {},
        _ => {
            return Err(corrupt(
                path,
                "recovered durable ACK metadata is inconsistent",
            ));
        },
    }
    if state.records.keys().any(|position| {
        *position <= state.last_acknowledged_position || *position >= state.next_position
    }) {
        return Err(corrupt(path, "recovered record range is inconsistent"));
    }
    if let Some(loss) = &state.data_loss
        && (loss.stream_id() != state.stream_id
            || loss.stream_epoch() != state.stream_epoch
            || loss.first_lost_position() == 0
            || loss.first_lost_position() > loss.last_lost_position()
            || loss.earliest_retained_position() <= loss.last_lost_position())
    {
        return Err(corrupt(
            path,
            "recovered data-loss evidence is inconsistent",
        ));
    }
    let records_live_bytes =
        state
            .records
            .iter()
            .try_fold(0_u64, |total, (position, record)| {
                record_live_bytes(record, state.data_loss_reports.get(position))
                    .map_err(|error| corrupt(path, &error.to_string()))
                    .and_then(|bytes| {
                        total.checked_add(bytes).ok_or_else(|| {
                            corrupt(path, "recovered record byte accounting overflow")
                        })
                    })
            })?;
    let receipts_live_bytes =
        state
            .acknowledged_receipts
            .values()
            .try_fold(0_u64, |total, receipt| {
                receipt_live_bytes(receipt)
                    .map_err(|error| corrupt(path, &error.to_string()))
                    .and_then(|bytes| {
                        total.checked_add(bytes).ok_or_else(|| {
                            corrupt(path, "recovered receipt byte accounting overflow")
                        })
                    })
            })?;
    let expected_live_bytes = records_live_bytes
        .checked_add(receipts_live_bytes)
        .ok_or_else(|| corrupt(path, "recovered live-byte accounting overflow"))?;
    if state.current_live_bytes != expected_live_bytes
        || state.current_live_bytes > state.max_live_bytes
    {
        return Err(corrupt(
            path,
            "recovered CloudLink live-byte accounting is inconsistent",
        ));
    }
    let (ordinary_live_bytes, system_live_bytes) = state
        .live_byte_partitions()
        .map_err(|error| corrupt(path, &error.to_string()))?;
    if ordinary_live_bytes
        > state
            .max_live_bytes
            .saturating_sub(crate::cloudlink_spool::CLOUDLINK_DATA_LOSS_RESERVED_LIVE_BYTES)
        || system_live_bytes > crate::cloudlink_spool::CLOUDLINK_DATA_LOSS_RESERVED_LIVE_BYTES
    {
        return Err(corrupt(
            path,
            "recovered CloudLink live-byte partitions consume the data-loss reserve",
        ));
    }
    Ok(())
}

fn valid_persisted_identifier(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

fn truncate_torn_tail(
    path: &Path,
    file: &mut File,
    offset: u64,
) -> Result<(), CloudLinkSpoolError> {
    file.set_len(offset)
        .map_err(|source| storage_error(path, "truncate torn journal tail", source))?;
    file.sync_all()
        .map_err(|source| storage_error(path, "sync repaired journal", source))
}

fn crc32(bytes: &[u8]) -> u32 {
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

fn storage_error(path: &Path, action: &str, source: std::io::Error) -> CloudLinkSpoolError {
    error(
        CloudLinkSpoolErrorReason::Storage,
        format!(
            "cannot {action} CloudLink spool journal {}: {source}",
            path.display()
        ),
    )
}

fn corrupt(path: &Path, message: &str) -> CloudLinkSpoolError {
    error(
        CloudLinkSpoolErrorReason::CorruptJournal,
        format!(
            "corrupt CloudLink spool journal {}: {message}",
            path.display()
        ),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use aether_domain::TimestampMs;
    use aether_ports::{CloudLinkMessageKind, CloudLinkReceiptRetention, CloudLinkSpool as _};

    use super::*;

    fn injected_io(message: &'static str) -> std::io::Error {
        std::io::Error::other(message)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn waiting_for_the_file_mutex_never_blocks_the_tokio_worker() {
        let root = tempfile::tempdir().expect("temp dir");
        let spool = Arc::new(
            FileCloudLinkSpool::open(root.path().join("slow.spool"), "business", 8).expect("spool"),
        );
        let inner = Arc::clone(&spool.inner);
        let (locked_tx, locked_rx) = std::sync::mpsc::channel();
        let blocker = std::thread::spawn(move || {
            let _guard = inner.lock().expect("file state lock");
            locked_tx.send(()).expect("announce lock");
            std::thread::sleep(Duration::from_millis(200));
        });
        locked_rx.recv().expect("file state locked");

        let waiting_spool = Arc::clone(&spool);
        let waiting = tokio::spawn(async move { waiting_spool.status().await });
        tokio::task::yield_now().await;
        let started = Instant::now();
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "a file mutex waiter blocked the only Tokio worker"
        );
        blocker.join().expect("blocker thread");
        waiting.await.expect("status task").expect("status");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelling_the_waiter_does_not_cancel_an_accepted_blocking_commit() {
        let root = tempfile::tempdir().expect("temp dir");
        let spool = Arc::new(
            FileCloudLinkSpool::open(root.path().join("cancelled.spool"), "business", 8)
                .expect("spool"),
        );
        let inner = Arc::clone(&spool.inner);
        let (locked_tx, locked_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let blocker = std::thread::spawn(move || {
            let _guard = inner.lock().expect("file state lock");
            locked_tx.send(()).expect("announce lock");
            release_rx.recv().expect("release lock");
        });
        locked_rx.recv().expect("file state locked");

        let input = CloudLinkEnqueue::new(
            CloudLinkMessageKind::AlarmEvent,
            "cancelled-admission",
            format!("sha256:{}", "a".repeat(64)),
            br#"{"alarm":true}"#.to_vec(),
            TimestampMs::new(1),
            None,
        );
        let retry = input.clone();
        let waiting_spool = Arc::clone(&spool);
        let waiting = tokio::spawn(async move {
            waiting_spool
                .admit_lossless(input, CloudLinkReceiptRetention::RetainForIdempotency)
                .await
        });

        tokio::time::timeout(Duration::from_secs(1), async {
            while spool.operation_gate.available_permits() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("blocking commit accepted operation-gate permit");
        waiting.abort();
        assert!(
            waiting.await.expect_err("cancelled waiter").is_cancelled(),
            "HTTP-style request cancellation must cancel only its waiter"
        );
        release_tx.send(()).expect("release file state");
        blocker.join().expect("blocker thread");

        let status = tokio::time::timeout(Duration::from_secs(2), spool.status())
            .await
            .expect("background commit completed")
            .expect("spool status");
        assert_eq!(status.pending_records(), 1);

        let duplicate = spool
            .admit_lossless(retry, CloudLinkReceiptRetention::RetainForIdempotency)
            .await
            .expect("retry admission");
        assert!(duplicate.duplicate());
        assert_eq!(duplicate.identity().position(), 1);
        assert_eq!(spool.status().await.expect("status").pending_records(), 1);
    }

    #[test]
    fn failed_append_rolls_back_or_permanently_poisoned_the_owner() {
        let root = tempfile::tempdir().expect("temp dir");
        let path = root.path().join("append.spool");
        let spool = FileCloudLinkSpool::open(&path, "business", 8).expect("spool");
        let mut guard = spool.lock().expect("state");
        let payload = encode_entry(&JournalEntry::Capacity {
            capacity: guard.state.capacity,
            acknowledged_receipt_capacity: guard.state.acknowledged_receipt_capacity,
            max_live_bytes: guard.state.max_live_bytes,
        })
        .expect("entry");
        let bytes = encoded_entry_bytes(payload.len()).expect("entry bytes");
        let original_length = guard.journal_bytes;

        append_payload_with_hooks(
            &path,
            &mut guard,
            &payload,
            bytes,
            |_| Err(injected_io("injected sync failure")),
            rollback_append,
        )
        .expect_err("sync failure");
        assert_eq!(guard.journal_bytes, original_length);
        assert_eq!(
            guard.file.metadata().expect("metadata").len(),
            original_length
        );
        assert!(guard.poisoned.is_none());

        append_payload_with_hooks(
            &path,
            &mut guard,
            &payload,
            bytes,
            |_| Err(injected_io("injected sync failure")),
            |_, _| Err(injected_io("injected rollback failure")),
        )
        .expect_err("rollback failure");
        assert!(guard.poisoned.is_some());
        assert!(ensure_healthy(&guard).is_err());
    }

    #[test]
    fn parent_sync_failure_after_compaction_commit_poisoned_until_reopen() {
        let root = tempfile::tempdir().expect("temp dir");
        let path = root.path().join("compact.spool");
        let spool = FileCloudLinkSpool::open(&path, "business", 8).expect("spool");
        {
            let mut guard = spool.lock().expect("state");
            let state = guard.state.clone();
            compact_locked_to_with_parent_sync(&path, &mut guard, &state, |_| {
                Err(storage_error(
                    &path,
                    "injected parent sync",
                    injected_io("injected parent sync failure"),
                ))
            })
            .expect_err("post-rename parent sync failure");
            assert!(guard.poisoned.is_some());
        }
        assert!(spool.current_status().is_err());
        drop(spool);
        FileCloudLinkSpool::open(&path, "business", 8)
            .expect("committed replacement remains recoverable");
    }
}
