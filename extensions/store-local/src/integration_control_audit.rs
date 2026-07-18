//! Process-exclusive append-only audit journal for governed Integration control.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use aether_integration_control::{
    AuditEvent, AuditEventKind, AuditRecord, ControlDependencyError, ControlFailureCode,
    IntegrationControlAudit,
};
use async_trait::async_trait;
use fs2::FileExt;
use serde::{Deserialize, Serialize};

const AUDIT_SCHEMA: &str = "aether.edge.integration-control-audit.v1";
const MAX_AUDIT_BYTES: u64 = 16 * 1_024 * 1_024;

/// Durable JSON-lines audit sink held by one process at a time.
pub struct FileIntegrationControlAudit {
    path: PathBuf,
    _lock_file: File,
    file: Mutex<File>,
}

impl std::fmt::Debug for FileIntegrationControlAudit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FileIntegrationControlAudit")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl FileIntegrationControlAudit {
    /// Opens and validates an append-only process-exclusive audit journal.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ControlDependencyError> {
        let path = path.as_ref().to_path_buf();
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .ok_or_else(audit_failure)?;
        std::fs::create_dir_all(parent).map_err(|_source| audit_failure())?;
        let parent_metadata =
            std::fs::symlink_metadata(parent).map_err(|_source| audit_failure())?;
        if !parent_metadata.file_type().is_dir() || parent_metadata.file_type().is_symlink() {
            return Err(audit_failure());
        }
        reject_non_regular_existing_file(&path)?;

        let lock_path = sibling_path(&path, ".lock")?;
        reject_non_regular_existing_file(&lock_path)?;
        let lock_file = private_writable_file(&lock_path)?;
        FileExt::try_lock_exclusive(&lock_file).map_err(|_source| audit_failure())?;

        validate_existing(&path)?;
        let file = private_writable_file(&path)?;
        Ok(Self {
            path,
            _lock_file: lock_file,
            file: Mutex::new(file),
        })
    }
}

#[async_trait]
impl IntegrationControlAudit for FileIntegrationControlAudit {
    async fn record(&self, event: &AuditEvent<'_>) -> Result<AuditRecord, ControlDependencyError> {
        let record_id = uuid::Uuid::new_v4().to_string();
        let record = StoredAuditRecord {
            schema: AUDIT_SCHEMA.to_string(),
            record_id: record_id.clone(),
            kind: event_kind(event.kind()).to_string(),
            job_id: event.job_id().to_string(),
            intent_digest: event.intent_digest().to_string(),
            failure_code: event
                .failure_code()
                .map(ControlFailureCode::as_str)
                .map(str::to_string),
        };
        validate_record(&record)?;
        let mut bytes = serde_json::to_vec(&record).map_err(|_source| audit_failure())?;
        bytes.push(b'\n');

        let mut file = self.file.lock().map_err(|_source| audit_failure())?;
        let current = file.metadata().map_err(|_source| audit_failure())?.len();
        let appended = u64::try_from(bytes.len()).map_err(|_source| audit_failure())?;
        if current.saturating_add(appended) > MAX_AUDIT_BYTES {
            return Err(audit_failure());
        }
        file.write_all(&bytes).map_err(|_source| audit_failure())?;
        file.sync_data().map_err(|_source| audit_failure())?;
        AuditRecord::complete(record_id)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredAuditRecord {
    schema: String,
    record_id: String,
    kind: String,
    job_id: String,
    intent_digest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure_code: Option<String>,
}

fn validate_existing(path: &Path) -> Result<(), ControlDependencyError> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_source) => return Err(audit_failure()),
    };
    let length = file.metadata().map_err(|_source| audit_failure())?.len();
    if length > MAX_AUDIT_BYTES {
        return Err(audit_failure());
    }
    let capacity = usize::try_from(length).map_err(|_source| audit_failure())?;
    let mut bytes = Vec::with_capacity(capacity);
    file.read_to_end(&mut bytes)
        .map_err(|_source| audit_failure())?;
    if !bytes.is_empty() && bytes.last() != Some(&b'\n') {
        return Err(audit_failure());
    }
    for line in bytes.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        let record: StoredAuditRecord =
            serde_json::from_slice(line).map_err(|_source| audit_failure())?;
        validate_record(&record)?;
    }
    Ok(())
}

fn validate_record(record: &StoredAuditRecord) -> Result<(), ControlDependencyError> {
    if record.schema != AUDIT_SCHEMA
        || uuid::Uuid::parse_str(&record.record_id).is_err()
        || uuid::Uuid::parse_str(&record.job_id).is_err()
        || !valid_digest(&record.intent_digest)
        || !matches!(
            record.kind.as_str(),
            "dispatch-authorized"
                | "edge-rejected"
                | "provider-accepted"
                | "provider-rejected"
                | "provider-outcome-unknown"
                | "interrupted-recovered"
        )
        || record
            .failure_code
            .as_deref()
            .is_some_and(|code| !valid_failure_code(code))
    {
        return Err(audit_failure());
    }
    Ok(())
}

fn event_kind(kind: AuditEventKind) -> &'static str {
    match kind {
        AuditEventKind::DispatchAuthorized => "dispatch-authorized",
        AuditEventKind::EdgeRejected => "edge-rejected",
        AuditEventKind::ProviderAccepted => "provider-accepted",
        AuditEventKind::ProviderRejected => "provider-rejected",
        AuditEventKind::ProviderOutcomeUnknown => "provider-outcome-unknown",
        AuditEventKind::InterruptedRecovered => "interrupted-recovered",
    }
}

fn valid_failure_code(value: &str) -> bool {
    matches!(
        value,
        "TARGET_NOT_FOUND"
            | "TOPOLOGY_GENERATION_MISMATCH"
            | "ENTITY_KIND_DENIED"
            | "POINT_DENIED"
            | "NOT_COMMISSIONED"
            | "DELEGATION_DENIED"
            | "POLICY_DENIED"
            | "CONFIRMATION_INVALID"
            | "TARGET_UNAVAILABLE"
            | "PROVIDER_REJECTED"
            | "PROVIDER_OUTCOME_UNKNOWN"
            | "PROVIDER_TIMEOUT"
            | "INTERRUPTED"
            | "AUDIT_INCOMPLETE"
    )
}

fn valid_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

fn reject_non_regular_existing_file(path: &Path) -> Result<(), ControlDependencyError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() && has_private_permissions(&metadata) => {
            Ok(())
        },
        Ok(_metadata) => Err(audit_failure()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_source) => Err(audit_failure()),
    }
}

fn private_writable_file(path: &Path) -> Result<File, ControlDependencyError> {
    let mut options = OpenOptions::new();
    options.create(true).append(true).read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path).map_err(|_source| audit_failure())?;
    if !has_private_permissions(&file.metadata().map_err(|_source| audit_failure())?) {
        return Err(audit_failure());
    }
    Ok(file)
}

fn sibling_path(path: &Path, suffix: &str) -> Result<PathBuf, ControlDependencyError> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(audit_failure)?;
    Ok(path.with_file_name(format!("{name}{suffix}")))
}

fn audit_failure() -> ControlDependencyError {
    ControlDependencyError::new(ControlFailureCode::AuditIncomplete)
}

#[cfg(unix)]
fn has_private_permissions(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;

    metadata.permissions().mode() & 0o077 == 0
}

// Portable fallback: platforms without Unix mode bits still retain regular-file,
// non-symlink, process-lock, and append-only validation checks.
#[cfg(not(unix))]
const fn has_private_permissions(_metadata: &std::fs::Metadata) -> bool {
    true
}
