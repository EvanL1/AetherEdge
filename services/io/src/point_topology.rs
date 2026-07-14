//! Governed application boundary and SQLite adapter for point topology.
//!
//! HTTP interfaces translate DTOs into the typed mutations in this module.
//! Only this adapter owns desired point-table SQL, the channel revision CAS,
//! and transaction boundaries.

use std::sync::Arc;

use aether_application::{
    ApplicationError, CompletionAuditStatus, MANAGE_CHANNEL_CAPABILITY, RequestContext,
    SafetyPolicy,
};
use aether_ports::{
    AuditOutcome, AuditRecord, AuditSink, ChannelRevision, PortError, PortErrorKind,
};
use sqlx::{SqliteConnection, SqlitePool};

const MAX_CHANNEL_ID: u32 = 10_000;

/// Physical point plane stored in SQLite and projected to SHM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointKind {
    Telemetry,
    Signal,
    Control,
    Adjustment,
}

impl PointKind {
    /// Parses the stable T/S/C/A API code.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.to_ascii_uppercase().as_str() {
            "T" => Ok(Self::Telemetry),
            "S" => Ok(Self::Signal),
            "C" => Ok(Self::Control),
            "A" => Ok(Self::Adjustment),
            _ => Err(format!(
                "Invalid point type '{value}'. Must be T, S, C, or A"
            )),
        }
    }

    /// Returns the stable API/audit code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Telemetry => "T",
            Self::Signal => "S",
            Self::Control => "C",
            Self::Adjustment => "A",
        }
    }

    const fn table(self) -> &'static str {
        match self {
            Self::Telemetry => "telemetry_points",
            Self::Signal => "signal_points",
            Self::Control => "control_points",
            Self::Adjustment => "adjustment_points",
        }
    }
}

/// Complete durable definition for one physical point.
#[derive(Debug, Clone)]
pub struct PointDefinitionMutation {
    pub point_id: u32,
    pub signal_name: String,
    pub scale: f64,
    pub offset: f64,
    pub unit: String,
    pub reverse: bool,
    pub data_type: String,
    pub description: String,
    pub normal_state: i64,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
    pub step: f64,
    /// `None` preserves an existing mapping during forced upsert; `Some(None)`
    /// clears it and `Some(Some(json))` replaces it.
    pub protocol_mapping: Option<Option<String>>,
}

/// Partial durable point update.
#[derive(Debug, Clone, Default)]
pub struct PointPatchMutation {
    pub signal_name: Option<String>,
    pub description: Option<String>,
    pub unit: Option<String>,
    pub scale: Option<f64>,
    pub offset: Option<f64>,
    pub data_type: Option<String>,
    pub reverse: Option<bool>,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
    pub step: Option<f64>,
}

impl PointPatchMutation {
    fn is_empty(&self) -> bool {
        self.signal_name.is_none()
            && self.description.is_none()
            && self.unit.is_none()
            && self.scale.is_none()
            && self.offset.is_none()
            && self.data_type.is_none()
            && self.reverse.is_none()
            && self.minimum.is_none()
            && self.maximum.is_none()
            && self.step.is_none()
    }
}

/// One protocol mapping update owned by the topology application.
#[derive(Debug, Clone)]
pub struct PointMappingMutation {
    pub kind: PointKind,
    pub point_id: u32,
    pub protocol_data: serde_json::Value,
}

/// One typed point-table mutation.
#[derive(Debug, Clone)]
pub enum PointMutation {
    Create {
        kind: PointKind,
        definition: PointDefinitionMutation,
        force: bool,
    },
    Update {
        kind: PointKind,
        point_id: u32,
        patch: PointPatchMutation,
    },
    Delete {
        kind: PointKind,
        point_id: u32,
    },
}

impl PointMutation {
    fn kind(&self) -> PointKind {
        match self {
            Self::Create { kind, .. } | Self::Update { kind, .. } | Self::Delete { kind, .. } => {
                *kind
            },
        }
    }

    fn operation(&self) -> &'static str {
        match self {
            Self::Create { .. } => "create",
            Self::Update { .. } => "update",
            Self::Delete { .. } => "delete",
        }
    }

    fn point_id(&self) -> u32 {
        match self {
            Self::Create { definition, .. } => definition.point_id,
            Self::Update { point_id, .. } | Self::Delete { point_id, .. } => *point_id,
        }
    }
}

/// One service application command and its atomicity boundary.
#[derive(Debug, Clone)]
pub enum PointTopologyMutation {
    Single {
        channel_id: u32,
        mutation: PointMutation,
    },
    Batch {
        channel_id: u32,
        mutations: Vec<PointMutation>,
    },
    Provision {
        channel_id: u32,
        replace_existing: bool,
        upsert_existing: bool,
        points: Vec<(PointKind, PointDefinitionMutation)>,
    },
    Mappings {
        channel_id: u32,
        merge: bool,
        mappings: Vec<PointMappingMutation>,
    },
}

impl PointTopologyMutation {
    /// Creates one CRUD command.
    #[must_use]
    pub const fn single(channel_id: u32, mutation: PointMutation) -> Self {
        Self::Single {
            channel_id,
            mutation,
        }
    }

    /// Returns the authoritative channel whose topology is mutated.
    #[must_use]
    pub const fn channel_id(&self) -> u32 {
        match self {
            Self::Single { channel_id, .. }
            | Self::Batch { channel_id, .. }
            | Self::Provision { channel_id, .. }
            | Self::Mappings { channel_id, .. } => *channel_id,
        }
    }

    fn point_count(&self) -> usize {
        match self {
            Self::Single { .. } => 1,
            Self::Batch { mutations, .. } => mutations.len(),
            Self::Provision { points, .. } => points.len(),
            Self::Mappings { mappings, .. } => mappings.len(),
        }
    }

    fn audit_operation(&self) -> &'static str {
        match self {
            Self::Single { mutation, .. } => mutation.operation(),
            Self::Batch { .. } => "batch",
            Self::Provision { .. } => "provision",
            Self::Mappings { .. } => "update_mappings",
        }
    }

    fn audit_point_type(&self) -> &'static str {
        match self {
            Self::Single { mutation, .. } => mutation.kind().code(),
            Self::Batch { .. } | Self::Provision { .. } => "mixed",
            Self::Mappings { .. } => "mapping",
        }
    }
}

/// Per-item receipt emitted only after an atomic batch fully succeeds.
#[derive(Debug, Clone)]
pub struct PointBatchMutationOutcome {
    pub operation: &'static str,
    pub point_type: &'static str,
    pub point_id: u32,
    pub error: Option<String>,
}

/// Typed application result consumed by HTTP and future CLI/AI interfaces.
#[derive(Debug, Clone)]
pub enum PointTopologyMutationResult {
    Single {
        signal_name: String,
    },
    Batch {
        outcomes: Vec<PointBatchMutationOutcome>,
    },
    Provisioned {
        point_count: usize,
    },
    MappingsUpdated {
        mapping_count: usize,
    },
}

/// Accepted non-idempotent point-topology command.
pub struct PointTopologyAcceptance {
    result: PointTopologyMutationResult,
    request_id: String,
    resulting_revision: ChannelRevision,
    completion_audit: CompletionAuditStatus,
}

impl PointTopologyAcceptance {
    #[must_use]
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    #[must_use]
    pub const fn resulting_revision(&self) -> ChannelRevision {
        self.resulting_revision
    }

    #[must_use]
    pub const fn completion_audit(&self) -> &CompletionAuditStatus {
        &self.completion_audit
    }

    #[must_use]
    pub fn into_result(self) -> PointTopologyMutationResult {
        self.result
    }
}

/// Service application facade over the SQLite point mutation adapter.
pub struct PointTopologyApplication {
    pool: SqlitePool,
    audit: Arc<dyn AuditSink>,
    policy: SafetyPolicy,
}

/// Capability lease issued before a point-topology command performs external I/O.
///
/// The fields are deliberately private: transports can only obtain a lease by
/// passing the same authorization and revision checks used by [`Self::mutate`].
/// Consuming the lease later preserves the captured request context and CAS
/// revision without authenticating a second time.
pub struct PointTopologyAuthorization {
    context: RequestContext,
    expected_revision: ChannelRevision,
}

impl PointTopologyApplication {
    #[must_use]
    pub fn new(pool: SqlitePool, audit: Arc<dyn AuditSink>) -> Self {
        Self {
            pool,
            audit,
            policy: SafetyPolicy,
        }
    }

    /// Authorizes external preparation work for one future mutation.
    ///
    /// This performs no audit or durable mutation. The returned lease must be
    /// consumed by [`Self::mutate_authorized`] after preparation has produced
    /// the complete typed command, which remains the single audited/CAS-fenced
    /// application command.
    pub fn preauthorize(
        &self,
        context: &RequestContext,
        expected_revision: Option<ChannelRevision>,
    ) -> Result<PointTopologyAuthorization, ApplicationError> {
        self.policy.authorize(MANAGE_CHANNEL_CAPABILITY, context)?;
        let expected_revision = validate_expected_revision(expected_revision)?;
        Ok(PointTopologyAuthorization {
            context: context.clone(),
            expected_revision,
        })
    }

    /// Authorizes, audits, CAS-fences, and applies one typed mutation.
    pub async fn mutate(
        &self,
        context: &RequestContext,
        expected_revision: Option<ChannelRevision>,
        mutation: PointTopologyMutation,
    ) -> Result<PointTopologyAcceptance, ApplicationError> {
        if let Err(error) = self.policy.authorize(MANAGE_CHANNEL_CAPABILITY, context) {
            self.record_audit(
                context,
                expected_revision,
                &mutation,
                AuditOutcome::Rejected,
                Some(error.to_string()),
            )
            .await?;
            return Err(error);
        }
        let expected_revision = match validate_command(expected_revision, &mutation) {
            Ok(revision) => revision,
            Err(error) => {
                self.record_audit(
                    context,
                    expected_revision,
                    &mutation,
                    AuditOutcome::Rejected,
                    Some(error.to_string()),
                )
                .await?;
                return Err(error);
            },
        };
        self.mutate_validated(context, expected_revision, mutation)
            .await
    }

    /// Audits, CAS-fences, and applies a command using a consumed authorization
    /// lease obtained before external preparation work.
    pub async fn mutate_authorized(
        &self,
        authorization: PointTopologyAuthorization,
        mutation: PointTopologyMutation,
    ) -> Result<PointTopologyAcceptance, ApplicationError> {
        let PointTopologyAuthorization {
            context,
            expected_revision,
        } = authorization;
        if let Err(error) = validate_mutation(&mutation) {
            self.record_audit(
                &context,
                Some(expected_revision),
                &mutation,
                AuditOutcome::Rejected,
                Some(error.to_string()),
            )
            .await?;
            return Err(error);
        }
        self.mutate_validated(&context, expected_revision, mutation)
            .await
    }

    async fn mutate_validated(
        &self,
        context: &RequestContext,
        expected_revision: ChannelRevision,
        mutation: PointTopologyMutation,
    ) -> Result<PointTopologyAcceptance, ApplicationError> {
        self.record_audit(
            context,
            Some(expected_revision),
            &mutation,
            AuditOutcome::Attempted,
            None,
        )
        .await?;

        let mut transaction = match self.pool.begin().await {
            Ok(transaction) => transaction,
            Err(error) => {
                return self
                    .failed(context, expected_revision, &mutation, database_error(error))
                    .await;
            },
        };
        let resulting_revision = match advance_revision(
            &mut transaction,
            mutation.channel_id(),
            expected_revision,
        )
        .await
        {
            Ok(revision) => revision,
            Err(error) => {
                let _ = transaction.rollback().await;
                return self
                    .failed(context, expected_revision, &mutation, error)
                    .await;
            },
        };
        let protocol = match load_channel_protocol(&mut transaction, mutation.channel_id()).await {
            Ok(protocol) => protocol,
            Err(error) => {
                let _ = transaction.rollback().await;
                return self
                    .failed(context, expected_revision, &mutation, error)
                    .await;
            },
        };
        let result =
            match apply_topology_mutation(&mut transaction, mutation.clone(), &protocol).await {
                Ok(result) => result,
                Err(error) => {
                    let _ = transaction.rollback().await;
                    return self
                        .failed(context, expected_revision, &mutation, error)
                        .await;
                },
            };
        if let Err(error) = transaction.commit().await {
            return self
                .failed(context, expected_revision, &mutation, database_error(error))
                .await;
        }

        let completion_audit = match self
            .record_audit(
                context,
                Some(expected_revision),
                &mutation,
                AuditOutcome::Succeeded,
                Some(format!("resulting_revision={}", resulting_revision.get())),
            )
            .await
        {
            Ok(()) => CompletionAuditStatus::Recorded,
            Err(ApplicationError::AuditUnavailable(failure)) => {
                CompletionAuditStatus::Incomplete { failure }
            },
            Err(error) => return Err(error),
        };
        Ok(PointTopologyAcceptance {
            result,
            request_id: context.request_id().to_string(),
            resulting_revision,
            completion_audit,
        })
    }

    async fn failed<T>(
        &self,
        context: &RequestContext,
        expected_revision: ChannelRevision,
        mutation: &PointTopologyMutation,
        failure: PortError,
    ) -> Result<T, ApplicationError> {
        self.record_audit(
            context,
            Some(expected_revision),
            mutation,
            AuditOutcome::Failed,
            Some(format!("port_error_kind={:?}", failure.kind())),
        )
        .await?;
        Err(ApplicationError::Port(failure))
    }

    async fn record_audit(
        &self,
        context: &RequestContext,
        expected_revision: Option<ChannelRevision>,
        mutation: &PointTopologyMutation,
        outcome: AuditOutcome,
        suffix: Option<String>,
    ) -> Result<(), ApplicationError> {
        let expected = expected_revision
            .map_or_else(|| "none".to_string(), |revision| revision.get().to_string());
        let mut detail = format!(
            "operation={}; channel_id={}; expected_revision={expected}; point_type={}; point_count={}",
            mutation.audit_operation(),
            mutation.channel_id(),
            mutation.audit_point_type(),
            mutation.point_count()
        );
        if let Some(suffix) = suffix {
            detail.push_str("; ");
            detail.push_str(&suffix);
        }
        self.audit
            .record(AuditRecord::new(
                context.request_id(),
                context.actor().id(),
                MANAGE_CHANNEL_CAPABILITY.name(),
                outcome,
                context.timestamp(),
                Some(detail),
            ))
            .await
            .map_err(ApplicationError::AuditUnavailable)
    }
}

fn validate_command(
    expected_revision: Option<ChannelRevision>,
    mutation: &PointTopologyMutation,
) -> Result<ChannelRevision, ApplicationError> {
    validate_mutation(mutation)?;
    validate_expected_revision(expected_revision)
}

fn validate_mutation(mutation: &PointTopologyMutation) -> Result<(), ApplicationError> {
    if mutation.channel_id() >= MAX_CHANNEL_ID {
        return Err(ApplicationError::InvalidChannelMutation(
            "channel_id must be less than 10000".to_string(),
        ));
    }
    if mutation.point_count() == 0 {
        return Err(ApplicationError::InvalidChannelMutation(
            "point topology command must affect at least one point".to_string(),
        ));
    }
    Ok(())
}

fn validate_expected_revision(
    expected_revision: Option<ChannelRevision>,
) -> Result<ChannelRevision, ApplicationError> {
    let revision = expected_revision.ok_or_else(|| {
        ApplicationError::InvalidChannelMutation(
            "x-aether-expected-revision is required for point topology mutations".to_string(),
        )
    })?;
    if revision.get() == 0 || revision.checked_next().is_none() {
        return Err(ApplicationError::InvalidChannelMutation(
            "expected_revision must be in 1..9223372036854775807".to_string(),
        ));
    }
    Ok(revision)
}

async fn apply_topology_mutation(
    connection: &mut SqliteConnection,
    mutation: PointTopologyMutation,
    protocol: &str,
) -> Result<PointTopologyMutationResult, PortError> {
    match mutation {
        PointTopologyMutation::Single {
            channel_id,
            mutation,
        } => Ok(PointTopologyMutationResult::Single {
            signal_name: apply_point_mutation(connection, channel_id, protocol, mutation).await?,
        }),
        PointTopologyMutation::Batch {
            channel_id,
            mutations,
        } => {
            let mut outcomes = Vec::with_capacity(mutations.len());
            for mutation in mutations {
                let operation = mutation.operation();
                let point_type = mutation.kind().code();
                let point_id = mutation.point_id();
                match apply_point_mutation(&mut *connection, channel_id, protocol, mutation).await {
                    Ok(_) => {
                        outcomes.push(PointBatchMutationOutcome {
                            operation,
                            point_type,
                            point_id,
                            error: None,
                        });
                    },
                    Err(error) => return Err(error),
                }
            }
            Ok(PointTopologyMutationResult::Batch { outcomes })
        },
        PointTopologyMutation::Provision {
            channel_id,
            replace_existing,
            upsert_existing,
            points,
        } => {
            if replace_existing {
                ensure_channel_measurements_not_routed(&mut *connection, channel_id).await?;
                for table in [
                    "telemetry_points",
                    "signal_points",
                    "control_points",
                    "adjustment_points",
                ] {
                    sqlx::query(&format!("DELETE FROM {table} WHERE channel_id = ?"))
                        .bind(i64::from(channel_id))
                        .execute(&mut *connection)
                        .await
                        .map_err(database_error)?;
                }
            }
            let point_count = points.len();
            for (kind, definition) in points {
                apply_point_mutation(
                    &mut *connection,
                    channel_id,
                    protocol,
                    PointMutation::Create {
                        kind,
                        definition,
                        force: upsert_existing,
                    },
                )
                .await?;
            }
            Ok(PointTopologyMutationResult::Provisioned { point_count })
        },
        PointTopologyMutation::Mappings {
            channel_id,
            merge,
            mappings,
        } => {
            let mapping_count = mappings.len();
            for mapping in mappings {
                update_mapping(&mut *connection, channel_id, protocol, merge, mapping).await?;
            }
            Ok(PointTopologyMutationResult::MappingsUpdated { mapping_count })
        },
    }
}

async fn load_channel_protocol(
    connection: &mut SqliteConnection,
    channel_id: u32,
) -> Result<String, PortError> {
    sqlx::query_scalar::<_, String>("SELECT protocol FROM channels WHERE channel_id = ?")
        .bind(i64::from(channel_id))
        .fetch_optional(connection)
        .await
        .map_err(database_error)?
        .ok_or_else(|| {
            PortError::new(
                PortErrorKind::NotFound,
                format!("channel {channel_id} does not exist"),
            )
        })
}

async fn update_mapping(
    connection: &mut SqliteConnection,
    channel_id: u32,
    protocol: &str,
    merge: bool,
    mapping: PointMappingMutation,
) -> Result<(), PortError> {
    let mut protocol_data = mapping.protocol_data;
    if merge {
        let existing: Option<String> = sqlx::query_scalar(&format!(
            "SELECT protocol_mappings FROM {} WHERE channel_id = ? AND point_id = ?",
            mapping.kind.table()
        ))
        .bind(i64::from(channel_id))
        .bind(i64::from(mapping.point_id))
        .fetch_optional(&mut *connection)
        .await
        .map_err(database_error)?
        .flatten();
        let mut base = match existing {
            Some(value) => serde_json::from_str::<serde_json::Value>(&value).map_err(|error| {
                PortError::new(
                    PortErrorKind::InvalidData,
                    format!("existing protocol mapping is invalid JSON: {error}"),
                )
            })?,
            None => serde_json::json!({}),
        };
        match (&mut base, protocol_data) {
            (serde_json::Value::Object(base), serde_json::Value::Object(update)) => {
                base.extend(update);
                protocol_data = base.clone().into();
            },
            (_, serde_json::Value::Null) => protocol_data = serde_json::Value::Null,
            _ => {
                return Err(PortError::new(
                    PortErrorKind::InvalidData,
                    "protocol mapping must be an object or null",
                ));
            },
        }
    }
    validate_protocol_mapping(protocol, mapping.kind, mapping.point_id, &protocol_data)?;
    let serialized = match &protocol_data {
        serde_json::Value::Null => None,
        serde_json::Value::Object(values) if values.is_empty() => None,
        serde_json::Value::Object(_) => {
            Some(serde_json::to_string(&protocol_data).map_err(|error| {
                PortError::new(
                    PortErrorKind::InvalidData,
                    format!("protocol mapping serialization failed: {error}"),
                )
            })?)
        },
        _ => {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "protocol mapping must be an object or null",
            ));
        },
    };
    let result = sqlx::query(&format!(
        "UPDATE {} SET protocol_mappings = ? WHERE channel_id = ? AND point_id = ?",
        mapping.kind.table()
    ))
    .bind(serialized)
    .bind(i64::from(channel_id))
    .bind(i64::from(mapping.point_id))
    .execute(connection)
    .await
    .map_err(database_error)?;
    if result.rows_affected() == 1 {
        Ok(())
    } else {
        Err(point_not_found(channel_id, mapping.kind, mapping.point_id))
    }
}

async fn apply_point_mutation(
    connection: &mut SqliteConnection,
    channel_id: u32,
    protocol: &str,
    mutation: PointMutation,
) -> Result<String, PortError> {
    match mutation {
        PointMutation::Create {
            kind,
            definition,
            force,
        } => create_point(connection, channel_id, protocol, kind, definition, force).await,
        PointMutation::Update {
            kind,
            point_id,
            patch,
        } => update_point(connection, channel_id, protocol, kind, point_id, patch).await,
        PointMutation::Delete { kind, point_id } => {
            ensure_point_not_routed(&mut *connection, channel_id, kind, point_id).await?;
            let query = format!(
                "DELETE FROM {} WHERE channel_id = ? AND point_id = ? RETURNING signal_name",
                kind.table()
            );
            sqlx::query_scalar::<_, String>(&query)
                .bind(i64::from(channel_id))
                .bind(i64::from(point_id))
                .fetch_optional(connection)
                .await
                .map_err(database_error)?
                .ok_or_else(|| point_not_found(channel_id, kind, point_id))
        },
    }
}

async fn create_point(
    connection: &mut SqliteConnection,
    channel_id: u32,
    protocol: &str,
    kind: PointKind,
    definition: PointDefinitionMutation,
    force: bool,
) -> Result<String, PortError> {
    validate_definition(kind, &definition)?;
    validate_definition_mapping(
        &mut *connection,
        channel_id,
        protocol,
        kind,
        &definition,
        force,
    )
    .await?;
    let mapping = definition.protocol_mapping.clone().flatten();
    let preserve_mapping = force && definition.protocol_mapping.is_none();
    let conflict = if force {
        let mut fields = vec![
            "signal_name=excluded.signal_name",
            "scale=excluded.scale",
            "offset=excluded.offset",
            "unit=excluded.unit",
            "reverse=excluded.reverse",
            "data_type=excluded.data_type",
            "description=excluded.description",
        ];
        if kind == PointKind::Signal {
            fields.push("normal_state=excluded.normal_state");
        }
        if kind == PointKind::Adjustment {
            fields.extend([
                "min_value=excluded.min_value",
                "max_value=excluded.max_value",
                "step=excluded.step",
            ]);
        }
        if !preserve_mapping {
            fields.push("protocol_mappings=excluded.protocol_mappings");
        }
        format!(
            " ON CONFLICT(channel_id, point_id) DO UPDATE SET {}",
            fields.join(", ")
        )
    } else {
        String::new()
    };
    let query = match kind {
        PointKind::Signal => format!(
            "INSERT INTO signal_points \
             (channel_id, point_id, signal_name, scale, offset, unit, reverse, normal_state, data_type, description, protocol_mappings) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?){conflict}"
        ),
        PointKind::Adjustment => format!(
            "INSERT INTO adjustment_points \
             (channel_id, point_id, signal_name, scale, offset, unit, reverse, data_type, description, protocol_mappings, min_value, max_value, step) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?){conflict}"
        ),
        PointKind::Telemetry | PointKind::Control => format!(
            "INSERT INTO {} \
             (channel_id, point_id, signal_name, scale, offset, unit, reverse, data_type, description, protocol_mappings) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?){conflict}",
            kind.table()
        ),
    };
    let mut query = sqlx::query(&query)
        .bind(i64::from(channel_id))
        .bind(i64::from(definition.point_id))
        .bind(&definition.signal_name)
        .bind(definition.scale)
        .bind(definition.offset)
        .bind(&definition.unit)
        .bind(definition.reverse);
    query = match kind {
        PointKind::Signal => query
            .bind(definition.normal_state)
            .bind(&definition.data_type)
            .bind(&definition.description)
            .bind(mapping),
        PointKind::Adjustment => query
            .bind(&definition.data_type)
            .bind(&definition.description)
            .bind(mapping)
            .bind(definition.minimum)
            .bind(definition.maximum)
            .bind(definition.step),
        PointKind::Telemetry | PointKind::Control => query
            .bind(&definition.data_type)
            .bind(&definition.description)
            .bind(mapping),
    };
    query.execute(connection).await.map_err(database_error)?;
    Ok(definition.signal_name)
}

async fn validate_definition_mapping(
    connection: &mut SqliteConnection,
    channel_id: u32,
    protocol: &str,
    kind: PointKind,
    definition: &PointDefinitionMutation,
    force: bool,
) -> Result<(), PortError> {
    let mapping = match &definition.protocol_mapping {
        Some(Some(mapping)) => parse_mapping_json(mapping)?,
        Some(None) => serde_json::Value::Null,
        None if force => {
            let existing: Option<Option<String>> = sqlx::query_scalar(&format!(
                "SELECT protocol_mappings FROM {} WHERE channel_id = ? AND point_id = ?",
                kind.table()
            ))
            .bind(i64::from(channel_id))
            .bind(i64::from(definition.point_id))
            .fetch_optional(connection)
            .await
            .map_err(database_error)?;
            mapping_value(existing.flatten())?
        },
        None => serde_json::Value::Null,
    };
    validate_protocol_mapping(protocol, kind, definition.point_id, &mapping)
}

async fn validate_existing_mapping(
    connection: &mut SqliteConnection,
    channel_id: u32,
    protocol: &str,
    kind: PointKind,
    point_id: u32,
) -> Result<(), PortError> {
    let existing: Option<Option<String>> = sqlx::query_scalar(&format!(
        "SELECT protocol_mappings FROM {} WHERE channel_id = ? AND point_id = ?",
        kind.table()
    ))
    .bind(i64::from(channel_id))
    .bind(i64::from(point_id))
    .fetch_optional(connection)
    .await
    .map_err(database_error)?;
    let stored = existing.ok_or_else(|| point_not_found(channel_id, kind, point_id))?;
    let mapping = mapping_value(stored)?;
    validate_protocol_mapping(protocol, kind, point_id, &mapping)
}

fn mapping_value(stored: Option<String>) -> Result<serde_json::Value, PortError> {
    stored.map_or(Ok(serde_json::Value::Null), |mapping| {
        parse_mapping_json(&mapping)
    })
}

fn parse_mapping_json(mapping: &str) -> Result<serde_json::Value, PortError> {
    serde_json::from_str(mapping).map_err(|error| {
        PortError::new(
            PortErrorKind::InvalidData,
            format!("protocol mapping is invalid JSON: {error}"),
        )
    })
}

pub(crate) fn validate_protocol_mapping(
    protocol: &str,
    kind: PointKind,
    point_id: u32,
    mapping: &serde_json::Value,
) -> Result<(), PortError> {
    if mapping.is_null() || mapping.as_object().is_some_and(serde_json::Map::is_empty) {
        return Ok(());
    }
    let values = mapping
        .as_object()
        .ok_or_else(|| invalid_mapping(point_id, "protocol mapping must be an object or null"))?;
    if crate::utils::is_modbus_family(protocol) {
        let slave_id = required_u64(values, point_id, "slave_id")?;
        if !(1..=247).contains(&slave_id) {
            return Err(invalid_mapping(point_id, "slave_id must be in 1..247"));
        }
        let function_code = required_u64(values, point_id, "function_code")?;
        if ![1, 2, 3, 4, 5, 6, 15, 16].contains(&function_code) {
            return Err(invalid_mapping(
                point_id,
                "function_code must be one of 1,2,3,4,5,6,15,16",
            ));
        }
        if required_u64(values, point_id, "register_address")? > u16::MAX.into() {
            return Err(invalid_mapping(point_id, "register_address must be a u16"));
        }
        let direction_valid = match kind {
            PointKind::Telemetry | PointKind::Signal => [1, 2, 3, 4].contains(&function_code),
            PointKind::Control => [5, 6, 15, 16].contains(&function_code),
            PointKind::Adjustment => [6, 16].contains(&function_code),
        };
        if !direction_valid {
            return Err(invalid_mapping(
                point_id,
                "function_code does not match point direction",
            ));
        }
        if let Some(data_type) = values.get("data_type") {
            let data_type = data_type
                .as_str()
                .ok_or_else(|| invalid_mapping(point_id, "data_type must be a string"))?;
            if ![
                "bool", "boolean", "uint16", "int16", "uint32", "int32", "float32", "float64",
            ]
            .contains(&data_type)
            {
                return Err(invalid_mapping(point_id, "unsupported data_type"));
            }
        }
        if let Some(byte_order) = values.get("byte_order") {
            let byte_order = byte_order
                .as_str()
                .ok_or_else(|| invalid_mapping(point_id, "byte_order must be a string"))?;
            if !["ABCD", "DCBA", "BADC", "CDAB", "AB", "BA"].contains(&byte_order) {
                return Err(invalid_mapping(point_id, "unsupported byte_order"));
            }
        }
        if let Some(bit_position) = values.get("bit_position")
            && bit_position
                .as_u64()
                .is_none_or(|value| value > u8::MAX.into())
        {
            return Err(invalid_mapping(point_id, "bit_position must be a u8"));
        }
        return Ok(());
    }

    match crate::utils::normalize_protocol_name(protocol).as_ref() {
        "virtual" => {
            // The current virtual runtime derives its address from point_id and
            // does not consume an inline mapping. Preserve existing metadata,
            // while still rejecting a malformed legacy expression when present.
            if let Some(expression) = values.get("expression") {
                let expression = expression
                    .as_str()
                    .ok_or_else(|| invalid_mapping(point_id, "expression must be a string"))?;
                if expression.trim().is_empty() {
                    return Err(invalid_mapping(point_id, "expression must not be blank"));
                }
            }
            Ok(())
        },
        "di_do" | "gpio" | "dido" => {
            let gpio = required_u64(values, point_id, "gpio_number")?;
            if gpio > 1023 {
                return Err(invalid_mapping(point_id, "gpio_number exceeds 1023"));
            }
            if !matches!(kind, PointKind::Signal | PointKind::Control) {
                return Err(invalid_mapping(
                    point_id,
                    "GPIO only supports signal and control points",
                ));
            }
            Ok(())
        },
        "can" => {
            validate_can_id(values, point_id)?;
            validate_can_layout(values, point_id)?;
            Ok(())
        },
        "aether_485" => {
            if !matches!(kind, PointKind::Telemetry | PointKind::Signal) {
                return Err(invalid_mapping(
                    point_id,
                    "aether_485 inline mappings are consumed only for telemetry and signal points",
                ));
            }
            if required_u64(values, point_id, "device_id")? > u8::MAX.into() {
                return Err(invalid_mapping(point_id, "device_id must be a u8"));
            }
            if values
                .get("cmd")
                .is_some_and(|cmd| cmd.as_u64().is_none_or(|value| value > u8::MAX.into()))
            {
                return Err(invalid_mapping(point_id, "cmd must be a u8"));
            }
            Ok(())
        },
        "iec61850" => {
            required_nonblank_string(values, point_id, "address")?;
            if values
                .get("ctrl_model")
                .is_some_and(|model| model.as_u64().is_none_or(|value| !(1..=4).contains(&value)))
            {
                return Err(invalid_mapping(point_id, "ctrl_model must be in 1..4"));
            }
            Ok(())
        },
        "iec104" => validate_iec104_mapping(values, point_id),
        "opcua" => validate_opcua_mapping(values, point_id),
        "mqtt" | "http" => validate_json_payload_mapping(values, point_id),
        other => Err(invalid_mapping(
            point_id,
            &format!("unsupported protocol {other}"),
        )),
    }
}

fn validate_json_payload_mapping(
    values: &serde_json::Map<String, serde_json::Value>,
    point_id: u32,
) -> Result<(), PortError> {
    for field in values.keys() {
        if !matches!(
            field.as_str(),
            "json_path" | "data_type" | "scale" | "offset" | "description"
        ) {
            return Err(invalid_mapping(
                point_id,
                &format!("unsupported JSON payload mapping field {field}"),
            ));
        }
    }

    let json_path = required_nonblank_string(values, point_id, "json_path")?;
    serde_json_path::JsonPath::parse(json_path)
        .map_err(|error| invalid_mapping(point_id, &format!("json_path is invalid: {error}")))?;

    if let Some(data_type) = values.get("data_type") {
        let data_type = data_type
            .as_str()
            .ok_or_else(|| invalid_mapping(point_id, "data_type must be a string"))?;
        if !matches!(
            data_type,
            "float" | "int" | "integer" | "bool" | "boolean" | "string" | "str"
        ) {
            return Err(invalid_mapping(
                point_id,
                "data_type must be float, int, bool, or string",
            ));
        }
    }

    for field in ["scale", "offset"] {
        if let Some(value) = values.get(field)
            && value.as_f64().is_none_or(|value| !value.is_finite())
        {
            return Err(invalid_mapping(
                point_id,
                &format!("{field} must be a finite number"),
            ));
        }
    }

    if values
        .get("description")
        .is_some_and(|description| !description.is_string())
    {
        return Err(invalid_mapping(point_id, "description must be a string"));
    }
    Ok(())
}

fn validate_can_layout(
    values: &serde_json::Map<String, serde_json::Value>,
    point_id: u32,
) -> Result<(), PortError> {
    let start_bit = if let Some(start_bit) = values.get("start_bit") {
        let start_bit = start_bit
            .as_u64()
            .ok_or_else(|| invalid_mapping(point_id, "start_bit must be an unsigned integer"))?;
        if start_bit > 63 {
            return Err(invalid_mapping(point_id, "start_bit exceeds 63"));
        }
        start_bit
    } else {
        let byte_offset = required_u64(values, point_id, "byte_offset")?;
        if byte_offset > 7 {
            return Err(invalid_mapping(point_id, "byte_offset exceeds 7"));
        }
        let bit_position = values.get("bit_position").map_or(Ok(0), |position| {
            position
                .as_u64()
                .filter(|value| *value <= 7)
                .ok_or_else(|| invalid_mapping(point_id, "bit_position exceeds 7"))
        })?;
        byte_offset * 8 + bit_position
    };
    let bit_length = required_u64(values, point_id, "bit_length")?;
    if !(1..=64).contains(&bit_length) || start_bit + bit_length > 64 {
        return Err(invalid_mapping(
            point_id,
            "bit layout must fit in a 64-bit CAN payload",
        ));
    }
    if let Some(data_type) = values.get("data_type") {
        let data_type = data_type
            .as_str()
            .ok_or_else(|| invalid_mapping(point_id, "data_type must be a string"))?;
        if ![
            "uint8", "uint16", "int16", "uint32", "int32", "float32", "ascii",
        ]
        .contains(&data_type)
        {
            return Err(invalid_mapping(point_id, "unsupported CAN data_type"));
        }
    }
    Ok(())
}

fn validate_iec104_mapping(
    values: &serde_json::Map<String, serde_json::Value>,
    point_id: u32,
) -> Result<(), PortError> {
    if let Some(address) = values.get("address") {
        if address
            .as_str()
            .is_some_and(|value| !value.trim().is_empty())
        {
            return Ok(());
        }
        return Err(invalid_mapping(
            point_id,
            "address must be a nonblank string",
        ));
    }
    if required_u64(values, point_id, "ioa")? > u32::MAX.into() {
        return Err(invalid_mapping(point_id, "ioa must be a u32"));
    }
    if values
        .get("type_id")
        .is_some_and(|value| value.as_u64().is_none_or(|value| value > u8::MAX.into()))
    {
        return Err(invalid_mapping(point_id, "type_id must be a u8"));
    }
    Ok(())
}

fn validate_opcua_mapping(
    values: &serde_json::Map<String, serde_json::Value>,
    point_id: u32,
) -> Result<(), PortError> {
    if values.get("address").is_some() {
        required_nonblank_string(values, point_id, "address")?;
    } else {
        required_nonblank_string(values, point_id, "node_id")?;
    }
    if values
        .get("namespace_index")
        .is_some_and(|value| value.as_u64().is_none_or(|value| value > u16::MAX.into()))
    {
        return Err(invalid_mapping(point_id, "namespace_index must be a u16"));
    }
    Ok(())
}

fn required_nonblank_string<'a>(
    values: &'a serde_json::Map<String, serde_json::Value>,
    point_id: u32,
    field: &str,
) -> Result<&'a str, PortError> {
    values
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| invalid_mapping(point_id, &format!("{field} must be a nonblank string")))
}

fn required_u64(
    values: &serde_json::Map<String, serde_json::Value>,
    point_id: u32,
    field: &str,
) -> Result<u64, PortError> {
    values
        .get(field)
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| invalid_mapping(point_id, &format!("{field} must be an unsigned integer")))
}

fn validate_can_id(
    values: &serde_json::Map<String, serde_json::Value>,
    point_id: u32,
) -> Result<(), PortError> {
    let Some(can_id) = values.get("can_id") else {
        return Err(invalid_mapping(point_id, "can_id is required"));
    };
    let valid = can_id
        .as_u64()
        .is_some_and(|value| value <= u32::MAX.into())
        || can_id.as_str().is_some_and(|value| {
            value
                .strip_prefix("0x")
                .or_else(|| value.strip_prefix("0X"))
                .is_some_and(|hex| u32::from_str_radix(hex, 16).is_ok())
        });
    if valid {
        Ok(())
    } else {
        Err(invalid_mapping(point_id, "can_id is invalid"))
    }
}

fn invalid_mapping(point_id: u32, reason: &str) -> PortError {
    PortError::new(
        PortErrorKind::InvalidData,
        format!("point {point_id} mapping validation failed: {reason}"),
    )
}

async fn ensure_point_not_routed(
    connection: &mut SqliteConnection,
    channel_id: u32,
    kind: PointKind,
    point_id: u32,
) -> Result<(), PortError> {
    let routing_table = match kind {
        PointKind::Telemetry | PointKind::Signal => "measurement_routing",
        PointKind::Control | PointKind::Adjustment => "action_routing",
    };
    let routed = sqlx::query_scalar::<_, i64>(&format!(
        "SELECT COUNT(*) FROM {routing_table} \
         WHERE channel_id = ? AND channel_type = ? AND channel_point_id = ?"
    ))
    .bind(i64::from(channel_id))
    .bind(kind.code())
    .bind(i64::from(point_id))
    .fetch_one(connection)
    .await
    .map_err(database_error)?;
    if routed == 0 {
        Ok(())
    } else {
        Err(PortError::new(
            PortErrorKind::Conflict,
            format!(
                "point {}:{}:{} is referenced by logical routing",
                channel_id,
                kind.code(),
                point_id
            ),
        ))
    }
}

async fn ensure_channel_measurements_not_routed(
    connection: &mut SqliteConnection,
    channel_id: u32,
) -> Result<(), PortError> {
    let routed = sqlx::query_scalar::<_, i64>(
        "SELECT \
           (SELECT COUNT(*) FROM measurement_routing WHERE channel_id = ?) + \
           (SELECT COUNT(*) FROM action_routing WHERE channel_id = ?)",
    )
    .bind(i64::from(channel_id))
    .bind(i64::from(channel_id))
    .fetch_one(connection)
    .await
    .map_err(database_error)?;
    if routed == 0 {
        Ok(())
    } else {
        Err(PortError::new(
            PortErrorKind::Conflict,
            format!(
                "channel {channel_id} has logical routes; remove them through the routing authority before replacing points"
            ),
        ))
    }
}

async fn update_point(
    connection: &mut SqliteConnection,
    channel_id: u32,
    protocol: &str,
    kind: PointKind,
    point_id: u32,
    patch: PointPatchMutation,
) -> Result<String, PortError> {
    validate_existing_mapping(&mut *connection, channel_id, protocol, kind, point_id).await?;
    if patch.is_empty() {
        return Err(PortError::new(
            PortErrorKind::InvalidData,
            "No fields provided for update",
        ));
    }
    validate_optional_finite("scale", patch.scale)?;
    validate_optional_finite("offset", patch.offset)?;
    if kind != PointKind::Adjustment
        && (patch.minimum.is_some() || patch.maximum.is_some() || patch.step.is_some())
    {
        return Err(PortError::new(
            PortErrorKind::InvalidData,
            "min_value, max_value, and step are only valid for adjustment points",
        ));
    }
    if kind == PointKind::Adjustment {
        let existing = sqlx::query_as::<_, (Option<f64>, Option<f64>, f64)>(
            "SELECT min_value, max_value, step FROM adjustment_points \
             WHERE channel_id = ? AND point_id = ?",
        )
        .bind(i64::from(channel_id))
        .bind(i64::from(point_id))
        .fetch_optional(&mut *connection)
        .await
        .map_err(database_error)?
        .ok_or_else(|| point_not_found(channel_id, kind, point_id))?;
        let minimum = patch.minimum.or(existing.0);
        let maximum = patch.maximum.or(existing.1);
        let step = patch.step.unwrap_or(existing.2);
        aether_domain::CommandConstraints::new(minimum, maximum, Some(step)).map_err(|error| {
            PortError::new(
                PortErrorKind::InvalidData,
                format!("Invalid adjustment constraints: {error}"),
            )
        })?;
        return sqlx::query_scalar::<_, String>(
            "UPDATE adjustment_points SET
                signal_name=COALESCE(?, signal_name), description=COALESCE(?, description),
                unit=COALESCE(?, unit), scale=COALESCE(?, scale), offset=COALESCE(?, offset),
                data_type=COALESCE(?, data_type), reverse=COALESCE(?, reverse),
                min_value=?, max_value=?, step=?
             WHERE channel_id=? AND point_id=? RETURNING signal_name",
        )
        .bind(patch.signal_name.as_deref())
        .bind(patch.description.as_deref())
        .bind(patch.unit.as_deref())
        .bind(patch.scale)
        .bind(patch.offset)
        .bind(patch.data_type.as_deref())
        .bind(patch.reverse)
        .bind(minimum)
        .bind(maximum)
        .bind(step)
        .bind(i64::from(channel_id))
        .bind(i64::from(point_id))
        .fetch_optional(connection)
        .await
        .map_err(database_error)?
        .ok_or_else(|| point_not_found(channel_id, kind, point_id));
    }
    let query = format!(
        "UPDATE {} SET
            signal_name=COALESCE(?, signal_name), description=COALESCE(?, description),
            unit=COALESCE(?, unit), scale=COALESCE(?, scale), offset=COALESCE(?, offset),
            data_type=COALESCE(?, data_type), reverse=COALESCE(?, reverse)
         WHERE channel_id=? AND point_id=? RETURNING signal_name",
        kind.table()
    );
    sqlx::query_scalar::<_, String>(&query)
        .bind(patch.signal_name.as_deref())
        .bind(patch.description.as_deref())
        .bind(patch.unit.as_deref())
        .bind(patch.scale)
        .bind(patch.offset)
        .bind(patch.data_type.as_deref())
        .bind(patch.reverse)
        .bind(i64::from(channel_id))
        .bind(i64::from(point_id))
        .fetch_optional(connection)
        .await
        .map_err(database_error)?
        .ok_or_else(|| point_not_found(channel_id, kind, point_id))
}

fn validate_definition(
    kind: PointKind,
    definition: &PointDefinitionMutation,
) -> Result<(), PortError> {
    if definition.signal_name.trim().is_empty() {
        return Err(PortError::new(
            PortErrorKind::InvalidData,
            "signal_name must not be blank",
        ));
    }
    validate_finite("scale", definition.scale)?;
    validate_finite("offset", definition.offset)?;
    if kind == PointKind::Adjustment {
        aether_domain::CommandConstraints::new(
            definition.minimum,
            definition.maximum,
            Some(definition.step),
        )
        .map_err(|error| {
            PortError::new(
                PortErrorKind::InvalidData,
                format!("Invalid adjustment constraints: {error}"),
            )
        })?;
    }
    Ok(())
}

fn validate_optional_finite(name: &str, value: Option<f64>) -> Result<(), PortError> {
    value.map_or(Ok(()), |value| validate_finite(name, value))
}

fn validate_finite(name: &str, value: f64) -> Result<(), PortError> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(PortError::new(
            PortErrorKind::InvalidData,
            format!("{name} must be finite"),
        ))
    }
}

async fn advance_revision(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    channel_id: u32,
    expected_revision: ChannelRevision,
) -> Result<ChannelRevision, PortError> {
    let expected = i64::try_from(expected_revision.get()).map_err(|_| {
        PortError::new(
            PortErrorKind::InvalidData,
            "expected revision exceeds SQLite INTEGER range",
        )
    })?;
    let updated = sqlx::query(
        "UPDATE channels SET revision = revision + 1 \
         WHERE channel_id = ? AND revision = ? AND revision < 9223372036854775807",
    )
    .bind(i64::from(channel_id))
    .bind(expected)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    if updated.rows_affected() == 1 {
        return expected_revision.checked_next().ok_or_else(|| {
            PortError::new(PortErrorKind::Permanent, "channel revision is exhausted")
        });
    }
    let current =
        sqlx::query_scalar::<_, i64>("SELECT revision FROM channels WHERE channel_id = ?")
            .bind(i64::from(channel_id))
            .fetch_optional(&mut **transaction)
            .await
            .map_err(database_error)?;
    match current {
        None => Err(PortError::new(
            PortErrorKind::NotFound,
            format!("channel {channel_id} does not exist"),
        )),
        Some(i64::MAX) => Err(PortError::new(
            PortErrorKind::Permanent,
            format!("channel {channel_id} revision is exhausted"),
        )),
        Some(current) => Err(PortError::new(
            PortErrorKind::Conflict,
            format!(
                "channel {channel_id} revision conflict: expected {expected}, current {current}"
            ),
        )),
    }
}

fn point_not_found(channel_id: u32, kind: PointKind, point_id: u32) -> PortError {
    PortError::new(
        PortErrorKind::NotFound,
        format!(
            "Point {point_id} (type {}) not found in channel {channel_id}",
            kind.code()
        ),
    )
}

fn database_error(error: sqlx::Error) -> PortError {
    let database_error = error.as_database_error();
    let is_busy_or_locked = database_error
        .and_then(sqlx::error::DatabaseError::code)
        .is_some_and(|code| is_sqlite_busy_or_locked_code(code.as_ref()));
    let kind = if is_busy_or_locked {
        PortErrorKind::Unavailable
    } else if database_error.is_some_and(sqlx::error::DatabaseError::is_unique_violation) {
        PortErrorKind::Conflict
    } else {
        PortErrorKind::Permanent
    };
    PortError::new(kind, format!("point topology database failure: {error}"))
}

fn is_sqlite_busy_or_locked_code(code: &str) -> bool {
    code == "SQLITE_BUSY"
        || code == "SQLITE_LOCKED"
        || code.starts_with("SQLITE_BUSY_")
        || code.starts_with("SQLITE_LOCKED_")
        || code
            .parse::<i32>()
            .is_ok_and(|numeric| matches!(numeric & 0xff, 5 | 6))
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::{PointKind, validate_protocol_mapping};

    #[test]
    fn canonical_validator_accepts_current_inline_mapping_consumers() {
        let cases = [
            (
                "modbus_tcp",
                PointKind::Telemetry,
                serde_json::json!({
                    "slave_id": 1,
                    "function_code": 3,
                    "register_address": 17
                }),
            ),
            (
                "virtual",
                PointKind::Telemetry,
                serde_json::json!({"initial_value": 25.0, "noise_range": 2.0}),
            ),
            (
                "gpio",
                PointKind::Signal,
                serde_json::json!({"gpio_number": 496}),
            ),
            (
                "can",
                PointKind::Telemetry,
                serde_json::json!({
                    "can_id": 849,
                    "start_bit": 0,
                    "bit_length": 16,
                    "data_type": "uint16"
                }),
            ),
            (
                "aether_485",
                PointKind::Telemetry,
                serde_json::json!({"device_id": 1, "cmd": 1}),
            ),
            (
                "iec61850",
                PointKind::Control,
                serde_json::json!({
                    "address": "simpleIOGenericIO/GGIO1$CO$SPCSO1$Oper",
                    "ctrl_model": 1
                }),
            ),
        ];

        for (protocol, kind, mapping) in cases {
            assert!(
                validate_protocol_mapping(protocol, kind, 1, &mapping).is_ok(),
                "{protocol} mapping should be accepted"
            );
        }
    }

    #[test]
    fn mqtt_and_http_accept_only_complete_valid_inline_json_mappings() {
        for protocol in ["mqtt", "http"] {
            assert!(
                validate_protocol_mapping(
                    protocol,
                    PointKind::Telemetry,
                    1,
                    &serde_json::json!({
                        "json_path": "$.value",
                        "data_type": "float",
                        "scale": 0.1,
                        "offset": -1.0
                    }),
                )
                .is_ok()
            );
            assert!(
                validate_protocol_mapping(
                    protocol,
                    PointKind::Telemetry,
                    1,
                    &serde_json::Value::Null
                )
                .is_ok()
            );

            for invalid in [
                serde_json::json!({"data_type": "float"}),
                serde_json::json!({"json_path": "invalid[[["}),
                serde_json::json!({"json_path": "$.value", "data_type": "decimal"}),
                serde_json::json!({"json_path": "$.value", "scale": "large"}),
                serde_json::json!({"json_path": "$.value", "offset": "high"}),
            ] {
                assert!(
                    validate_protocol_mapping(protocol, PointKind::Telemetry, 1, &invalid).is_err(),
                    "{protocol} unexpectedly accepted {invalid}"
                );
            }
        }
    }
}
