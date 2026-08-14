//! Durable, bounded command lifecycle ledger.
//!
//! The ledger records command identities and state transitions, but deliberately
//! never replays device commands. On startup, commands that were only received
//! are failed, while queued or dispatching commands are classified as possibly
//! applied because a crash may have occurred after the device write boundary.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aether_domain::{CommandId, TimestampMs};
use sqlx::{Sqlite, SqlitePool, Transaction};

/// Default maximum number of durable command records.
pub const DEFAULT_COMMAND_LEDGER_ROW_CAPACITY: u64 = 100_000;

/// Default period for retaining terminal command identities after expiration.
pub const DEFAULT_COMMAND_LEDGER_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);

/// Default maximum number of terminal rows removed by one cleanup pass.
pub const DEFAULT_COMMAND_LEDGER_CLEANUP_LIMIT: u32 = 1_024;

/// Maximum UTF-8 byte length retained for one durable terminal diagnostic.
pub const MAX_COMMAND_LEDGER_DIAGNOSTIC_BYTES: usize = 512;

const RESTART_RECEIVED_DIAGNOSTIC: &str =
    "service restarted before queue admission completed; command was not replayed";
const RESTART_AMBIGUOUS_DIAGNOSTIC: &str = "service restarted after durable queue admission; device outcome is unknown; command was not replayed";
const COMMAND_LEDGER_TABLE_NAME: &str = "io_command_ledger";
const COMMAND_LEDGER_TABLE_DEFINITION: &str = r#"(
    command_id     TEXT PRIMARY KEY NOT NULL
        CHECK (
            length(command_id) = 32
            AND command_id = lower(command_id)
            AND command_id NOT GLOB '*[^0-9a-f]*'
        ),
    channel_id     INTEGER CHECK (
        channel_id IS NULL OR channel_id BETWEEN 1 AND 9999
    ),
    digest         BLOB NOT NULL
        CHECK (typeof(digest) = 'blob' AND length(digest) = 32),
    state          TEXT NOT NULL
        CHECK (state IN (
            'received', 'queued', 'dispatching', 'succeeded',
            'failed', 'expired', 'possibly_applied'
        )),
    expires_at_ms  INTEGER NOT NULL CHECK (expires_at_ms >= 0),
    received_at_ms INTEGER NOT NULL CHECK (received_at_ms >= 0),
    accepted_at_ms INTEGER CHECK (
        accepted_at_ms IS NULL OR accepted_at_ms >= received_at_ms
    ),
    updated_at_ms  INTEGER NOT NULL CHECK (updated_at_ms >= received_at_ms),
    diagnostic     TEXT CHECK (
        diagnostic IS NULL
        OR (
            typeof(diagnostic) = 'text'
            AND length(CAST(diagnostic AS BLOB)) <= 512
        )
    )
)"#;

fn command_ledger_table_sql(if_not_exists: bool) -> String {
    let conditional = if if_not_exists { " IF NOT EXISTS" } else { "" };
    format!(
        "CREATE TABLE{conditional} {COMMAND_LEDGER_TABLE_NAME} {COMMAND_LEDGER_TABLE_DEFINITION}"
    )
}

/// Tokenizes CREATE TABLE SQL so insignificant whitespace and keyword case do
/// not affect identity, while token boundaries and quoted CHECK literals stay
/// exact. This deliberately does not accept semantically equivalent rewrites:
/// the runtime owns one canonical table contract.
fn normalized_schema_identity(sql: &str) -> String {
    fn flush_token(token: &mut String, output: &mut Vec<String>) {
        if !token.is_empty() {
            output.push(std::mem::take(token));
        }
    }

    let mut output = Vec::new();
    let mut token = String::new();
    let mut characters = sql.chars().peekable();
    while let Some(character) = characters.next() {
        if character.is_ascii_whitespace() {
            flush_token(&mut token, &mut output);
            continue;
        }
        if character.is_ascii_alphanumeric() || matches!(character, '_' | '$') {
            token.push(character.to_ascii_lowercase());
            continue;
        }

        flush_token(&mut token, &mut output);
        if matches!(character, '\'' | '"' | '`' | '[') {
            let terminator = if character == '[' { ']' } else { character };
            let mut quoted = String::from(character);
            while let Some(next) = characters.next() {
                quoted.push(next);
                if next == terminator {
                    if characters.peek().copied() == Some(terminator) {
                        quoted.push(characters.next().unwrap_or(terminator));
                    } else {
                        break;
                    }
                }
            }
            output.push(quoted);
        } else if character != ';' {
            output.push(character.to_string());
        }
    }
    flush_token(&mut token, &mut output);
    output.join("\u{1f}")
}

/// Runtime configuration for the bounded ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandLedgerConfig {
    row_capacity: u64,
    terminal_retention: Duration,
    cleanup_limit: u32,
}

impl CommandLedgerConfig {
    /// Creates an explicit ledger configuration.
    #[must_use]
    pub const fn new(row_capacity: u64, terminal_retention: Duration, cleanup_limit: u32) -> Self {
        Self {
            row_capacity,
            terminal_retention,
            cleanup_limit,
        }
    }

    /// Returns the maximum number of durable rows.
    #[must_use]
    pub const fn row_capacity(self) -> u64 {
        self.row_capacity
    }

    /// Returns how long terminal identities remain protected after expiration.
    #[must_use]
    pub const fn terminal_retention(self) -> Duration {
        self.terminal_retention
    }

    /// Returns the maximum cleanup batch used by admission.
    #[must_use]
    pub const fn cleanup_limit(self) -> u32 {
        self.cleanup_limit
    }

    fn validate(self) -> Result<(), CommandLedgerError> {
        if self.row_capacity == 0 || self.row_capacity > i64::MAX as u64 {
            return Err(CommandLedgerError::InvalidConfiguration(
                "command ledger row capacity must be between 1 and i64::MAX",
            ));
        }
        if self.cleanup_limit == 0 {
            return Err(CommandLedgerError::InvalidConfiguration(
                "command ledger cleanup limit must be non-zero",
            ));
        }
        Ok(())
    }
}

impl Default for CommandLedgerConfig {
    fn default() -> Self {
        Self::new(
            DEFAULT_COMMAND_LEDGER_ROW_CAPACITY,
            DEFAULT_COMMAND_LEDGER_RETENTION,
            DEFAULT_COMMAND_LEDGER_CLEANUP_LIMIT,
        )
    }
}

/// Durable command lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommandLedgerState {
    /// The exact command identity was durably admitted.
    Received,
    /// The command was accepted into a channel's bounded queue.
    Queued,
    /// The channel task crossed into device dispatch.
    Dispatching,
    /// The device adapter reported success.
    Succeeded,
    /// The command failed before a successful device outcome was known.
    Failed,
    /// The command reached its deadline before a successful device outcome.
    Expired,
    /// A restart made the physical outcome unknowable; the command is not replayed.
    PossiblyApplied,
}

impl CommandLedgerState {
    /// Returns the stable SQLite representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Received => "received",
            Self::Queued => "queued",
            Self::Dispatching => "dispatching",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Expired => "expired",
            Self::PossiblyApplied => "possibly_applied",
        }
    }

    /// Returns whether no further automatic lifecycle transition is allowed.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Expired | Self::PossiblyApplied
        )
    }

    const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Received, Self::Queued | Self::Failed | Self::Expired)
                | (
                    Self::Queued,
                    Self::Dispatching | Self::Failed | Self::Expired
                )
                | (Self::Dispatching, Self::Succeeded | Self::PossiblyApplied)
        )
    }

    fn from_stored(value: &str) -> Result<Self, CommandLedgerError> {
        match value {
            "received" => Ok(Self::Received),
            "queued" => Ok(Self::Queued),
            "dispatching" => Ok(Self::Dispatching),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "expired" => Ok(Self::Expired),
            "possibly_applied" => Ok(Self::PossiblyApplied),
            _ => Err(CommandLedgerError::Corrupt(
                "command ledger row has an unknown state",
            )),
        }
    }
}

/// One typed record read from the durable ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandLedgerRecord {
    command_id: CommandId,
    channel_id: Option<u32>,
    digest: [u8; 32],
    state: CommandLedgerState,
    expires_at: TimestampMs,
    received_at: TimestampMs,
    accepted_at: Option<TimestampMs>,
    updated_at: TimestampMs,
    diagnostic: Option<String>,
}

impl CommandLedgerRecord {
    /// Returns the public command identity.
    #[must_use]
    pub const fn command_id(&self) -> CommandId {
        self.command_id
    }

    /// Returns the IO channel owning this command.
    #[must_use]
    pub const fn channel_id(&self) -> Option<u32> {
        self.channel_id
    }

    /// Returns the digest permanently bound to the command identity.
    #[must_use]
    pub const fn digest(&self) -> &[u8; 32] {
        &self.digest
    }

    /// Returns the latest durable lifecycle state.
    #[must_use]
    pub const fn state(&self) -> CommandLedgerState {
        self.state
    }

    /// Returns the command's fixed execution deadline.
    #[must_use]
    pub const fn expires_at(&self) -> TimestampMs {
        self.expires_at
    }

    /// Returns when this identity was first durably admitted.
    #[must_use]
    pub const fn received_at(&self) -> TimestampMs {
        self.received_at
    }

    /// Returns when queue admission was durably confirmed after enqueue.
    ///
    /// `None` is deliberately distinct from [`Self::state`]: a crash can leave
    /// a command in `PossiblyApplied` without proving that admission completed.
    #[must_use]
    pub const fn accepted_at(&self) -> Option<TimestampMs> {
        self.accepted_at
    }

    /// Returns when the durable state last advanced.
    #[must_use]
    pub const fn updated_at(&self) -> TimestampMs {
        self.updated_at
    }

    /// Returns the bounded durable diagnostic for terminal/ambiguous states.
    #[must_use]
    pub fn diagnostic(&self) -> Option<&str> {
        self.diagnostic.as_deref()
    }
}

/// Current durable command-ledger population by state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CommandLedgerStats {
    pub total: u64,
    pub capacity: u64,
    pub received: u64,
    pub queued: u64,
    pub dispatching: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub expired: u64,
    pub possibly_applied: u64,
    pub oldest_nonterminal_updated_at_ms: Option<u64>,
    pub oldest_terminal_updated_at_ms: Option<u64>,
    pub capacity_rejections: u64,
    pub cleanup_failures: u64,
    pub outcome_persistence_failures: u64,
    pub outcome_persistence_pending: u64,
}

/// Conservative state repair after one channel task exits abnormally.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CommandLedgerReconcile {
    /// Commands removed from the queue without crossing device dispatch.
    pub queued_failed: u64,
    /// Commands cancelled after device dispatch began.
    pub dispatching_possibly_applied: u64,
}

/// Result of binding a command identity to an exact semantic-operation digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandLedgerAdmission {
    /// A new identity was durably inserted in [`CommandLedgerState::Received`].
    New(CommandLedgerRecord),
    /// The same identity, target, kind, and value were already present; no state was changed.
    Same(CommandLedgerRecord),
    /// The identity was already bound to a different digest.
    Conflict,
}

/// Result of one compare-and-transition operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandLedgerTransition {
    /// The expected state matched and the transition was durably applied.
    Updated(CommandLedgerRecord),
    /// The record exists, but its current state did not match the expected state.
    NotUpdated(CommandLedgerRecord),
    /// The command identity is not present in the ledger.
    Missing,
}

/// Result of durably completing queue admission after the permit was sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandLedgerAcceptance {
    /// This call stored the first durable queue-admission timestamp.
    Marked(CommandLedgerRecord),
    /// A prior call already stored the queue-admission timestamp.
    AlreadyMarked(CommandLedgerRecord),
    /// The identity exists, but never reached a state that may be admitted.
    NotMarkable(CommandLedgerRecord),
    /// The identity does not exist.
    Missing,
}

/// Fail-closed ledger validation, capacity, or storage error.
#[derive(Debug, thiserror::Error)]
pub enum CommandLedgerError {
    /// Ledger bounds are invalid.
    #[error("invalid command ledger configuration: {0}")]
    InvalidConfiguration(&'static str),
    /// A timestamp cannot be represented by SQLite's signed INTEGER type.
    #[error("command ledger timestamp exceeds SQLite INTEGER range: {0}")]
    InvalidTimestamp(&'static str),
    /// No protected row may be evicted to admit another command.
    #[error("command ledger row capacity {capacity} is exhausted")]
    Capacity {
        /// Configured row quota.
        capacity: u64,
    },
    /// The requested lifecycle edge is forbidden.
    #[error("invalid command ledger transition from {from:?} to {to:?}")]
    InvalidTransition {
        /// Requested current state.
        from: CommandLedgerState,
        /// Requested next state.
        to: CommandLedgerState,
    },
    /// Persisted bytes violate the typed schema contract.
    #[error("corrupt command ledger: {0}")]
    Corrupt(&'static str),
    /// SQLite could not complete a ledger operation.
    #[error("command ledger storage unavailable: {0}")]
    Storage(#[from] sqlx::Error),
}

/// SQLite-backed command identity and lifecycle ledger.
#[derive(Clone)]
pub struct CommandLedger {
    pool: SqlitePool,
    config: CommandLedgerConfig,
    metrics: Arc<CommandLedgerMetrics>,
}

#[derive(Default)]
struct CommandLedgerMetrics {
    capacity_rejections: AtomicU64,
    cleanup_failures: AtomicU64,
    outcome_persistence_failures: AtomicU64,
    outcome_persistence_pending: AtomicU64,
}

impl CommandLedger {
    /// Creates the schema and performs conservative, non-replaying recovery.
    pub async fn initialize(pool: SqlitePool) -> Result<Self, CommandLedgerError> {
        Self::initialize_with_config(pool, CommandLedgerConfig::default()).await
    }

    /// Creates the schema with explicit bounds and performs restart recovery.
    pub async fn initialize_with_config(
        pool: SqlitePool,
        config: CommandLedgerConfig,
    ) -> Result<Self, CommandLedgerError> {
        config.validate()?;
        let create_table_sql = command_ledger_table_sql(true);
        sqlx::query(&create_table_sql).execute(&pool).await?;
        let actual_schema: Option<String> =
            sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?")
                .bind(COMMAND_LEDGER_TABLE_NAME)
                .fetch_optional(&pool)
                .await?;
        let expected_schema = command_ledger_table_sql(false);
        if actual_schema.as_deref().map(normalized_schema_identity)
            != Some(normalized_schema_identity(&expected_schema))
        {
            return Err(CommandLedgerError::Corrupt(
                "io_command_ledger schema does not match the command contract",
            ));
        }
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_io_command_ledger_cleanup
             ON io_command_ledger(state, expires_at_ms, updated_at_ms)",
        )
        .execute(&pool)
        .await?;

        let now_ms = system_time_ms()?;
        let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query(
            "UPDATE io_command_ledger
             SET state = 'failed', updated_at_ms = MAX(updated_at_ms, ?),
                 diagnostic = ?
             WHERE state = 'received'",
        )
        .bind(now_ms)
        .bind(RESTART_RECEIVED_DIAGNOSTIC)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "UPDATE io_command_ledger
             SET state = 'possibly_applied', updated_at_ms = MAX(updated_at_ms, ?),
                 diagnostic = ?
             WHERE state IN ('queued', 'dispatching')",
        )
        .bind(now_ms)
        .bind(RESTART_AMBIGUOUS_DIAGNOSTIC)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;

        Ok(Self {
            pool,
            config,
            metrics: Arc::new(CommandLedgerMetrics::default()),
        })
    }

    /// Returns this ledger's fixed capacity and retention configuration.
    #[must_use]
    pub const fn config(&self) -> CommandLedgerConfig {
        self.config
    }

    /// Records a terminal outcome that remained unpersisted after bounded
    /// retries. This is sticky and keeps readiness fail-closed until restart.
    pub fn record_outcome_persistence_failure(&self) {
        self.metrics
            .outcome_persistence_failures
            .fetch_add(1, Ordering::Relaxed);
        self.metrics
            .outcome_persistence_pending
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Atomically admits a new identity, recognizes an exact retry, or reports a conflict.
    pub async fn admit(
        &self,
        command_id: CommandId,
        digest: [u8; 32],
        expires_at: TimestampMs,
    ) -> Result<CommandLedgerAdmission, CommandLedgerError> {
        self.admit_inner(command_id, None, digest, expires_at).await
    }

    /// Atomically admits a command bound to its owning IO channel.
    pub async fn admit_for_channel(
        &self,
        command_id: CommandId,
        channel_id: u32,
        digest: [u8; 32],
        expires_at: TimestampMs,
    ) -> Result<CommandLedgerAdmission, CommandLedgerError> {
        if !(1..=9_999).contains(&channel_id) {
            return Err(CommandLedgerError::InvalidConfiguration(
                "command ledger channel id must be between 1 and 9999",
            ));
        }
        self.admit_inner(command_id, Some(channel_id), digest, expires_at)
            .await
    }

    async fn admit_inner(
        &self,
        command_id: CommandId,
        channel_id: Option<u32>,
        digest: [u8; 32],
        expires_at: TimestampMs,
    ) -> Result<CommandLedgerAdmission, CommandLedgerError> {
        let command_id_text = command_id_text(command_id);
        let expires_at_ms = sqlite_timestamp(expires_at, "expires_at")?;
        let now_ms = system_time_ms()?;
        let mut transaction = self.pool.begin_with("BEGIN IMMEDIATE").await?;

        if let Some(existing) = load_stored_record(&mut transaction, &command_id_text).await? {
            let existing = existing.into_typed()?;
            let outcome = if existing.digest == digest {
                CommandLedgerAdmission::Same(existing)
            } else {
                CommandLedgerAdmission::Conflict
            };
            transaction.commit().await?;
            return Ok(outcome);
        }

        let mut row_count = ledger_row_count(&mut transaction).await?;
        if row_count >= self.config.row_capacity {
            if let Some(cutoff) = cleanup_cutoff(now_ms, self.config.terminal_retention) {
                if let Err(error) =
                    delete_terminal_rows(&mut transaction, cutoff, self.config.cleanup_limit).await
                {
                    self.metrics
                        .cleanup_failures
                        .fetch_add(1, Ordering::Relaxed);
                    return Err(error.into());
                }
                row_count = ledger_row_count(&mut transaction).await?;
            }
            if row_count >= self.config.row_capacity {
                self.metrics
                    .capacity_rejections
                    .fetch_add(1, Ordering::Relaxed);
                transaction.commit().await?;
                return Err(CommandLedgerError::Capacity {
                    capacity: self.config.row_capacity,
                });
            }
        }

        sqlx::query(
            "INSERT INTO io_command_ledger
                (command_id, channel_id, digest, state, expires_at_ms, received_at_ms, updated_at_ms)
             VALUES (?, ?, ?, 'received', ?, ?, ?)",
        )
        .bind(&command_id_text)
        .bind(channel_id.map(i64::from))
        .bind(digest.as_slice())
        .bind(expires_at_ms)
        .bind(now_ms)
        .bind(now_ms)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;

        Ok(CommandLedgerAdmission::New(CommandLedgerRecord {
            command_id,
            channel_id,
            digest,
            state: CommandLedgerState::Received,
            expires_at,
            received_at: TimestampMs::new(now_ms as u64),
            accepted_at: None,
            updated_at: TimestampMs::new(now_ms as u64),
            diagnostic: None,
        }))
    }

    /// Applies one legal lifecycle edge only when the durable state equals `expected`.
    pub async fn transition(
        &self,
        command_id: CommandId,
        expected: CommandLedgerState,
        next: CommandLedgerState,
    ) -> Result<CommandLedgerTransition, CommandLedgerError> {
        if !expected.can_transition_to(next) {
            return Err(CommandLedgerError::InvalidTransition {
                from: expected,
                to: next,
            });
        }

        self.transition_with_diagnostic(command_id, expected, next, None)
            .await
    }

    /// Applies a legal lifecycle edge with a bounded durable diagnostic.
    pub async fn transition_with_diagnostic(
        &self,
        command_id: CommandId,
        expected: CommandLedgerState,
        next: CommandLedgerState,
        diagnostic: Option<&str>,
    ) -> Result<CommandLedgerTransition, CommandLedgerError> {
        if !expected.can_transition_to(next) {
            return Err(CommandLedgerError::InvalidTransition {
                from: expected,
                to: next,
            });
        }
        let diagnostic =
            diagnostic.map(|value| truncate_diagnostic(value, MAX_COMMAND_LEDGER_DIAGNOSTIC_BYTES));
        let command_id_text = command_id_text(command_id);
        let now_ms = system_time_ms()?;
        let mut transaction = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let updated = sqlx::query_as::<_, StoredCommandLedgerRecord>(
            "UPDATE io_command_ledger
             SET state = ?, updated_at_ms = MAX(updated_at_ms, ?), diagnostic = ?
             WHERE command_id = ? AND state = ?
             RETURNING command_id, channel_id, digest, state, expires_at_ms, received_at_ms,
                       accepted_at_ms, updated_at_ms, diagnostic",
        )
        .bind(next.as_str())
        .bind(now_ms)
        .bind(diagnostic)
        .bind(&command_id_text)
        .bind(expected.as_str())
        .fetch_optional(&mut *transaction)
        .await?;

        let outcome = match updated {
            Some(record) => CommandLedgerTransition::Updated(record.into_typed()?),
            None => match load_stored_record(&mut transaction, &command_id_text).await? {
                Some(record) => CommandLedgerTransition::NotUpdated(record.into_typed()?),
                None => CommandLedgerTransition::Missing,
            },
        };
        transaction.commit().await?;
        Ok(outcome)
    }

    /// Durably marks queue admission after the reserved permit has been sent.
    ///
    /// The channel task may already have advanced beyond `queued`, so this
    /// operation accepts every post-queue state. `received` remains
    /// non-markable because no command has crossed the enqueue boundary.
    pub async fn mark_accepted(
        &self,
        command_id: CommandId,
    ) -> Result<CommandLedgerAcceptance, CommandLedgerError> {
        let command_id_text = command_id_text(command_id);
        let now_ms = system_time_ms()?;
        let mut transaction = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let updated = sqlx::query_as::<_, StoredCommandLedgerRecord>(
            "UPDATE io_command_ledger
             SET accepted_at_ms = MAX(received_at_ms, ?),
                 updated_at_ms = MAX(updated_at_ms, received_at_ms, ?)
             WHERE command_id = ?
               AND accepted_at_ms IS NULL
               AND state IN (
                   'queued','dispatching','succeeded','failed','expired','possibly_applied'
               )
             RETURNING command_id, channel_id, digest, state, expires_at_ms, received_at_ms,
                       accepted_at_ms, updated_at_ms, diagnostic",
        )
        .bind(now_ms)
        .bind(now_ms)
        .bind(&command_id_text)
        .fetch_optional(&mut *transaction)
        .await?;

        let outcome = match updated {
            Some(record) => CommandLedgerAcceptance::Marked(record.into_typed()?),
            None => match load_stored_record(&mut transaction, &command_id_text).await? {
                Some(record) => {
                    let record = record.into_typed()?;
                    if record.accepted_at().is_some() {
                        CommandLedgerAcceptance::AlreadyMarked(record)
                    } else {
                        CommandLedgerAcceptance::NotMarkable(record)
                    }
                },
                None => CommandLedgerAcceptance::Missing,
            },
        };
        transaction.commit().await?;
        Ok(outcome)
    }

    /// Conservatively closes commands owned by a channel whose task has
    /// stopped, whether normally, by panic, or by forced abort. No command is replayed.
    pub async fn reconcile_stopped_channel(
        &self,
        channel_id: u32,
    ) -> Result<CommandLedgerReconcile, CommandLedgerError> {
        let now_ms = system_time_ms()?;
        let mut transaction = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let queued = sqlx::query(
            "UPDATE io_command_ledger
             SET state = 'failed', updated_at_ms = MAX(updated_at_ms, ?),
                 diagnostic = 'channel task exited before queued command reached device dispatch; command was not replayed'
             WHERE channel_id = ? AND state = 'queued'",
        )
        .bind(now_ms)
        .bind(i64::from(channel_id))
        .execute(&mut *transaction)
        .await?;
        let dispatching = sqlx::query(
            "UPDATE io_command_ledger
             SET state = 'possibly_applied', updated_at_ms = MAX(updated_at_ms, ?),
                 diagnostic = 'channel task stopped after device dispatch began; physical outcome is unknown; command was not replayed'
             WHERE channel_id = ? AND state = 'dispatching'",
        )
        .bind(now_ms)
        .bind(i64::from(channel_id))
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(CommandLedgerReconcile {
            queued_failed: queued.rows_affected(),
            dispatching_possibly_applied: dispatching.rows_affected(),
        })
    }

    /// Reads one command without mutating or replaying it.
    pub async fn query(
        &self,
        command_id: CommandId,
    ) -> Result<Option<CommandLedgerRecord>, CommandLedgerError> {
        let command_id_text = command_id_text(command_id);
        sqlx::query_as::<_, StoredCommandLedgerRecord>(
            "SELECT command_id, channel_id, digest, state, expires_at_ms, received_at_ms,
                    accepted_at_ms, updated_at_ms, diagnostic
             FROM io_command_ledger WHERE command_id = ?",
        )
        .bind(command_id_text)
        .fetch_optional(&self.pool)
        .await?
        .map(StoredCommandLedgerRecord::into_typed)
        .transpose()
    }

    /// Returns a bounded aggregate suitable for health/status reporting.
    pub async fn stats(&self) -> Result<CommandLedgerStats, CommandLedgerError> {
        let rows: Vec<(String, i64)> =
            sqlx::query_as("SELECT state, COUNT(*) FROM io_command_ledger GROUP BY state")
                .fetch_all(&self.pool)
                .await?;
        let mut stats = CommandLedgerStats {
            capacity: self.config.row_capacity,
            ..CommandLedgerStats::default()
        };
        for (state, count) in rows {
            let count = u64::try_from(count)
                .map_err(|_| CommandLedgerError::Corrupt("negative state row count"))?;
            stats.total = stats.total.saturating_add(count);
            match CommandLedgerState::from_stored(&state)? {
                CommandLedgerState::Received => stats.received = count,
                CommandLedgerState::Queued => stats.queued = count,
                CommandLedgerState::Dispatching => stats.dispatching = count,
                CommandLedgerState::Succeeded => stats.succeeded = count,
                CommandLedgerState::Failed => stats.failed = count,
                CommandLedgerState::Expired => stats.expired = count,
                CommandLedgerState::PossiblyApplied => stats.possibly_applied = count,
            }
        }
        let (nonterminal, terminal): (Option<i64>, Option<i64>) = sqlx::query_as(
            "SELECT
                MIN(CASE WHEN state IN ('received','queued','dispatching') THEN updated_at_ms END),
                MIN(CASE WHEN state IN ('succeeded','failed','expired','possibly_applied') THEN updated_at_ms END)
             FROM io_command_ledger",
        )
        .fetch_one(&self.pool)
        .await?;
        stats.oldest_nonterminal_updated_at_ms =
            optional_stored_timestamp(nonterminal, "negative oldest nonterminal updated_at_ms")?;
        stats.oldest_terminal_updated_at_ms =
            optional_stored_timestamp(terminal, "negative oldest terminal updated_at_ms")?;
        stats.capacity_rejections = self.metrics.capacity_rejections.load(Ordering::Relaxed);
        stats.cleanup_failures = self.metrics.cleanup_failures.load(Ordering::Relaxed);
        stats.outcome_persistence_failures = self
            .metrics
            .outcome_persistence_failures
            .load(Ordering::Relaxed);
        stats.outcome_persistence_pending = self
            .metrics
            .outcome_persistence_pending
            .load(Ordering::Relaxed);
        Ok(stats)
    }

    /// Removes at most `limit` expired terminal rows after the requested retention period.
    pub async fn cleanup(
        &self,
        retention: Duration,
        limit: u32,
    ) -> Result<u64, CommandLedgerError> {
        if limit == 0 {
            return Ok(0);
        }
        let now_ms = system_time_ms()?;
        let Some(cutoff) = cleanup_cutoff(now_ms, retention) else {
            return Ok(0);
        };
        let mut transaction = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let removed = match delete_terminal_rows(&mut transaction, cutoff, limit).await {
            Ok(removed) => removed,
            Err(error) => {
                self.metrics
                    .cleanup_failures
                    .fetch_add(1, Ordering::Relaxed);
                return Err(error.into());
            },
        };
        transaction.commit().await?;
        Ok(removed)
    }
}

#[derive(Debug, sqlx::FromRow)]
struct StoredCommandLedgerRecord {
    command_id: String,
    channel_id: Option<i64>,
    digest: Vec<u8>,
    state: String,
    expires_at_ms: i64,
    received_at_ms: i64,
    accepted_at_ms: Option<i64>,
    updated_at_ms: i64,
    diagnostic: Option<String>,
}

impl StoredCommandLedgerRecord {
    fn into_typed(self) -> Result<CommandLedgerRecord, CommandLedgerError> {
        if self.command_id.len() != 32
            || self
                .command_id
                .bytes()
                .any(|byte| !byte.is_ascii_hexdigit() || byte.is_ascii_uppercase())
        {
            return Err(CommandLedgerError::Corrupt(
                "command_id is not 32 lowercase hexadecimal characters",
            ));
        }
        let command_id = u128::from_str_radix(&self.command_id, 16).map_err(|_| {
            CommandLedgerError::Corrupt("command_id is not a valid hexadecimal integer")
        })?;
        let channel_id = self
            .channel_id
            .map(|value| {
                u32::try_from(value)
                    .ok()
                    .filter(|value| (1..=9_999).contains(value))
                    .ok_or(CommandLedgerError::Corrupt(
                        "channel_id is outside the supported range",
                    ))
            })
            .transpose()?;
        let digest = self
            .digest
            .try_into()
            .map_err(|_| CommandLedgerError::Corrupt("command digest is not exactly 32 bytes"))?;
        let expires_at = stored_timestamp(self.expires_at_ms, "negative expires_at_ms")?;
        let received_at = stored_timestamp(self.received_at_ms, "negative received_at_ms")?;
        let accepted_at = self
            .accepted_at_ms
            .map(|value| stored_timestamp(value, "negative accepted_at_ms"))
            .transpose()?;
        let updated_at = stored_timestamp(self.updated_at_ms, "negative updated_at_ms")?;
        if updated_at.get() < received_at.get() {
            return Err(CommandLedgerError::Corrupt(
                "updated_at_ms precedes received_at_ms",
            ));
        }
        if accepted_at.is_some_and(|accepted| accepted.get() < received_at.get()) {
            return Err(CommandLedgerError::Corrupt(
                "accepted_at_ms precedes received_at_ms",
            ));
        }
        if self
            .diagnostic
            .as_ref()
            .is_some_and(|diagnostic| diagnostic.len() > MAX_COMMAND_LEDGER_DIAGNOSTIC_BYTES)
        {
            return Err(CommandLedgerError::Corrupt(
                "command diagnostic exceeds the UTF-8 byte limit",
            ));
        }
        Ok(CommandLedgerRecord {
            command_id: CommandId::new(command_id),
            channel_id,
            digest,
            state: CommandLedgerState::from_stored(&self.state)?,
            expires_at,
            received_at,
            accepted_at,
            updated_at,
            diagnostic: self.diagnostic,
        })
    }
}

async fn load_stored_record(
    transaction: &mut Transaction<'_, Sqlite>,
    command_id: &str,
) -> Result<Option<StoredCommandLedgerRecord>, sqlx::Error> {
    sqlx::query_as::<_, StoredCommandLedgerRecord>(
        "SELECT command_id, channel_id, digest, state, expires_at_ms, received_at_ms,
                accepted_at_ms, updated_at_ms, diagnostic
         FROM io_command_ledger WHERE command_id = ?",
    )
    .bind(command_id)
    .fetch_optional(&mut **transaction)
    .await
}

fn truncate_diagnostic(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

async fn ledger_row_count(
    transaction: &mut Transaction<'_, Sqlite>,
) -> Result<u64, CommandLedgerError> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM io_command_ledger")
        .fetch_one(&mut **transaction)
        .await?;
    u64::try_from(count)
        .map_err(|_| CommandLedgerError::Corrupt("command ledger row count is negative"))
}

async fn delete_terminal_rows(
    transaction: &mut Transaction<'_, Sqlite>,
    cutoff: i64,
    limit: u32,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "DELETE FROM io_command_ledger
         WHERE command_id IN (
             SELECT command_id FROM io_command_ledger
             WHERE state IN ('succeeded', 'failed', 'expired', 'possibly_applied')
               AND expires_at_ms <= ?
             ORDER BY expires_at_ms, updated_at_ms, command_id
             LIMIT ?
         )",
    )
    .bind(cutoff)
    .bind(i64::from(limit))
    .execute(&mut **transaction)
    .await?;
    Ok(result.rows_affected())
}

fn command_id_text(command_id: CommandId) -> String {
    format!("{:032x}", command_id.get())
}

fn sqlite_timestamp(
    timestamp: TimestampMs,
    field: &'static str,
) -> Result<i64, CommandLedgerError> {
    i64::try_from(timestamp.get()).map_err(|_| CommandLedgerError::InvalidTimestamp(field))
}

fn stored_timestamp(
    timestamp: i64,
    failure: &'static str,
) -> Result<TimestampMs, CommandLedgerError> {
    u64::try_from(timestamp)
        .map(TimestampMs::new)
        .map_err(|_| CommandLedgerError::Corrupt(failure))
}

fn optional_stored_timestamp(
    timestamp: Option<i64>,
    failure: &'static str,
) -> Result<Option<u64>, CommandLedgerError> {
    timestamp
        .map(|value| u64::try_from(value).map_err(|_| CommandLedgerError::Corrupt(failure)))
        .transpose()
}

fn system_time_ms() -> Result<i64, CommandLedgerError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CommandLedgerError::InvalidTimestamp("system clock precedes UNIX epoch"))?;
    i64::try_from(elapsed.as_millis())
        .map_err(|_| CommandLedgerError::InvalidTimestamp("system clock"))
}

/// Converts `now >= expires + retention` into `expires <= now - retention`.
/// Subtraction avoids overflow at the upper end of the persisted timestamp range.
fn cleanup_cutoff(now_ms: i64, retention: Duration) -> Option<i64> {
    let now_ms = u128::try_from(now_ms).ok()?;
    let cutoff = now_ms.checked_sub(retention.as_millis())?;
    i64::try_from(cutoff).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn test_ledger(capacity: u64, retention: Duration) -> (SqlitePool, CommandLedger) {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open in-memory ledger");
        let ledger = CommandLedger::initialize_with_config(
            pool.clone(),
            CommandLedgerConfig::new(capacity, retention, 16),
        )
        .await
        .expect("initialize ledger");
        (pool, ledger)
    }

    fn command(value: u128) -> CommandId {
        CommandId::new(value)
    }

    fn digest(value: u8) -> [u8; 32] {
        [value; 32]
    }

    fn record(outcome: CommandLedgerAdmission) -> CommandLedgerRecord {
        match outcome {
            CommandLedgerAdmission::New(record) | CommandLedgerAdmission::Same(record) => record,
            CommandLedgerAdmission::Conflict => panic!("expected admitted command record"),
        }
    }

    async fn transition(
        ledger: &CommandLedger,
        id: CommandId,
        expected: CommandLedgerState,
        next: CommandLedgerState,
    ) -> CommandLedgerRecord {
        match ledger
            .transition(id, expected, next)
            .await
            .expect("transition command")
        {
            CommandLedgerTransition::Updated(record) => record,
            other => panic!("expected updated command, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn same_identity_and_digest_is_idempotent_but_different_digest_conflicts() {
        let (_pool, ledger) = test_ledger(10, Duration::ZERO).await;
        let id = command(0xabc);
        let first = record(
            ledger
                .admit(id, digest(1), TimestampMs::new(u64::MAX >> 1))
                .await
                .expect("admit command"),
        );

        let same = ledger
            .admit(id, digest(1), TimestampMs::new(1))
            .await
            .expect("repeat exact command");
        assert_eq!(same, CommandLedgerAdmission::Same(first.clone()));
        assert_eq!(
            ledger
                .admit(id, digest(2), TimestampMs::new(1))
                .await
                .expect("classify conflicting command"),
            CommandLedgerAdmission::Conflict
        );
        assert_eq!(ledger.query(id).await.expect("query command"), Some(first));
    }

    #[tokio::test]
    async fn quota_fails_closed_when_only_protected_rows_exist() {
        let (_pool, ledger) = test_ledger(1, Duration::ZERO).await;
        let protected_until = system_time_ms().expect("clock") as u64 + 60_000;
        ledger
            .admit(command(1), digest(1), TimestampMs::new(protected_until))
            .await
            .expect("fill ledger");

        let error = ledger
            .admit(command(2), digest(2), TimestampMs::new(protected_until))
            .await
            .expect_err("nonterminal row must not be evicted");
        assert!(matches!(
            error,
            CommandLedgerError::Capacity { capacity: 1 }
        ));
        assert!(
            ledger
                .query(command(1))
                .await
                .expect("query first")
                .is_some()
        );
        assert!(
            ledger
                .query(command(2))
                .await
                .expect("query second")
                .is_none()
        );
        let stats = ledger.stats().await.expect("query ledger stats");
        assert_eq!(stats.total, 1);
        assert_eq!(stats.received, 1);
        assert_eq!(stats.capacity_rejections, 1);
    }

    #[tokio::test]
    async fn cleanup_is_bounded_and_protects_nonterminal_and_retained_rows() {
        let (_pool, ledger) = test_ledger(10, Duration::ZERO).await;
        let now = system_time_ms().expect("clock") as u64;
        let ancient_nonterminal = command(1);
        let retained_terminal = command(2);
        let eligible_terminal = command(3);

        ledger
            .admit(ancient_nonterminal, digest(1), TimestampMs::new(0))
            .await
            .expect("admit nonterminal");
        ledger
            .admit(
                retained_terminal,
                digest(2),
                TimestampMs::new(now.saturating_sub(100)),
            )
            .await
            .expect("admit retained terminal");
        transition(
            &ledger,
            retained_terminal,
            CommandLedgerState::Received,
            CommandLedgerState::Failed,
        )
        .await;
        ledger
            .admit(eligible_terminal, digest(3), TimestampMs::new(0))
            .await
            .expect("admit eligible terminal");
        transition(
            &ledger,
            eligible_terminal,
            CommandLedgerState::Received,
            CommandLedgerState::Failed,
        )
        .await;

        assert_eq!(
            ledger
                .cleanup(Duration::from_secs(60), 1)
                .await
                .expect("bounded cleanup"),
            1
        );
        assert!(
            ledger
                .query(ancient_nonterminal)
                .await
                .expect("query nonterminal")
                .is_some()
        );
        assert!(
            ledger
                .query(retained_terminal)
                .await
                .expect("query retained")
                .is_some()
        );
        assert!(
            ledger
                .query(eligible_terminal)
                .await
                .expect("query eligible")
                .is_none()
        );
    }

    #[tokio::test]
    async fn capacity_cleanup_admits_after_an_expired_terminal_row() {
        let (_pool, ledger) = test_ledger(1, Duration::ZERO).await;
        let old = command(1);
        ledger
            .admit(old, digest(1), TimestampMs::new(0))
            .await
            .expect("admit old command");
        transition(
            &ledger,
            old,
            CommandLedgerState::Received,
            CommandLedgerState::Failed,
        )
        .await;

        assert!(matches!(
            ledger
                .admit(command(2), digest(2), TimestampMs::new(1))
                .await
                .expect("cleanup and admit replacement"),
            CommandLedgerAdmission::New(_)
        ));
        assert!(ledger.query(old).await.expect("query old").is_none());
    }

    #[tokio::test]
    async fn stats_expose_every_state_oldest_timestamps_and_capacity_rejections() {
        let (pool, ledger) = test_ledger(7, Duration::ZERO).await;
        let expires_at = TimestampMs::new(u64::MAX >> 1);
        for value in 1_u8..=7 {
            ledger
                .admit(command(u128::from(value)), digest(value), expires_at)
                .await
                .expect("admit stats fixture");
        }

        transition(
            &ledger,
            command(2),
            CommandLedgerState::Received,
            CommandLedgerState::Queued,
        )
        .await;
        transition(
            &ledger,
            command(3),
            CommandLedgerState::Received,
            CommandLedgerState::Queued,
        )
        .await;
        transition(
            &ledger,
            command(3),
            CommandLedgerState::Queued,
            CommandLedgerState::Dispatching,
        )
        .await;
        transition(
            &ledger,
            command(4),
            CommandLedgerState::Received,
            CommandLedgerState::Queued,
        )
        .await;
        transition(
            &ledger,
            command(4),
            CommandLedgerState::Queued,
            CommandLedgerState::Dispatching,
        )
        .await;
        transition(
            &ledger,
            command(4),
            CommandLedgerState::Dispatching,
            CommandLedgerState::Succeeded,
        )
        .await;
        transition(
            &ledger,
            command(5),
            CommandLedgerState::Received,
            CommandLedgerState::Failed,
        )
        .await;
        transition(
            &ledger,
            command(6),
            CommandLedgerState::Received,
            CommandLedgerState::Expired,
        )
        .await;
        transition(
            &ledger,
            command(7),
            CommandLedgerState::Received,
            CommandLedgerState::Queued,
        )
        .await;
        transition(
            &ledger,
            command(7),
            CommandLedgerState::Queued,
            CommandLedgerState::Dispatching,
        )
        .await;
        transition(
            &ledger,
            command(7),
            CommandLedgerState::Dispatching,
            CommandLedgerState::PossiblyApplied,
        )
        .await;

        sqlx::query(
            "UPDATE io_command_ledger SET received_at_ms = 101, updated_at_ms = 101
             WHERE command_id = ?",
        )
        .bind(command_id_text(command(1)))
        .execute(&pool)
        .await
        .expect("set oldest nonterminal timestamp");
        sqlx::query(
            "UPDATE io_command_ledger SET received_at_ms = 202, updated_at_ms = 202
             WHERE command_id = ?",
        )
        .bind(command_id_text(command(5)))
        .execute(&pool)
        .await
        .expect("set oldest terminal timestamp");

        assert!(matches!(
            ledger.admit(command(8), digest(8), expires_at).await,
            Err(CommandLedgerError::Capacity { capacity: 7 })
        ));
        let stats = ledger.stats().await.expect("query all-state stats");
        assert_eq!(stats.total, 7);
        assert_eq!(stats.capacity, 7);
        assert_eq!(stats.received, 1);
        assert_eq!(stats.queued, 1);
        assert_eq!(stats.dispatching, 1);
        assert_eq!(stats.succeeded, 1);
        assert_eq!(stats.failed, 1);
        assert_eq!(stats.expired, 1);
        assert_eq!(stats.possibly_applied, 1);
        assert_eq!(stats.oldest_nonterminal_updated_at_ms, Some(101));
        assert_eq!(stats.oldest_terminal_updated_at_ms, Some(202));
        assert_eq!(stats.capacity_rejections, 1);
    }

    #[tokio::test]
    async fn terminal_diagnostic_is_bounded_by_utf8_bytes_without_splitting_a_scalar() {
        let (pool, ledger) = test_ledger(10, Duration::ZERO).await;
        let id = command(0xd1a6);
        ledger
            .admit(id, digest(1), TimestampMs::new(u64::MAX >> 1))
            .await
            .expect("admit diagnostic fixture");
        let oversized = "界".repeat(300);
        let updated = ledger
            .transition_with_diagnostic(
                id,
                CommandLedgerState::Received,
                CommandLedgerState::Failed,
                Some(&oversized),
            )
            .await
            .expect("persist bounded terminal diagnostic");
        let record = match updated {
            CommandLedgerTransition::Updated(record) => record,
            other => panic!("expected updated command, got {other:?}"),
        };
        let diagnostic = record.diagnostic().expect("terminal diagnostic");
        assert_eq!(diagnostic.len(), 510);
        assert!(diagnostic.len() <= MAX_COMMAND_LEDGER_DIAGNOSTIC_BYTES);
        assert_eq!(diagnostic, "界".repeat(170));

        let stored_bytes: i64 = sqlx::query_scalar(
            "SELECT length(CAST(diagnostic AS BLOB)) FROM io_command_ledger WHERE command_id = ?",
        )
        .bind(command_id_text(id))
        .fetch_one(&pool)
        .await
        .expect("read stored diagnostic byte length");
        assert_eq!(stored_bytes, 510);
    }

    #[tokio::test]
    async fn startup_recovery_classifies_each_ambiguous_nonterminal_state_without_replay() {
        let (pool, ledger) = test_ledger(10, Duration::ZERO).await;
        let received = command(1);
        let queued = command(2);
        let dispatching = command(3);
        for (id, marker) in [(received, 1), (queued, 2), (dispatching, 3)] {
            ledger
                .admit(id, digest(marker), TimestampMs::new(u64::MAX >> 1))
                .await
                .expect("admit recovery fixture");
        }
        transition(
            &ledger,
            queued,
            CommandLedgerState::Received,
            CommandLedgerState::Queued,
        )
        .await;
        transition(
            &ledger,
            dispatching,
            CommandLedgerState::Received,
            CommandLedgerState::Queued,
        )
        .await;
        transition(
            &ledger,
            dispatching,
            CommandLedgerState::Queued,
            CommandLedgerState::Dispatching,
        )
        .await;
        drop(ledger);

        let reopened = CommandLedger::initialize_with_config(
            pool,
            CommandLedgerConfig::new(10, Duration::ZERO, 16),
        )
        .await
        .expect("reopen and recover ledger");
        let recovered_received = reopened
            .query(received)
            .await
            .expect("query received")
            .expect("received recovery row");
        assert_eq!(recovered_received.state(), CommandLedgerState::Failed);
        assert_eq!(
            recovered_received.diagnostic(),
            Some(RESTART_RECEIVED_DIAGNOSTIC)
        );

        for (id, marker) in [(queued, 2), (dispatching, 3)] {
            let recovered = reopened
                .query(id)
                .await
                .expect("query ambiguous command")
                .expect("ambiguous recovery row");
            assert_eq!(recovered.digest(), &digest(marker));
            assert_eq!(recovered.state(), CommandLedgerState::PossiblyApplied);
            assert_eq!(recovered.diagnostic(), Some(RESTART_AMBIGUOUS_DIAGNOSTIC));
        }

        let stats = reopened.stats().await.expect("query recovery stats");
        assert_eq!(stats.total, 3);
        assert_eq!(stats.received, 0);
        assert_eq!(stats.queued, 0);
        assert_eq!(stats.dispatching, 0);
        assert_eq!(stats.failed, 1);
        assert_eq!(stats.possibly_applied, 2);
        assert!(matches!(
            reopened
                .admit(received, digest(1), TimestampMs::new(0))
                .await
                .expect("same identity remains classified after restart"),
            CommandLedgerAdmission::Same(record)
                if record.state() == CommandLedgerState::Failed
                    && record.diagnostic() == Some(RESTART_RECEIVED_DIAGNOSTIC)
        ));
        assert_eq!(
            reopened
                .stats()
                .await
                .expect("query post-retry stats")
                .total,
            3,
            "recovery and exact retries must never enqueue or insert a replay"
        );
    }

    #[tokio::test]
    async fn transition_is_legal_conditional_and_observable() {
        let (_pool, ledger) = test_ledger(10, Duration::ZERO).await;
        let id = command(9);
        ledger
            .admit(id, digest(9), TimestampMs::new(u64::MAX >> 1))
            .await
            .expect("admit command");

        let wrong_expected = ledger
            .transition(
                id,
                CommandLedgerState::Queued,
                CommandLedgerState::Dispatching,
            )
            .await
            .expect("conditional transition result");
        assert!(matches!(
            wrong_expected,
            CommandLedgerTransition::NotUpdated(ref record)
                if record.state() == CommandLedgerState::Received
        ));

        let illegal = ledger
            .transition(
                id,
                CommandLedgerState::Received,
                CommandLedgerState::Succeeded,
            )
            .await
            .expect_err("illegal edge must fail before storage mutation");
        assert!(matches!(
            illegal,
            CommandLedgerError::InvalidTransition {
                from: CommandLedgerState::Received,
                to: CommandLedgerState::Succeeded,
            }
        ));

        transition(
            &ledger,
            id,
            CommandLedgerState::Received,
            CommandLedgerState::Queued,
        )
        .await;
        transition(
            &ledger,
            id,
            CommandLedgerState::Queued,
            CommandLedgerState::Dispatching,
        )
        .await;
        let dispatch_failure = ledger
            .transition(
                id,
                CommandLedgerState::Dispatching,
                CommandLedgerState::Failed,
            )
            .await
            .expect_err("a post-dispatch outcome must remain conservative");
        assert!(matches!(
            dispatch_failure,
            CommandLedgerError::InvalidTransition {
                from: CommandLedgerState::Dispatching,
                to: CommandLedgerState::Failed,
            }
        ));
    }

    #[tokio::test]
    async fn queue_acceptance_is_distinct_and_can_follow_concurrent_terminal_progress() {
        let (_pool, ledger) = test_ledger(10, Duration::ZERO).await;
        let id = command(0xa11);
        let received = record(
            ledger
                .admit_for_channel(id, 7, digest(1), TimestampMs::new(u64::MAX >> 1))
                .await
                .expect("admit channel command"),
        );
        assert_eq!(received.channel_id(), Some(7));
        assert_eq!(received.accepted_at(), None);
        assert!(matches!(
            ledger.mark_accepted(id).await.expect("not markable result"),
            CommandLedgerAcceptance::NotMarkable(record)
                if record.state() == CommandLedgerState::Received
        ));

        transition(
            &ledger,
            id,
            CommandLedgerState::Received,
            CommandLedgerState::Queued,
        )
        .await;
        transition(
            &ledger,
            id,
            CommandLedgerState::Queued,
            CommandLedgerState::Dispatching,
        )
        .await;
        transition(
            &ledger,
            id,
            CommandLedgerState::Dispatching,
            CommandLedgerState::Succeeded,
        )
        .await;

        let marked = match ledger.mark_accepted(id).await.expect("mark accepted") {
            CommandLedgerAcceptance::Marked(record) => record,
            other => panic!("expected first acceptance marker, got {other:?}"),
        };
        assert_eq!(marked.state(), CommandLedgerState::Succeeded);
        assert!(marked.accepted_at().is_some());
        assert!(matches!(
            ledger.mark_accepted(id).await.expect("idempotent marker"),
            CommandLedgerAcceptance::AlreadyMarked(record)
                if record.accepted_at() == marked.accepted_at()
        ));
    }

    #[tokio::test]
    async fn forced_channel_abort_reconciles_queued_and_dispatching_for_only_that_channel() {
        let (_pool, ledger) = test_ledger(10, Duration::ZERO).await;
        for (id, channel_id) in [(command(1), 7), (command(2), 7), (command(3), 8)] {
            ledger
                .admit_for_channel(
                    id,
                    channel_id,
                    digest(channel_id as u8),
                    TimestampMs::new(u64::MAX >> 1),
                )
                .await
                .expect("admit channel command");
            transition(
                &ledger,
                id,
                CommandLedgerState::Received,
                CommandLedgerState::Queued,
            )
            .await;
            if id != command(1) {
                transition(
                    &ledger,
                    id,
                    CommandLedgerState::Queued,
                    CommandLedgerState::Dispatching,
                )
                .await;
            }
        }

        let reconciled = ledger
            .reconcile_stopped_channel(7)
            .await
            .expect("reconcile channel");
        assert_eq!(reconciled.queued_failed, 1);
        assert_eq!(reconciled.dispatching_possibly_applied, 1);
        let queued = ledger.query(command(1)).await.unwrap().unwrap();
        let dispatching = ledger.query(command(2)).await.unwrap().unwrap();
        let other_channel = ledger.query(command(3)).await.unwrap().unwrap();
        assert_eq!(queued.state(), CommandLedgerState::Failed);
        assert_eq!(dispatching.state(), CommandLedgerState::PossiblyApplied);
        assert!(
            dispatching
                .diagnostic()
                .is_some_and(|value| value.contains("task stopped"))
        );
        assert_eq!(other_channel.state(), CommandLedgerState::Dispatching);
    }

    #[tokio::test]
    async fn acceptance_marker_tolerates_fast_ambiguous_device_outcome() {
        let (_pool, ledger) = test_ledger(10, Duration::ZERO).await;
        let id = command(0xa12);
        ledger
            .admit_for_channel(id, 7, digest(1), TimestampMs::new(u64::MAX >> 1))
            .await
            .expect("admit command");
        transition(
            &ledger,
            id,
            CommandLedgerState::Received,
            CommandLedgerState::Queued,
        )
        .await;
        transition(
            &ledger,
            id,
            CommandLedgerState::Queued,
            CommandLedgerState::Dispatching,
        )
        .await;
        transition(
            &ledger,
            id,
            CommandLedgerState::Dispatching,
            CommandLedgerState::PossiblyApplied,
        )
        .await;

        assert!(matches!(
            ledger.mark_accepted(id).await.expect("mark accepted"),
            CommandLedgerAcceptance::Marked(record)
                if record.state() == CommandLedgerState::PossiblyApplied
                    && record.accepted_at().is_some()
        ));
    }

    type LedgerRowSnapshot = (
        String,
        Option<i64>,
        Vec<u8>,
        String,
        i64,
        i64,
        Option<i64>,
        i64,
        Option<String>,
    );

    async fn assert_incompatible_table_is_untouched(create_sql: &str) {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open incompatible ledger");
        sqlx::query(create_sql)
            .execute(&pool)
            .await
            .expect("create incompatible schema");
        sqlx::query(
            "INSERT INTO io_command_ledger (
                command_id, channel_id, digest, state, expires_at_ms,
                received_at_ms, accepted_at_ms, updated_at_ms, diagnostic
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind("0000000000000000000000000000feed")
        .bind(7_i64)
        .bind(vec![9_u8; 32])
        .bind("received")
        .bind(999_i64)
        .bind(11_i64)
        .bind(Option::<i64>::None)
        .bind(17_i64)
        .bind("sentinel")
        .execute(&pool)
        .await
        .expect("insert sentinel row");

        let schema_before: String =
            sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?")
                .bind(COMMAND_LEDGER_TABLE_NAME)
                .fetch_one(&pool)
                .await
                .expect("query schema before initialization");
        let rows_before: Vec<LedgerRowSnapshot> = sqlx::query_as(
            "SELECT command_id, channel_id, digest, state, expires_at_ms,
                    received_at_ms, accepted_at_ms, updated_at_ms, diagnostic
             FROM io_command_ledger ORDER BY command_id",
        )
        .fetch_all(&pool)
        .await
        .expect("query rows before initialization");
        let indexes_before: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT name, sql FROM sqlite_master
             WHERE type = 'index' AND tbl_name = ? ORDER BY name",
        )
        .bind(COMMAND_LEDGER_TABLE_NAME)
        .fetch_all(&pool)
        .await
        .expect("query indexes before initialization");

        let error = match CommandLedger::initialize(pool.clone()).await {
            Ok(_) => panic!("incompatible schema must fail closed"),
            Err(error) => error,
        };
        assert!(matches!(error, CommandLedgerError::Corrupt(_)));

        let schema_after: String =
            sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?")
                .bind(COMMAND_LEDGER_TABLE_NAME)
                .fetch_one(&pool)
                .await
                .expect("query schema after initialization");
        let rows_after: Vec<LedgerRowSnapshot> = sqlx::query_as(
            "SELECT command_id, channel_id, digest, state, expires_at_ms,
                    received_at_ms, accepted_at_ms, updated_at_ms, diagnostic
             FROM io_command_ledger ORDER BY command_id",
        )
        .fetch_all(&pool)
        .await
        .expect("query rows after initialization");
        let indexes_after: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT name, sql FROM sqlite_master
             WHERE type = 'index' AND tbl_name = ? ORDER BY name",
        )
        .bind(COMMAND_LEDGER_TABLE_NAME)
        .fetch_all(&pool)
        .await
        .expect("query indexes after initialization");

        assert_eq!(schema_after, schema_before, "table definition was modified");
        assert_eq!(rows_after, rows_before, "sentinel data was modified");
        assert_eq!(indexes_after, indexes_before, "indexes were modified");
        assert!(
            indexes_after
                .iter()
                .all(|(name, _)| name != "idx_io_command_ledger_cleanup"),
            "cleanup index must not be created before schema validation"
        );
    }

    #[tokio::test]
    async fn same_columns_without_constraints_are_rejected_without_mutation() {
        assert_incompatible_table_is_untouched(
            "CREATE TABLE io_command_ledger (
                command_id TEXT,
                channel_id INTEGER,
                digest BLOB,
                state TEXT,
                expires_at_ms INTEGER,
                received_at_ms INTEGER,
                accepted_at_ms INTEGER,
                updated_at_ms INTEGER,
                diagnostic TEXT
            )",
        )
        .await;
    }

    #[tokio::test]
    async fn same_columns_with_wrong_type_are_rejected_without_mutation() {
        let wrong_definition = COMMAND_LEDGER_TABLE_DEFINITION.replacen(
            "digest         BLOB NOT NULL",
            "digest         TEXT NOT NULL",
            1,
        );
        assert_ne!(wrong_definition, COMMAND_LEDGER_TABLE_DEFINITION);
        let create_sql = format!("CREATE TABLE {COMMAND_LEDGER_TABLE_NAME} {wrong_definition}");
        assert_incompatible_table_is_untouched(&create_sql).await;
    }

    #[tokio::test]
    async fn acceptance_timestamp_clamps_to_received_time_after_clock_rollback() {
        let (pool, ledger) = test_ledger(10, Duration::ZERO).await;
        let id = command(0xc10c);
        ledger
            .admit_for_channel(id, 7, digest(1), TimestampMs::new(u64::MAX >> 1))
            .await
            .expect("admit command");
        transition(
            &ledger,
            id,
            CommandLedgerState::Received,
            CommandLedgerState::Queued,
        )
        .await;
        let future = system_time_ms().expect("clock") + 60_000;
        sqlx::query(
            "UPDATE io_command_ledger SET received_at_ms = ?, updated_at_ms = ?
             WHERE command_id = ?",
        )
        .bind(future)
        .bind(future)
        .bind(command_id_text(id))
        .execute(&pool)
        .await
        .expect("simulate clock rollback");

        let marked = match ledger.mark_accepted(id).await.expect("mark accepted") {
            CommandLedgerAcceptance::Marked(record) => record,
            other => panic!("expected marker, got {other:?}"),
        };
        assert_eq!(marked.received_at().get(), future as u64);
        assert_eq!(marked.accepted_at().unwrap().get(), future as u64);
    }
}
