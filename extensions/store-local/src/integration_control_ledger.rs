//! Process-exclusive crash-safe Integration-control job ledger and receipt spool.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use aether_integration_control::{
    ActionReceiptPayload, ControlStoreError, IntegrationControlLedger, LedgerClaim,
    LedgerClaimOutcome, LedgerEntry, LedgerEntryState, LedgerJobKey, SpooledActionReceipt,
};
use aether_ports::{CloudLinkDurableAck, DurableAckOutcome};
use async_trait::async_trait;
use fs2::FileExt;
use serde::{Deserialize, Serialize};

const FILE_SCHEMA: &str = "aether.edge.integration-control-ledger.v1";
const MAX_FILE_BYTES: u64 = 16 * 1_024 * 1_024;
const MAX_JOBS: usize = 65_536;

#[derive(Debug, Clone)]
struct LedgerState {
    jobs: BTreeMap<LedgerJobKey, LedgerEntry>,
    deliveries: BTreeMap<String, SpooledActionReceipt>,
    pending: BTreeMap<u64, SpooledActionReceipt>,
    next_position: u64,
    last_ack: Option<CloudLinkDurableAck>,
    last_ack_terminal: Option<SpooledActionReceipt>,
}

impl Default for LedgerState {
    fn default() -> Self {
        Self {
            jobs: BTreeMap::new(),
            deliveries: BTreeMap::new(),
            pending: BTreeMap::new(),
            next_position: 1,
            last_ack: None,
            last_ack_terminal: None,
        }
    }
}

/// Atomically rewritten local job ledger with a durable terminal receipt spool.
///
/// The process lock prevents two runtimes from dispatching the same job. Every
/// claim is synchronized before it returns, and every terminal receipt and
/// spool insertion is committed in one replacement.
pub struct FileIntegrationControlLedger {
    path: PathBuf,
    _lock_file: File,
    state: Mutex<LedgerState>,
}

impl std::fmt::Debug for FileIntegrationControlLedger {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FileIntegrationControlLedger")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl FileIntegrationControlLedger {
    /// Opens or creates a process-exclusive persistent ledger.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ControlStoreError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|_source| ControlStoreError)?;
            let metadata =
                std::fs::symlink_metadata(parent).map_err(|_source| ControlStoreError)?;
            if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
                return Err(ControlStoreError);
            }
        }
        reject_non_regular_existing_file(&path)?;
        let lock_path = sibling_path(&path, ".lock");
        reject_non_regular_existing_file(&lock_path)?;
        let lock_file = writable_file(&lock_path, true)?;
        FileExt::try_lock_exclusive(&lock_file).map_err(|_source| ControlStoreError)?;

        let temporary_path = sibling_path(&path, ".replace.tmp");
        match std::fs::remove_file(&temporary_path) {
            Ok(()) => {},
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {},
            Err(_source) => return Err(ControlStoreError),
        }

        let state = load(&path)?;
        if !path.exists() {
            persist(&path, &state)?;
        }
        Ok(Self {
            path,
            _lock_file: lock_file,
            state: Mutex::new(state),
        })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, LedgerState>, ControlStoreError> {
        self.state.lock().map_err(|_source| ControlStoreError)
    }

    fn mutate<T>(
        &self,
        operation: impl FnOnce(&mut LedgerState) -> Result<T, ControlStoreError>,
    ) -> Result<T, ControlStoreError> {
        let mut current = self.lock()?;
        let mut next = current.clone();
        let result = operation(&mut next)?;
        if next.jobs != current.jobs
            || next.deliveries != current.deliveries
            || next.pending != current.pending
            || next.next_position != current.next_position
            || next.last_ack != current.last_ack
            || next.last_ack_terminal != current.last_ack_terminal
        {
            persist(&self.path, &next)?;
            *current = next;
        }
        Ok(result)
    }
}

fn reject_non_regular_existing_file(path: &Path) -> Result<(), ControlStoreError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() && has_private_permissions(&metadata) => {
            Ok(())
        },
        Ok(_metadata) => Err(ControlStoreError),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_source) => Err(ControlStoreError),
    }
}

#[async_trait]
impl IntegrationControlLedger for FileIntegrationControlLedger {
    async fn find(&self, key: &LedgerJobKey) -> Result<Option<LedgerEntry>, ControlStoreError> {
        Ok(self.lock()?.jobs.get(key).cloned())
    }

    async fn claim(&self, claim: LedgerClaim) -> Result<LedgerClaimOutcome, ControlStoreError> {
        self.mutate(|state| {
            if let Some(existing) = state.jobs.get(claim.key()) {
                return if existing.intent_digest() == claim.intent_digest() {
                    Ok(LedgerClaimOutcome::Existing(Box::new(existing.clone())))
                } else {
                    Ok(LedgerClaimOutcome::DigestConflict)
                };
            }
            if state.jobs.len() >= MAX_JOBS {
                return Err(ControlStoreError);
            }
            state
                .jobs
                .insert(claim.key().clone(), LedgerEntry::in_progress(claim));
            Ok(LedgerClaimOutcome::Claimed)
        })
    }

    async fn complete(
        &self,
        key: &LedgerJobKey,
        intent_digest: &str,
        receipt: ActionReceiptPayload,
    ) -> Result<(), ControlStoreError> {
        receipt
            .validate_contract()
            .map_err(|_source| ControlStoreError)?;
        if receipt.job_id() != key.job_id() || receipt.intent_digest() != intent_digest {
            return Err(ControlStoreError);
        }
        self.mutate(|state| {
            let Some(existing) = state.jobs.get(key) else {
                return Err(ControlStoreError);
            };
            if existing.intent_digest() != intent_digest {
                return Err(ControlStoreError);
            }
            if let LedgerEntryState::Complete(stored) = existing.state() {
                return if stored == &receipt {
                    Ok(())
                } else {
                    Err(ControlStoreError)
                };
            }
            let delivery = match state.deliveries.get(receipt.receipt_id()) {
                Some(delivery) => delivery.clone(),
                None => {
                    let position = state.next_position;
                    state.next_position = state
                        .next_position
                        .checked_add(1)
                        .ok_or(ControlStoreError)?;
                    let delivery = SpooledActionReceipt::new(position, receipt.clone())?;
                    state
                        .deliveries
                        .insert(receipt.receipt_id().to_string(), delivery.clone());
                    delivery
                },
            };
            state.pending.insert(delivery.position(), delivery);
            state.jobs.insert(
                key.clone(),
                LedgerEntry::complete(intent_digest.to_string(), receipt),
            );
            Ok(())
        })
    }

    async fn requeue(&self, key: &LedgerJobKey) -> Result<(), ControlStoreError> {
        self.mutate(|state| {
            let receipt = match state.jobs.get(key).map(LedgerEntry::state) {
                Some(LedgerEntryState::Complete(receipt)) => receipt.clone(),
                Some(LedgerEntryState::InProgress(_)) | None => return Err(ControlStoreError),
            };
            let mut delivery = state
                .deliveries
                .get(receipt.receipt_id())
                .cloned()
                .ok_or(ControlStoreError)?;
            if state
                .last_ack
                .as_ref()
                .is_some_and(|ack| delivery.position() <= ack.acknowledged_position())
            {
                let position = state.next_position;
                state.next_position = state
                    .next_position
                    .checked_add(1)
                    .ok_or(ControlStoreError)?;
                delivery = SpooledActionReceipt::new(position, receipt.clone())?;
                state
                    .deliveries
                    .insert(receipt.receipt_id().to_string(), delivery.clone());
            }
            state.pending.insert(delivery.position(), delivery);
            Ok(())
        })
    }

    async fn interrupted(&self) -> Result<Vec<LedgerClaim>, ControlStoreError> {
        Ok(self
            .lock()?
            .jobs
            .values()
            .filter_map(|entry| match entry.state() {
                LedgerEntryState::InProgress(claim) => Some(claim.clone()),
                LedgerEntryState::Complete(_) => None,
            })
            .collect())
    }

    async fn pending_receipts(
        &self,
        limit: usize,
    ) -> Result<Vec<SpooledActionReceipt>, ControlStoreError> {
        Ok(self.lock()?.pending.values().take(limit).cloned().collect())
    }

    async fn acknowledge(
        &self,
        ack: &CloudLinkDurableAck,
    ) -> Result<DurableAckOutcome, ControlStoreError> {
        self.mutate(|state| {
            if state.last_ack.as_ref() == Some(ack) {
                return Ok(DurableAckOutcome::Duplicate);
            }
            let terminal = terminal_for_ack(ack, &state.deliveries)?.clone();
            if state.pending.get(&terminal.position()) != Some(&terminal) {
                return Err(ControlStoreError);
            }
            if state
                .last_ack
                .as_ref()
                .is_some_and(|last| ack.acknowledged_position() <= last.acknowledged_position())
            {
                return Err(ControlStoreError);
            }
            let positions: Vec<_> = state
                .pending
                .range(..=ack.acknowledged_position())
                .map(|(position, _record)| *position)
                .collect();
            for position in &positions {
                state.pending.remove(position);
            }
            state.last_ack = Some(ack.clone());
            state.last_ack_terminal = Some(terminal);
            Ok(DurableAckOutcome::Applied {
                removed: positions.len(),
            })
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileDocument {
    schema: String,
    next_position: u64,
    jobs: Vec<FileJob>,
    deliveries: Vec<SpooledActionReceipt>,
    pending_positions: Vec<u64>,
    last_ack: Option<CloudLinkDurableAck>,
    last_ack_terminal: Option<SpooledActionReceipt>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileJob {
    key: LedgerJobKey,
    entry: LedgerEntry,
}

fn load(path: &Path) -> Result<LedgerState, ControlStoreError> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LedgerState::default());
        },
        Err(_source) => return Err(ControlStoreError),
    };
    let length = file.metadata().map_err(|_source| ControlStoreError)?.len();
    if length == 0 || length > MAX_FILE_BYTES {
        return Err(ControlStoreError);
    }
    let capacity = usize::try_from(length).map_err(|_source| ControlStoreError)?;
    let mut bytes = Vec::with_capacity(capacity);
    file.read_to_end(&mut bytes)
        .map_err(|_source| ControlStoreError)?;
    let document: FileDocument =
        serde_json::from_slice(&bytes).map_err(|_source| ControlStoreError)?;
    if document.schema != FILE_SCHEMA
        || document.jobs.len() > MAX_JOBS
        || document.deliveries.len() > MAX_JOBS
        || document.next_position == 0
    {
        return Err(ControlStoreError);
    }

    let mut jobs = BTreeMap::new();
    for file_job in document.jobs {
        validate_file_job(&file_job)?;
        if jobs.insert(file_job.key, file_job.entry).is_some() {
            return Err(ControlStoreError);
        }
    }

    let mut deliveries = BTreeMap::new();
    let mut deliveries_by_position = BTreeMap::new();
    for delivery in document.deliveries {
        delivery.validate()?;
        if deliveries_by_position
            .insert(delivery.position(), delivery.clone())
            .is_some()
            || deliveries
                .insert(delivery.payload().receipt_id().to_string(), delivery)
                .is_some()
        {
            return Err(ControlStoreError);
        }
    }
    if deliveries_by_position
        .last_key_value()
        .is_some_and(|(position, _record)| *position >= document.next_position)
    {
        return Err(ControlStoreError);
    }
    for entry in jobs.values() {
        if let LedgerEntryState::Complete(receipt) = entry.state()
            && deliveries
                .get(receipt.receipt_id())
                .is_none_or(|delivery| delivery.payload() != receipt)
        {
            return Err(ControlStoreError);
        }
    }

    let mut pending = BTreeMap::new();
    for position in document.pending_positions {
        let delivery = deliveries_by_position
            .get(&position)
            .cloned()
            .ok_or(ControlStoreError)?;
        if pending.insert(position, delivery).is_some() {
            return Err(ControlStoreError);
        }
    }
    match (&document.last_ack, &document.last_ack_terminal) {
        (Some(ack), Some(terminal)) => {
            terminal.validate()?;
            validate_ack_against_terminal(ack, terminal)?;
            if terminal.position() >= document.next_position
                || deliveries
                    .get(terminal.payload().receipt_id())
                    .is_none_or(|current| current.payload() != terminal.payload())
                || pending
                    .first_key_value()
                    .is_some_and(|(position, _record)| *position <= ack.acknowledged_position())
            {
                return Err(ControlStoreError);
            }
        },
        (None, None) => {},
        (Some(_), None) | (None, Some(_)) => return Err(ControlStoreError),
    }
    let acknowledged_position = document
        .last_ack
        .as_ref()
        .map_or(0, CloudLinkDurableAck::acknowledged_position);
    if deliveries_by_position.iter().any(|(position, delivery)| {
        let is_pending = pending.get(position) == Some(delivery);
        (*position > acknowledged_position) != is_pending
    }) {
        return Err(ControlStoreError);
    }
    Ok(LedgerState {
        jobs,
        deliveries,
        pending,
        next_position: document.next_position,
        last_ack: document.last_ack,
        last_ack_terminal: document.last_ack_terminal,
    })
}

fn validate_file_job(file_job: &FileJob) -> Result<(), ControlStoreError> {
    let rebuilt_key = LedgerJobKey::new(file_job.key.gateway_id(), file_job.key.job_id())
        .map_err(|_source| ControlStoreError)?;
    if rebuilt_key != file_job.key {
        return Err(ControlStoreError);
    }
    match file_job.entry.state() {
        LedgerEntryState::InProgress(claim) => {
            let rebuilt = LedgerClaim::new(
                rebuilt_key,
                claim.intent_digest(),
                claim.target().clone(),
                claim.value(),
                claim.audit_record_id(),
            )
            .map_err(|_source| ControlStoreError)?;
            if &rebuilt != claim
                || claim.key() != &file_job.key
                || claim.intent_digest() != file_job.entry.intent_digest()
            {
                return Err(ControlStoreError);
            }
        },
        LedgerEntryState::Complete(receipt) => {
            receipt
                .validate_contract()
                .map_err(|_source| ControlStoreError)?;
            if receipt.job_id() != file_job.key.job_id()
                || receipt.intent_digest() != file_job.entry.intent_digest()
            {
                return Err(ControlStoreError);
            }
        },
    }
    Ok(())
}

fn terminal_for_ack<'a>(
    ack: &CloudLinkDurableAck,
    deliveries: &'a BTreeMap<String, SpooledActionReceipt>,
) -> Result<&'a SpooledActionReceipt, ControlStoreError> {
    validate_ack_identity(ack)?;
    let terminal = deliveries
        .values()
        .find(|record| record.position() == ack.acknowledged_position())
        .ok_or(ControlStoreError)?;
    validate_ack_against_terminal(ack, terminal)?;
    Ok(terminal)
}

fn validate_ack_against_terminal(
    ack: &CloudLinkDurableAck,
    terminal: &SpooledActionReceipt,
) -> Result<(), ControlStoreError> {
    validate_ack_identity(ack)?;
    if terminal.position() != ack.acknowledged_position()
        || terminal.batch_id() != ack.batch_id()
        || terminal.digest() != ack.digest()
    {
        return Err(ControlStoreError);
    }
    Ok(())
}

fn validate_ack_identity(ack: &CloudLinkDurableAck) -> Result<(), ControlStoreError> {
    if ack.stream_id() != "integration-control-receipts"
        || ack.stream_epoch() != 1
        || ack.acknowledged_position() == 0
        || ack.session().session_epoch() == 0
        || !valid_identifier(ack.session().session_id())
        || !valid_identifier(ack.receipt_id())
    {
        return Err(ControlStoreError);
    }
    Ok(())
}

fn valid_identifier(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric())
        && value.len() <= 128
        && bytes
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

fn persist(path: &Path, state: &LedgerState) -> Result<(), ControlStoreError> {
    let document = FileDocument {
        schema: FILE_SCHEMA.to_string(),
        next_position: state.next_position,
        jobs: state
            .jobs
            .iter()
            .map(|(key, entry)| FileJob {
                key: key.clone(),
                entry: entry.clone(),
            })
            .collect(),
        deliveries: state.deliveries.values().cloned().collect(),
        pending_positions: state.pending.keys().copied().collect(),
        last_ack: state.last_ack.clone(),
        last_ack_terminal: state.last_ack_terminal.clone(),
    };
    let bytes = serde_json::to_vec(&document).map_err(|_source| ControlStoreError)?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(ControlStoreError);
    }
    let temporary_path = sibling_path(path, ".replace.tmp");
    let mut temporary = create_new_file(&temporary_path)?;
    let write_result = temporary
        .write_all(&bytes)
        .and_then(|()| temporary.sync_all())
        .map_err(|_source| ControlStoreError);
    if let Err(error) = write_result {
        let _ignored = std::fs::remove_file(&temporary_path);
        return Err(error);
    }
    if std::fs::rename(&temporary_path, path).is_err() {
        let _ignored = std::fs::remove_file(&temporary_path);
        return Err(ControlStoreError);
    }
    sync_parent_directory(path)
}

fn writable_file(path: &Path, create: bool) -> Result<File, ControlStoreError> {
    let mut options = OpenOptions::new();
    options
        .create(create)
        .truncate(false)
        .read(true)
        .write(true);
    set_private_mode(&mut options);
    let file = options.open(path).map_err(|_source| ControlStoreError)?;
    if !has_private_permissions(&file.metadata().map_err(|_source| ControlStoreError)?) {
        return Err(ControlStoreError);
    }
    Ok(file)
}

fn create_new_file(path: &Path) -> Result<File, ControlStoreError> {
    let mut options = OpenOptions::new();
    options.create_new(true).read(true).write(true);
    set_private_mode(&mut options);
    let file = options.open(path).map_err(|_source| ControlStoreError)?;
    if !has_private_permissions(&file.metadata().map_err(|_source| ControlStoreError)?) {
        return Err(ControlStoreError);
    }
    Ok(file)
}

#[cfg(unix)]
fn set_private_mode(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn set_private_mode(_options: &mut OpenOptions) {}

#[cfg(unix)]
fn has_private_permissions(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;

    metadata.permissions().mode() & 0o077 == 0
}

// Portable fallback: platforms without Unix mode bits still retain regular-file,
// non-symlink, process-lock, and exclusive-replacement checks.
#[cfg(not(unix))]
const fn has_private_permissions(_metadata: &std::fs::Metadata) -> bool {
    true
}

fn sibling_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

fn sync_parent_directory(path: &Path) -> Result<(), ControlStoreError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|_source| ControlStoreError)
}
