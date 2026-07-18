//! Crash-recoverable file implementation of the dedicated CloudLink spool.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use aether_ports::{
    CloudLinkDataLossEvidence, CloudLinkDeliveryState, CloudLinkDurableAck, CloudLinkEnqueue,
    CloudLinkRecord, CloudLinkRecordIdentity, CloudLinkReplayWindow, CloudLinkSessionBinding,
    CloudLinkSpool, CloudLinkSpoolError, CloudLinkSpoolErrorReason, CloudLinkSpoolStatus,
    DurableAckOutcome,
};
use async_trait::async_trait;
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::cloudlink_spool::{CloudLinkSpoolState, MAX_SPOOL_PAYLOAD_BYTES, error};

const MAGIC: &[u8; 8] = b"AETHCL2\n";
const MAX_JOURNAL_RECORD_BYTES: usize = 2 * 1024 * 1024;
const COMPACT_AFTER_MUTATIONS: usize = 256;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "kebab-case")]
enum JournalEntry {
    Checkpoint {
        stream_id: String,
        stream_epoch: u64,
        next_position: u64,
        capacity: usize,
        last_ack: Option<CloudLinkDurableAck>,
        last_acknowledged_position: u64,
        data_loss: Option<CloudLinkDataLossEvidence>,
    },
    Record {
        record: CloudLinkRecord,
    },
    Enqueued {
        record: CloudLinkRecord,
        next_position: u64,
        data_loss: Option<CloudLinkDataLossEvidence>,
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
    },
}

struct FileState {
    file: File,
    state: CloudLinkSpoolState,
    mutations_since_compaction: usize,
}

/// Incremental-journal-backed CloudLink spool with exclusive process ownership.
pub struct FileCloudLinkSpool {
    path: PathBuf,
    inner: Mutex<FileState>,
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
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|source| storage_error(&path, "create parent directory", source))?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|source| storage_error(&path, "open", source))?;
        file.try_lock_exclusive()
            .map_err(|source| storage_error(&path, "acquire exclusive process lock", source))?;

        let length = file
            .metadata()
            .map_err(|source| storage_error(&path, "read metadata", source))?
            .len();
        let (mut state, mut mutations_since_compaction) = if length == 0 {
            let state = CloudLinkSpoolState::new(stream_id, capacity)?;
            write_compacted_journal(&path, &mut file, &state)?;
            (state, 0)
        } else {
            recover(&path, &mut file)?
        };

        if state.validate_open(stream_id, capacity)? {
            append_entry(&path, &mut file, &JournalEntry::Capacity { capacity })?;
            mutations_since_compaction = mutations_since_compaction.saturating_add(1);
        }
        file.seek(SeekFrom::End(0))
            .map_err(|source| storage_error(&path, "seek append position", source))?;

        let spool = Self {
            path,
            inner: Mutex::new(FileState {
                file,
                state,
                mutations_since_compaction,
            }),
        };
        if mutations_since_compaction >= COMPACT_AFTER_MUTATIONS {
            spool.compact()?;
        }
        Ok(spool)
    }

    /// Atomically rewrites the journal with only live records and cursor metadata.
    pub fn compact(&self) -> Result<(), CloudLinkSpoolError> {
        let mut guard = self.lock()?;
        compact_locked(&self.path, &mut guard)
    }

    /// Returns current stream metadata without requiring an async composition step.
    ///
    /// This is intended for fail-fast process composition before tasks are
    /// spawned. Runtime code should continue to use the port method.
    pub fn current_status(&self) -> Result<CloudLinkSpoolStatus, CloudLinkSpoolError> {
        Ok(self.lock()?.state.status())
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, FileState>, CloudLinkSpoolError> {
        self.inner.lock().map_err(|_| {
            error(
                CloudLinkSpoolErrorReason::Storage,
                "CloudLink file spool lock was poisoned",
            )
        })
    }

    fn mutate<T>(
        &self,
        operation: impl FnOnce(
            &mut CloudLinkSpoolState,
        ) -> Result<(T, JournalEntry), CloudLinkSpoolError>,
    ) -> Result<T, CloudLinkSpoolError> {
        let mut guard = self.lock()?;
        if guard.mutations_since_compaction >= COMPACT_AFTER_MUTATIONS {
            compact_locked(&self.path, &mut guard)?;
        }

        let mut next = guard.state.clone();
        let (result, entry) = operation(&mut next)?;
        if next != guard.state {
            append_entry(&self.path, &mut guard.file, &entry)?;
            guard.state = next;
            guard.mutations_since_compaction = guard.mutations_since_compaction.saturating_add(1);
        }
        Ok(result)
    }
}

#[async_trait]
impl CloudLinkSpool for FileCloudLinkSpool {
    async fn enqueue(
        &self,
        input: CloudLinkEnqueue,
    ) -> Result<CloudLinkRecord, CloudLinkSpoolError> {
        self.mutate(|state| {
            let record = state.enqueue(input)?;
            let entry = JournalEntry::Enqueued {
                record: record.clone(),
                next_position: state.next_position,
                data_loss: state.data_loss.clone(),
            };
            Ok((record, entry))
        })
    }

    async fn replay_from(
        &self,
        requested_position: u64,
        limit: usize,
    ) -> Result<CloudLinkReplayWindow, CloudLinkSpoolError> {
        self.lock()?.state.replay_from(requested_position, limit)
    }

    async fn mark_offered(
        &self,
        identity: &CloudLinkRecordIdentity,
        session: &CloudLinkSessionBinding,
    ) -> Result<(), CloudLinkSpoolError> {
        self.mutate(|state| {
            state.mark_offered(identity, session)?;
            Ok((
                (),
                JournalEntry::Offered {
                    identity: identity.clone(),
                    session: session.clone(),
                },
            ))
        })
    }

    async fn mark_transport_published(
        &self,
        identity: &CloudLinkRecordIdentity,
        session: &CloudLinkSessionBinding,
    ) -> Result<(), CloudLinkSpoolError> {
        self.mutate(|state| {
            state.mark_transport_published(identity, session)?;
            Ok((
                (),
                JournalEntry::TransportPublished {
                    identity: identity.clone(),
                    session: session.clone(),
                },
            ))
        })
    }

    async fn acknowledge(
        &self,
        ack: &CloudLinkDurableAck,
    ) -> Result<DurableAckOutcome, CloudLinkSpoolError> {
        self.mutate(|state| {
            let outcome = state.acknowledge(ack)?;
            Ok((outcome, JournalEntry::Acknowledged { ack: ack.clone() }))
        })
    }

    async fn status(&self) -> Result<CloudLinkSpoolStatus, CloudLinkSpoolError> {
        Ok(self.lock()?.state.status())
    }

    async fn rotate_stream_epoch(&self) -> Result<u64, CloudLinkSpoolError> {
        self.mutate(|state| {
            let stream_epoch = state.rotate_stream_epoch()?;
            Ok((stream_epoch, JournalEntry::Rotated { stream_epoch }))
        })
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
    for record in state.records.values() {
        write_entry(
            file,
            &JournalEntry::Record {
                record: record.clone(),
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
        last_ack: state.last_ack.clone(),
        last_acknowledged_position: state.last_acknowledged_position,
        data_loss: state.data_loss.clone(),
    }
}

fn compact_locked(path: &Path, guard: &mut FileState) -> Result<(), CloudLinkSpoolError> {
    let temp_path = compaction_path(path);
    let mut temp = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&temp_path)
        .map_err(|source| storage_error(&temp_path, "create compaction journal", source))?;
    temp.try_lock_exclusive()
        .map_err(|source| storage_error(&temp_path, "lock compaction journal", source))?;

    if let Err(error) = write_compacted_journal(&temp_path, &mut temp, &guard.state) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(error);
    }
    if let Err(source) = std::fs::rename(&temp_path, path) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(storage_error(path, "commit compacted journal", source));
    }
    temp.seek(SeekFrom::End(0))
        .map_err(|source| storage_error(path, "seek compacted journal", source))?;
    guard.file = temp;
    guard.mutations_since_compaction = 0;
    sync_parent_directory(path)
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
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| storage_error(parent, "sync parent directory", source))
}

fn append_entry(
    path: &Path,
    file: &mut File,
    entry: &JournalEntry,
) -> Result<(), CloudLinkSpoolError> {
    file.seek(SeekFrom::End(0))
        .map_err(|source| storage_error(path, "seek append position", source))?;
    write_entry(file, entry)?;
    file.sync_data()
        .map_err(|source| storage_error(path, "sync journal mutation", source))
}

fn write_entry(file: &mut File, entry: &JournalEntry) -> Result<(), CloudLinkSpoolError> {
    let payload = serde_json::to_vec(entry).map_err(|source| {
        error(
            CloudLinkSpoolErrorReason::Storage,
            format!("cannot encode CloudLink spool journal mutation: {source}"),
        )
    })?;
    if payload.is_empty() || payload.len() > MAX_JOURNAL_RECORD_BYTES {
        return Err(error(
            CloudLinkSpoolErrorReason::Storage,
            "CloudLink spool journal mutation exceeds the 2 MiB safety bound",
        ));
    }
    let length = u32::try_from(payload.len()).map_err(|_| {
        error(
            CloudLinkSpoolErrorReason::Storage,
            "CloudLink spool journal mutation length exceeds uint32",
        )
    })?;
    file.write_all(&length.to_le_bytes())
        .and_then(|()| file.write_all(&payload))
        .and_then(|()| file.write_all(&crc32(&payload).to_le_bytes()))
        .map_err(|source| {
            error(
                CloudLinkSpoolErrorReason::Storage,
                format!("cannot write CloudLink spool journal mutation: {source}"),
            )
        })
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
            last_ack,
            last_acknowledged_position,
            data_loss,
        } => {
            if state.is_some() {
                return Err(corrupt(path, "journal contains more than one checkpoint"));
            }
            let mut recovered = CloudLinkSpoolState::new(stream_id, capacity)
                .map_err(|error| corrupt(path, &error.to_string()))?;
            recovered.stream_epoch = stream_epoch;
            recovered.next_position = next_position;
            recovered.last_ack = last_ack;
            recovered.last_acknowledged_position = last_acknowledged_position;
            recovered.data_loss = data_loss;
            *state = Some(recovered);
        },
        JournalEntry::Record { record } => {
            if *mutations_started {
                return Err(corrupt(path, "checkpoint record appears after a mutation"));
            }
            restore_checkpoint_record(path, active_state(path, state)?, record)?;
        },
        JournalEntry::Enqueued {
            record,
            next_position,
            data_loss,
        } => {
            *mutations_started = true;
            apply_enqueued(
                path,
                active_state(path, state)?,
                record,
                next_position,
                data_loss,
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
        JournalEntry::Capacity { capacity } => {
            *mutations_started = true;
            let recovered = active_state(path, state)?;
            let stream_id = recovered.stream_id.clone();
            recovered
                .validate_open(&stream_id, capacity)
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
) -> Result<(), CloudLinkSpoolError> {
    validate_persisted_record(path, state, &record)?;
    let position = record.identity().position();
    if position >= state.next_position || state.records.insert(position, record).is_some() {
        return Err(corrupt(path, "checkpoint record position is inconsistent"));
    }
    if state.records.len() > state.capacity {
        return Err(corrupt(path, "checkpoint exceeds configured capacity"));
    }
    Ok(())
}

fn apply_enqueued(
    path: &Path,
    state: &mut CloudLinkSpoolState,
    record: CloudLinkRecord,
    next_position: u64,
    data_loss: Option<CloudLinkDataLossEvidence>,
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
    if state.records.len() == state.capacity {
        let earliest = state
            .records
            .keys()
            .next()
            .copied()
            .ok_or_else(|| corrupt(path, "capacity eviction has no retained record"))?;
        state.records.remove(&earliest);
    }
    state.records.insert(record.identity().position(), record);
    state.next_position = next_position;
    state.data_loss = data_loss;
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
    if state.stream_epoch == 0
        || state.next_position == 0
        || state.capacity == 0
        || state.records.len() > state.capacity
        || state.last_acknowledged_position >= state.next_position
    {
        return Err(corrupt(
            path,
            "recovered CloudLink cursor metadata is invalid",
        ));
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
    Ok(())
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
