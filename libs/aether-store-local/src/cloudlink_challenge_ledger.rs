//! Process-exclusive, bounded CloudLink session challenge replay ledger.

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

const LEDGER_SCHEMA: &str = "aether.edge.cloudlink-challenge-ledger.v1";
const MAX_CAPACITY: usize = 256;
const MAX_TRANSCRIPT_BYTES: usize = 64 * 1024;
const MAX_LEDGER_BYTES: u64 = 32 * 1024 * 1024;

/// Fail-closed challenge persistence or replay failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CloudLinkChallengeLedgerError {
    /// This file adapter requires Unix no-follow and owner-permission semantics.
    #[error("CloudLink challenge ledger is unsupported on this platform")]
    UnsupportedPlatform,
    /// Configuration or one opaque bounded transcript input is invalid.
    #[error("CloudLink challenge ledger input is invalid")]
    InvalidInput,
    /// The configured capacity cannot admit another unexpired challenge.
    #[error("CloudLink challenge ledger capacity is exhausted")]
    CapacityExceeded,
    /// One challenge identity was reused with conflicting signed bytes or hello bytes.
    #[error("CloudLink challenge replay conflicts with persisted authentication state")]
    ConflictingReplay,
    /// A consumed challenge was presented again.
    #[error("CloudLink challenge was already completed")]
    CompletedReplay,
    /// A strict challenge deadline has been reached or passed.
    #[error("CloudLink challenge deadline has expired")]
    MessageExpired,
    /// A mutation referenced a challenge that was never durably reserved.
    #[error("CloudLink challenge is not reserved")]
    MissingChallenge,
    /// A state transition was attempted before its prerequisite was durable.
    #[error("CloudLink challenge transition is invalid")]
    InvalidTransition,
    /// Ledger or lock file permissions permit access beyond the owner.
    #[error("CloudLink challenge ledger permissions are insecure")]
    InsecurePermissions,
    /// Persisted bytes are malformed, oversized, or internally inconsistent.
    #[error("CloudLink challenge ledger is corrupt")]
    Corrupt,
    /// A local file, lock, sync, or atomic replacement operation failed.
    #[error("CloudLink challenge ledger storage is unavailable")]
    Storage,
}

/// Result of reserving one exact signed challenge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudLinkChallengeReservation {
    /// Persisted challenge and original request inputs must be used to prepare the hello.
    Prepare {
        /// Exact canonical challenge bytes stored before signing the hello.
        challenge: Vec<u8>,
        /// Exact canonical request bytes whose client nonce and resume set are frozen.
        request: Vec<u8>,
    },
    /// An earlier attempt already persisted the exact hello; resend these bytes unchanged.
    RetryHello(Vec<u8>),
}

/// Exact pending challenge request and its restart-stable local deadline.
#[derive(Clone, PartialEq, Eq)]
pub struct CloudLinkPendingChallengeRequest {
    payload: Vec<u8>,
    expires_at_ms: u64,
}

impl core::fmt::Debug for CloudLinkPendingChallengeRequest {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("CloudLinkPendingChallengeRequest")
            .field("authentication_transcript", &"[REDACTED]")
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

impl CloudLinkPendingChallengeRequest {
    /// Returns the exact sensitive request bytes to publish or retry unchanged.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// Returns the fixed local deadline after which a new request may be generated.
    #[must_use]
    pub const fn expires_at_ms(&self) -> u64 {
        self.expires_at_ms
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ChallengeStatus {
    Pending,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChallengeRecord {
    challenge_id: String,
    expires_at_ms: u64,
    challenge_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    challenge: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    request: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hello: Option<String>,
    status: ChallengeStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    completion_order: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingRequest {
    expires_at_ms: u64,
    request: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LedgerState {
    schema: String,
    capacity: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending_request: Option<PendingRequest>,
    next_completion_order: u64,
    records: Vec<ChallengeRecord>,
}

impl LedgerState {
    fn empty(capacity: usize) -> Self {
        Self {
            schema: LEDGER_SCHEMA.to_owned(),
            capacity,
            pending_request: None,
            next_completion_order: 1,
            records: Vec::new(),
        }
    }

    fn validate(&self, configured_capacity: usize) -> Result<(), CloudLinkChallengeLedgerError> {
        if self.schema != LEDGER_SCHEMA
            || self.capacity != configured_capacity
            || self.records.len() > self.capacity
            || self.next_completion_order == 0
        {
            return Err(CloudLinkChallengeLedgerError::Corrupt);
        }
        if self.pending_request.as_ref().is_some_and(|pending| {
            pending.expires_at_ms == 0 || !valid_transcript(pending.request.as_bytes())
        }) {
            return Err(CloudLinkChallengeLedgerError::Corrupt);
        }
        let mut identities = HashSet::with_capacity(self.records.len());
        let mut completion_orders = HashSet::with_capacity(self.records.len());
        for record in &self.records {
            if !valid_identity(&record.challenge_id)
                || !identities.insert(record.challenge_id.as_str())
                || record.expires_at_ms == 0
                || !valid_digest(&record.challenge_digest)
            {
                return Err(CloudLinkChallengeLedgerError::Corrupt);
            }
            match record.status {
                ChallengeStatus::Pending => {
                    if record
                        .challenge
                        .as_ref()
                        .is_none_or(|challenge| !valid_transcript(challenge.as_bytes()))
                        || record
                            .request
                            .as_ref()
                            .is_none_or(|request| !valid_transcript(request.as_bytes()))
                        || record
                            .hello
                            .as_ref()
                            .is_some_and(|hello| !valid_transcript(hello.as_bytes()))
                        || record.completion_order.is_some()
                    {
                        return Err(CloudLinkChallengeLedgerError::Corrupt);
                    }
                },
                ChallengeStatus::Completed => {
                    if record.challenge.is_some()
                        || record.request.is_some()
                        || record.hello.is_some()
                        || record.completion_order.is_none_or(|order| {
                            order >= self.next_completion_order || !completion_orders.insert(order)
                        })
                    {
                        return Err(CloudLinkChallengeLedgerError::Corrupt);
                    }
                },
            }
        }
        Ok(())
    }
}

/// Owner-only, atomically rewritten local challenge replay ledger.
pub struct FileCloudLinkChallengeLedger {
    path: PathBuf,
    state: Mutex<LedgerState>,
    _process_lock: File,
}

impl core::fmt::Debug for FileCloudLinkChallengeLedger {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("FileCloudLinkChallengeLedger")
            .field("authentication_transcript", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl FileCloudLinkChallengeLedger {
    /// Opens one process-exclusive ledger with a fixed restart-stable capacity.
    ///
    /// This file adapter is available only on Unix, where every ledger and lock
    /// open can enforce no-follow and exact owner-only permission semantics.
    pub fn open(
        path: impl AsRef<Path>,
        capacity: usize,
    ) -> Result<Self, CloudLinkChallengeLedgerError> {
        #[cfg(not(unix))]
        {
            let _ = (path, capacity);
            Err(CloudLinkChallengeLedgerError::UnsupportedPlatform)
        }
        #[cfg(unix)]
        {
            Self::open_unix(path.as_ref(), capacity)
        }
    }

    #[cfg(unix)]
    fn open_unix(path: &Path, capacity: usize) -> Result<Self, CloudLinkChallengeLedgerError> {
        if !(1..=MAX_CAPACITY).contains(&capacity) {
            return Err(CloudLinkChallengeLedgerError::InvalidInput);
        }
        if !path.is_absolute() || path.file_name().is_none() {
            return Err(CloudLinkChallengeLedgerError::InvalidInput);
        }
        let parent = path
            .parent()
            .ok_or(CloudLinkChallengeLedgerError::InvalidInput)?;
        ensure_secure_parent_directory(parent)?;
        let lock_path = sibling_path(path, ".lock")?;
        let process_lock = open_owner_only(&lock_path, true)?;
        fs2::FileExt::try_lock_exclusive(&process_lock)
            .map_err(|_source| CloudLinkChallengeLedgerError::Storage)?;

        let state = match open_existing_owner_only(path)? {
            Some(file) => {
                let metadata = file
                    .metadata()
                    .map_err(|_source| CloudLinkChallengeLedgerError::Storage)?;
                validate_regular_owner_only(&metadata)?;
                if metadata.len() == 0 || metadata.len() > MAX_LEDGER_BYTES {
                    return Err(CloudLinkChallengeLedgerError::Corrupt);
                }
                let mut bytes = Vec::with_capacity(
                    usize::try_from(metadata.len())
                        .map_err(|_source| CloudLinkChallengeLedgerError::Corrupt)?,
                );
                file.take(MAX_LEDGER_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|_source| CloudLinkChallengeLedgerError::Storage)?;
                if bytes.is_empty()
                    || u64::try_from(bytes.len())
                        .ok()
                        .is_none_or(|length| length > MAX_LEDGER_BYTES)
                {
                    return Err(CloudLinkChallengeLedgerError::Corrupt);
                }
                let state: LedgerState = serde_json::from_slice(&bytes)
                    .map_err(|_source| CloudLinkChallengeLedgerError::Corrupt)?;
                state.validate(capacity)?;
                state
            },
            None => {
                let state = LedgerState::empty(capacity);
                write_state(path, &state)?;
                state
            },
        };

        Ok(Self {
            path: path.to_path_buf(),
            state: Mutex::new(state),
            _process_lock: process_lock,
        })
    }

    /// Persists a complete challenge request before its first publication.
    ///
    /// While an earlier request is live, reconnects and process restarts receive
    /// its exact bytes rather than a newly generated client nonce or resume set.
    pub fn prepare_request(
        &self,
        request: &[u8],
        expires_at_ms: u64,
        evaluation_time_ms: u64,
    ) -> Result<CloudLinkPendingChallengeRequest, CloudLinkChallengeLedgerError> {
        if !valid_transcript(request) || expires_at_ms == 0 {
            return Err(CloudLinkChallengeLedgerError::InvalidInput);
        }
        if evaluation_time_ms >= expires_at_ms {
            return Err(CloudLinkChallengeLedgerError::MessageExpired);
        }
        let request = std::str::from_utf8(request)
            .map_err(|_source| CloudLinkChallengeLedgerError::InvalidInput)?;
        self.mutate(|state| {
            if state
                .pending_request
                .as_ref()
                .is_some_and(|pending| pending.expires_at_ms <= evaluation_time_ms)
            {
                state.pending_request = None;
            }
            if let Some(pending) = &state.pending_request {
                return Ok(CloudLinkPendingChallengeRequest {
                    payload: pending.request.as_bytes().to_vec(),
                    expires_at_ms: pending.expires_at_ms,
                });
            }
            state.pending_request = Some(PendingRequest {
                expires_at_ms,
                request: request.to_owned(),
            });
            Ok(CloudLinkPendingChallengeRequest {
                payload: request.as_bytes().to_vec(),
                expires_at_ms,
            })
        })
    }

    /// Reserves an exact challenge before a Gateway hello is created or sent.
    pub fn reserve(
        &self,
        challenge_id: &str,
        expires_at_ms: u64,
        challenge: &[u8],
        request: &[u8],
        evaluation_time_ms: u64,
    ) -> Result<CloudLinkChallengeReservation, CloudLinkChallengeLedgerError> {
        if !valid_identity(challenge_id)
            || expires_at_ms == 0
            || !valid_transcript(challenge)
            || !valid_transcript(request)
        {
            return Err(CloudLinkChallengeLedgerError::InvalidInput);
        }
        if evaluation_time_ms >= expires_at_ms {
            return Err(CloudLinkChallengeLedgerError::MessageExpired);
        }
        let challenge = std::str::from_utf8(challenge)
            .map_err(|_source| CloudLinkChallengeLedgerError::InvalidInput)?;
        let challenge_digest = transcript_digest(challenge.as_bytes());
        let request = std::str::from_utf8(request)
            .map_err(|_source| CloudLinkChallengeLedgerError::InvalidInput)?;
        self.mutate(|state| {
            state.records.retain(|record| {
                record.status == ChallengeStatus::Completed
                    || record.expires_at_ms > evaluation_time_ms
            });
            if let Some(record) = state
                .records
                .iter()
                .find(|record| record.challenge_id == challenge_id)
            {
                if record.expires_at_ms != expires_at_ms
                    || record.challenge_digest != challenge_digest
                {
                    return Err(CloudLinkChallengeLedgerError::ConflictingReplay);
                }
                if record.status == ChallengeStatus::Completed {
                    return Err(CloudLinkChallengeLedgerError::CompletedReplay);
                }
                let pending_request = state
                    .pending_request
                    .as_mut()
                    .filter(|pending| pending.expires_at_ms > evaluation_time_ms)
                    .ok_or(CloudLinkChallengeLedgerError::MissingChallenge)?;
                if pending_request.request != request {
                    return Err(CloudLinkChallengeLedgerError::ConflictingReplay);
                }
                pending_request.expires_at_ms = expires_at_ms;
                let persisted_challenge = record
                    .challenge
                    .as_ref()
                    .ok_or(CloudLinkChallengeLedgerError::Corrupt)?;
                let persisted_request = record
                    .request
                    .as_ref()
                    .ok_or(CloudLinkChallengeLedgerError::Corrupt)?;
                return match &record.hello {
                    Some(hello) => Ok(CloudLinkChallengeReservation::RetryHello(
                        hello.as_bytes().to_vec(),
                    )),
                    None => Ok(CloudLinkChallengeReservation::Prepare {
                        challenge: persisted_challenge.as_bytes().to_vec(),
                        request: persisted_request.as_bytes().to_vec(),
                    }),
                };
            }
            let pending_request = state
                .pending_request
                .as_mut()
                .filter(|pending| pending.expires_at_ms > evaluation_time_ms)
                .ok_or(CloudLinkChallengeLedgerError::MissingChallenge)?;
            if pending_request.request != request {
                return Err(CloudLinkChallengeLedgerError::ConflictingReplay);
            }
            pending_request.expires_at_ms = expires_at_ms;
            if state.records.len() == state.capacity
                && let Some((index, _record)) = state
                    .records
                    .iter()
                    .enumerate()
                    .filter(|(_index, record)| record.status == ChallengeStatus::Completed)
                    .min_by_key(|(_index, record)| record.completion_order)
            {
                state.records.remove(index);
            }
            if state.records.len() == state.capacity {
                return Err(CloudLinkChallengeLedgerError::CapacityExceeded);
            }
            state.records.push(ChallengeRecord {
                challenge_id: challenge_id.to_owned(),
                expires_at_ms,
                challenge_digest,
                challenge: Some(challenge.to_owned()),
                request: Some(request.to_owned()),
                hello: None,
                status: ChallengeStatus::Pending,
                completion_order: None,
            });
            state
                .records
                .sort_unstable_by(|left, right| left.challenge_id.cmp(&right.challenge_id));
            Ok(CloudLinkChallengeReservation::Prepare {
                challenge: challenge.as_bytes().to_vec(),
                request: request.as_bytes().to_vec(),
            })
        })
    }

    /// Persists one exact signed hello before it is published.
    pub fn store_hello(
        &self,
        challenge_id: &str,
        hello: &[u8],
    ) -> Result<Vec<u8>, CloudLinkChallengeLedgerError> {
        if !valid_identity(challenge_id) || !valid_transcript(hello) {
            return Err(CloudLinkChallengeLedgerError::InvalidInput);
        }
        let hello = std::str::from_utf8(hello)
            .map_err(|_source| CloudLinkChallengeLedgerError::InvalidInput)?;
        self.mutate(|state| {
            let record = state
                .records
                .iter_mut()
                .find(|record| record.challenge_id == challenge_id)
                .ok_or(CloudLinkChallengeLedgerError::MissingChallenge)?;
            if record.status == ChallengeStatus::Completed {
                return Err(CloudLinkChallengeLedgerError::CompletedReplay);
            }
            match &record.hello {
                Some(existing) if existing == hello => Ok(existing.as_bytes().to_vec()),
                Some(_) => Err(CloudLinkChallengeLedgerError::ConflictingReplay),
                None => {
                    record.hello = Some(hello.to_owned());
                    Ok(hello.as_bytes().to_vec())
                },
            }
        })
    }

    /// Marks a challenge consumed only after a corresponding session was accepted.
    pub fn complete(&self, challenge_id: &str) -> Result<(), CloudLinkChallengeLedgerError> {
        if !valid_identity(challenge_id) {
            return Err(CloudLinkChallengeLedgerError::InvalidInput);
        }
        self.mutate(|state| {
            let record = state
                .records
                .iter_mut()
                .find(|record| record.challenge_id == challenge_id)
                .ok_or(CloudLinkChallengeLedgerError::MissingChallenge)?;
            if record.status == ChallengeStatus::Completed {
                return Err(CloudLinkChallengeLedgerError::CompletedReplay);
            }
            if record.hello.is_none() {
                return Err(CloudLinkChallengeLedgerError::InvalidTransition);
            }
            let record_request = record
                .request
                .as_deref()
                .ok_or(CloudLinkChallengeLedgerError::Corrupt)?;
            if state
                .pending_request
                .as_ref()
                .is_none_or(|pending| pending.request != record_request)
            {
                return Err(CloudLinkChallengeLedgerError::InvalidTransition);
            }
            record.status = ChallengeStatus::Completed;
            record.challenge = None;
            record.request = None;
            record.hello = None;
            record.completion_order = Some(state.next_completion_order);
            state.next_completion_order = state
                .next_completion_order
                .checked_add(1)
                .ok_or(CloudLinkChallengeLedgerError::Storage)?;
            state.pending_request = None;
            Ok(())
        })
    }

    fn mutate<T>(
        &self,
        mutation: impl FnOnce(&mut LedgerState) -> Result<T, CloudLinkChallengeLedgerError>,
    ) -> Result<T, CloudLinkChallengeLedgerError> {
        let mut current = self
            .state
            .lock()
            .map_err(|_source| CloudLinkChallengeLedgerError::Storage)?;
        let mut next = current.clone();
        let result = mutation(&mut next)?;
        next.validate(current.capacity)?;
        if next != *current {
            write_state(&self.path, &next)?;
            *current = next;
        }
        Ok(result)
    }
}

fn valid_identity(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes[8] == b'-'
        && bytes[13] == b'-'
        && bytes[18] == b'-'
        && bytes[23] == b'-'
        && matches!(bytes[14], b'1'..=b'8')
        && matches!(bytes[19], b'8' | b'9' | b'a' | b'b')
        && bytes.iter().enumerate().all(|(index, byte)| {
            matches!(index, 8 | 13 | 18 | 23)
                || (byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        })
}

fn valid_transcript(bytes: &[u8]) -> bool {
    !bytes.is_empty() && bytes.len() <= MAX_TRANSCRIPT_BYTES && std::str::from_utf8(bytes).is_ok()
}

fn transcript_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn sibling_path(path: &Path, suffix: &str) -> Result<PathBuf, CloudLinkChallengeLedgerError> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(CloudLinkChallengeLedgerError::InvalidInput)?;
    Ok(path.with_file_name(format!("{file_name}{suffix}")))
}

fn open_owner_only(path: &Path, create: bool) -> Result<File, CloudLinkChallengeLedgerError> {
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        validate_regular_owner_only(&metadata)?;
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(create);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .map_err(|_source| CloudLinkChallengeLedgerError::Storage)?;
    let metadata = file
        .metadata()
        .map_err(|_source| CloudLinkChallengeLedgerError::Storage)?;
    validate_regular_owner_only(&metadata)?;
    Ok(file)
}

#[cfg(unix)]
fn open_existing_owner_only(path: &Path) -> Result<Option<File>, CloudLinkChallengeLedgerError> {
    use std::os::unix::fs::OpenOptionsExt as _;

    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        validate_regular_owner_only(&metadata)?;
    }
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW);
    match options.open(path) {
        Ok(file) => {
            let metadata = file
                .metadata()
                .map_err(|_source| CloudLinkChallengeLedgerError::Storage)?;
            validate_regular_owner_only(&metadata)?;
            Ok(Some(file))
        },
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_source) => Err(CloudLinkChallengeLedgerError::Storage),
    }
}

#[cfg(unix)]
fn ensure_secure_parent_directory(parent: &Path) -> Result<(), CloudLinkChallengeLedgerError> {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};

    let existed = match std::fs::symlink_metadata(parent) {
        Ok(_metadata) => true,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => false,
        Err(_source) => return Err(CloudLinkChallengeLedgerError::Storage),
    };
    if !existed {
        let mut builder = std::fs::DirBuilder::new();
        builder
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .map_err(|_source| CloudLinkChallengeLedgerError::Storage)?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            .map_err(|_source| CloudLinkChallengeLedgerError::Storage)?;
    }
    let metadata = std::fs::symlink_metadata(parent)
        .map_err(|_source| CloudLinkChallengeLedgerError::Storage)?;
    if !metadata.file_type().is_dir() {
        return Err(CloudLinkChallengeLedgerError::Corrupt);
    }
    if metadata.permissions().mode() & 0o022 != 0 {
        return Err(CloudLinkChallengeLedgerError::InsecurePermissions);
    }
    Ok(())
}

fn validate_regular_owner_only(
    metadata: &std::fs::Metadata,
) -> Result<(), CloudLinkChallengeLedgerError> {
    if !metadata.file_type().is_file() {
        return Err(CloudLinkChallengeLedgerError::Corrupt);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        if metadata.permissions().mode() & 0o777 != 0o600 {
            return Err(CloudLinkChallengeLedgerError::InsecurePermissions);
        }
        if metadata.nlink() != 1 {
            return Err(CloudLinkChallengeLedgerError::InsecurePermissions);
        }
    }
    Ok(())
}

fn write_state(path: &Path, state: &LedgerState) -> Result<(), CloudLinkChallengeLedgerError> {
    let bytes =
        serde_json::to_vec(state).map_err(|_source| CloudLinkChallengeLedgerError::Corrupt)?;
    if bytes.is_empty()
        || u64::try_from(bytes.len())
            .ok()
            .is_none_or(|length| length > MAX_LEDGER_BYTES)
    {
        return Err(CloudLinkChallengeLedgerError::Corrupt);
    }
    let temporary = sibling_path(path, &format!(".tmp-{}", std::process::id()))?;
    match std::fs::remove_file(&temporary) {
        Ok(()) => {},
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {},
        Err(_source) => return Err(CloudLinkChallengeLedgerError::Storage),
    }
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let result = (|| {
        let mut file = options
            .open(&temporary)
            .map_err(|_source| CloudLinkChallengeLedgerError::Storage)?;
        file.write_all(&bytes)
            .map_err(|_source| CloudLinkChallengeLedgerError::Storage)?;
        file.sync_all()
            .map_err(|_source| CloudLinkChallengeLedgerError::Storage)?;
        std::fs::rename(&temporary, path)
            .map_err(|_source| CloudLinkChallengeLedgerError::Storage)?;
        sync_parent_directory(path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> Result<(), CloudLinkChallengeLedgerError> {
    use std::os::unix::fs::OpenOptionsExt as _;

    let parent = path
        .parent()
        .ok_or(CloudLinkChallengeLedgerError::Storage)?;
    let mut options = OpenOptions::new();
    let directory = options
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
        .open(parent)
        .map_err(|_source| CloudLinkChallengeLedgerError::Storage)?;
    let metadata = directory
        .metadata()
        .map_err(|_source| CloudLinkChallengeLedgerError::Storage)?;
    if !metadata.file_type().is_dir() {
        return Err(CloudLinkChallengeLedgerError::Corrupt);
    }
    directory
        .sync_all()
        .map_err(|_source| CloudLinkChallengeLedgerError::Storage)
}

#[cfg(not(unix))]
fn sync_parent_directory(_path: &Path) -> Result<(), CloudLinkChallengeLedgerError> {
    Ok(())
}
