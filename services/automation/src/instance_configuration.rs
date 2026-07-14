//! Governed instance-configuration command boundary.
//!
//! SQLite is the sole desired-state authority for commissioned instances. A
//! command advances the aggregate `instances` revision with compare-and-set in
//! the same transaction as validation and mutation. Process-local name and
//! routing projections are refreshed only after the durable commit and report
//! explicit reconciliation degradation instead of disguising a committed
//! command as a retryable error.

use std::collections::HashMap;
use std::sync::Arc;

use aether_application::{MANAGE_INSTANCE_CAPABILITY, RequestContext, SafetyPolicy};
use aether_model::validate_instance_name;
use aether_ports::{AuditOutcome, AuditRecord, AuditSink, PortError};
use sqlx::Sqlite;

use crate::dto::InstancePropertyPoint;
use crate::error::AutomationError;
use crate::instance_manager::InstanceManager;
use crate::product_loader::{CreateInstanceRequest, Instance};

const INSTANCES_REVISION_SCOPE: &str = "instances";

/// Aggregate compare-and-set head for all commissioned instances.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InstanceConfigurationRevision(u64);

impl InstanceConfigurationRevision {
    /// Creates a revision token received from the configuration authority.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the serialized revision value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// One typed instance desired-state mutation.
#[derive(Debug, Clone)]
pub enum InstanceConfigurationMutation {
    /// Commission one instance and its initial property values.
    Create {
        request: CreateInstanceRequest,
        expected_revision: InstanceConfigurationRevision,
    },
    /// Atomically rename an instance and/or replace its complete property map.
    Update {
        instance_id: u32,
        instance_name: Option<String>,
        properties: Option<HashMap<String, serde_json::Value>>,
        expected_revision: InstanceConfigurationRevision,
    },
    /// Upsert one property without changing sibling properties.
    UpsertProperty {
        instance_id: u32,
        property_id: i32,
        value: serde_json::Value,
        expected_revision: InstanceConfigurationRevision,
    },
    /// Delete one property value without changing sibling properties.
    DeleteProperty {
        instance_id: u32,
        property_id: i32,
        expected_revision: InstanceConfigurationRevision,
    },
    /// Delete one complete hierarchy subtree after validating every member.
    DeleteSubtree {
        instance_id: u32,
        expected_revision: InstanceConfigurationRevision,
    },
}

impl InstanceConfigurationMutation {
    fn expected_revision(&self) -> InstanceConfigurationRevision {
        match self {
            Self::Create {
                expected_revision, ..
            }
            | Self::Update {
                expected_revision, ..
            }
            | Self::UpsertProperty {
                expected_revision, ..
            }
            | Self::DeleteProperty {
                expected_revision, ..
            }
            | Self::DeleteSubtree {
                expected_revision, ..
            } => *expected_revision,
        }
    }

    fn target(&self) -> Option<u32> {
        match self {
            Self::Create { request, .. } => request.instance_id,
            Self::Update { instance_id, .. }
            | Self::UpsertProperty { instance_id, .. }
            | Self::DeleteProperty { instance_id, .. }
            | Self::DeleteSubtree { instance_id, .. } => Some(*instance_id),
        }
    }

    fn audit_detail(&self) -> String {
        let expected_revision = self.expected_revision().get();
        match self {
            Self::Create { request, .. } => format!(
                "operation=create; expected_revision={expected_revision}; requested_instance_id={:?}; instance_name={}; product_name={}; parent_id={:?}; property_count={}",
                request.instance_id,
                request.instance_name,
                request.product_name,
                request.parent_id,
                request.properties.len()
            ),
            Self::Update {
                instance_id,
                instance_name,
                properties,
                ..
            } => format!(
                "operation=update; expected_revision={expected_revision}; instance_id={instance_id}; rename={}; replace_properties={}; property_count={}",
                instance_name.is_some(),
                properties.is_some(),
                properties.as_ref().map_or(0, HashMap::len)
            ),
            Self::UpsertProperty {
                instance_id,
                property_id,
                ..
            } => format!(
                "operation=upsert_property; expected_revision={expected_revision}; instance_id={instance_id}; property_id={property_id}"
            ),
            Self::DeleteProperty {
                instance_id,
                property_id,
                ..
            } => format!(
                "operation=delete_property; expected_revision={expected_revision}; instance_id={instance_id}; property_id={property_id}"
            ),
            Self::DeleteSubtree { instance_id, .. } => format!(
                "operation=delete_subtree; expected_revision={expected_revision}; root_instance_id={instance_id}"
            ),
        }
    }
}

/// Stable command classification returned to callers and audit records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceConfigurationMutationKind {
    Create,
    Update,
    UpsertProperty,
    DeleteProperty,
    DeleteSubtree,
}

impl InstanceConfigurationMutationKind {
    /// Returns the stable wire/audit name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::UpsertProperty => "upsert_property",
            Self::DeleteProperty => "delete_property",
            Self::DeleteSubtree => "delete_subtree",
        }
    }
}

/// Operation-specific committed data.
#[derive(Debug, Clone)]
pub enum InstanceConfigurationPayload {
    Created(Instance),
    Updated {
        instance_id: u32,
        instance_name: String,
    },
    Property(InstancePropertyPoint),
    Deleted {
        root_instance_id: u32,
        deleted_instance_ids: Vec<u32>,
    },
}

/// Post-commit publication state of process-local instance/routing projections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstanceConfigurationRuntimeStatus {
    Refreshed,
    Degraded { failure: String },
}

impl InstanceConfigurationRuntimeStatus {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Refreshed => "refreshed",
            Self::Degraded { .. } => "degraded",
        }
    }

    #[must_use]
    pub const fn reconciliation_required(&self) -> bool {
        matches!(self, Self::Degraded { .. })
    }

    #[must_use]
    pub fn failure(&self) -> Option<&str> {
        match self {
            Self::Refreshed => None,
            Self::Degraded { failure } => Some(failure),
        }
    }
}

/// Terminal-audit state for an already committed non-idempotent operation.
#[derive(Debug, Clone)]
pub enum InstanceConfigurationAuditStatus {
    Recorded,
    Incomplete { failure: PortError },
}

impl InstanceConfigurationAuditStatus {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Recorded => "recorded",
            Self::Incomplete { .. } => "incomplete",
        }
    }

    #[must_use]
    pub const fn failure(&self) -> Option<&PortError> {
        match self {
            Self::Recorded => None,
            Self::Incomplete { failure } => Some(failure),
        }
    }
}

/// Accepted non-idempotent instance command. A degraded cache publication or
/// terminal audit never makes the already committed command safe to retry.
#[derive(Debug, Clone)]
pub struct InstanceConfigurationAcceptance {
    request_id: String,
    kind: InstanceConfigurationMutationKind,
    payload: InstanceConfigurationPayload,
    resulting_revision: InstanceConfigurationRevision,
    runtime_status: InstanceConfigurationRuntimeStatus,
    audit_status: InstanceConfigurationAuditStatus,
}

impl InstanceConfigurationAcceptance {
    #[must_use]
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    #[must_use]
    pub const fn kind(&self) -> InstanceConfigurationMutationKind {
        self.kind
    }

    #[must_use]
    pub const fn payload(&self) -> &InstanceConfigurationPayload {
        &self.payload
    }

    #[must_use]
    pub const fn resulting_revision(&self) -> InstanceConfigurationRevision {
        self.resulting_revision
    }

    #[must_use]
    pub const fn runtime_status(&self) -> &InstanceConfigurationRuntimeStatus {
        &self.runtime_status
    }

    #[must_use]
    pub const fn audit_status(&self) -> &InstanceConfigurationAuditStatus {
        &self.audit_status
    }

    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        false
    }
}

struct CommittedMutation {
    kind: InstanceConfigurationMutationKind,
    payload: InstanceConfigurationPayload,
    resulting_revision: InstanceConfigurationRevision,
}

/// Authenticated, confirmed, audited application facade for online instance
/// configuration. HTTP is only an adapter; tests and future CLI/MCP transports
/// invoke the same typed command.
pub struct InstanceConfigurationApplication {
    manager: Arc<InstanceManager>,
    audit: Arc<dyn AuditSink>,
    policy: SafetyPolicy,
}

impl InstanceConfigurationApplication {
    #[must_use]
    pub fn new(manager: Arc<InstanceManager>, audit: Arc<dyn AuditSink>) -> Self {
        Self {
            manager,
            audit,
            policy: SafetyPolicy,
        }
    }

    /// Reads the current authoritative CAS head for clients preparing a command.
    pub async fn current_revision(&self) -> Result<InstanceConfigurationRevision, AutomationError> {
        let revision = sqlx::query_scalar::<_, i64>(
            "SELECT revision FROM configuration_revisions WHERE scope = 'instances'",
        )
        .fetch_optional(self.manager.pool())
        .await
        .map_err(database_error)?
        .ok_or_else(|| {
            AutomationError::MissingConfig("instances revision is not initialized".to_string())
        })?;
        Ok(InstanceConfigurationRevision::new(
            u64::try_from(revision).map_err(|_| {
                AutomationError::DatabaseError("instances revision became negative".to_string())
            })?,
        ))
    }

    /// Applies one governed instance mutation.
    pub async fn mutate(
        &self,
        context: &RequestContext,
        mutation: InstanceConfigurationMutation,
    ) -> Result<InstanceConfigurationAcceptance, AutomationError> {
        let target = mutation.target();
        let detail = mutation.audit_detail();

        if let Err(error) = self.policy.authorize(MANAGE_INSTANCE_CAPABILITY, context) {
            self.record_audit(
                context,
                AuditOutcome::Rejected,
                &detail,
                Some(error.to_string()),
            )
            .await?;
            return Err(AutomationError::from(error));
        }

        self.record_audit(context, AuditOutcome::Attempted, &detail, None)
            .await?;

        let committed = match self.commit(mutation).await {
            Ok(committed) => committed,
            Err(error) => {
                self.record_audit(
                    context,
                    AuditOutcome::Failed,
                    &detail,
                    Some(error.to_string()),
                )
                .await?;
                return Err(error);
            },
        };

        let runtime_status = self.publish_committed_configuration().await;
        let completion_detail = format!(
            "{detail}; resulting_revision={}; runtime_status={}; reconciliation_required={}; target={target:?}",
            committed.resulting_revision.get(),
            runtime_status.as_str(),
            runtime_status.reconciliation_required()
        );
        let audit_status = match self
            .record_audit_raw(context, AuditOutcome::Succeeded, &completion_detail, None)
            .await
        {
            Ok(()) => InstanceConfigurationAuditStatus::Recorded,
            Err(failure) => InstanceConfigurationAuditStatus::Incomplete { failure },
        };

        Ok(InstanceConfigurationAcceptance {
            request_id: context.request_id().to_string(),
            kind: committed.kind,
            payload: committed.payload,
            resulting_revision: committed.resulting_revision,
            runtime_status,
            audit_status,
        })
    }

    async fn commit(
        &self,
        mutation: InstanceConfigurationMutation,
    ) -> Result<CommittedMutation, AutomationError> {
        validate_expected_revision(mutation.expected_revision())?;
        match mutation {
            InstanceConfigurationMutation::Create {
                request,
                expected_revision,
            } => self.commit_create(request, expected_revision).await,
            InstanceConfigurationMutation::Update {
                instance_id,
                instance_name,
                properties,
                expected_revision,
            } => {
                self.commit_update(instance_id, instance_name, properties, expected_revision)
                    .await
            },
            InstanceConfigurationMutation::UpsertProperty {
                instance_id,
                property_id,
                value,
                expected_revision,
            } => {
                self.commit_upsert_property(instance_id, property_id, value, expected_revision)
                    .await
            },
            InstanceConfigurationMutation::DeleteProperty {
                instance_id,
                property_id,
                expected_revision,
            } => {
                self.commit_delete_property(instance_id, property_id, expected_revision)
                    .await
            },
            InstanceConfigurationMutation::DeleteSubtree {
                instance_id,
                expected_revision,
            } => {
                self.commit_delete_subtree(instance_id, expected_revision)
                    .await
            },
        }
    }

    async fn commit_create(
        &self,
        request: CreateInstanceRequest,
        expected_revision: InstanceConfigurationRevision,
    ) -> Result<CommittedMutation, AutomationError> {
        validate_name(&request.instance_name)?;
        self.manager
            .product_loader()
            .get_product(&request.product_name)
            .map_err(|error| AutomationError::InvalidData(format!("unknown product: {error}")))?;

        let mut transaction = self.manager.pool().begin().await.map_err(database_error)?;
        if let Some(parent_id) = request.parent_id {
            let parent_exists =
                sqlx::query_scalar::<_, i64>("SELECT 1 FROM instances WHERE instance_id = ?")
                    .bind(i64::from(parent_id))
                    .fetch_optional(&mut *transaction)
                    .await
                    .map_err(database_error)?
                    .is_some();
            if !parent_exists {
                return Err(AutomationError::InstanceNotFound(format!(
                    "parent instance {parent_id}"
                )));
            }
        }

        let resulting_revision =
            compare_and_advance_revision(&mut transaction, expected_revision).await?;
        let instance_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO instances (instance_id, instance_name, product_name, parent_id) \
             VALUES (?, ?, ?, ?) RETURNING instance_id",
        )
        .bind(request.instance_id.map(i64::from))
        .bind(&request.instance_name)
        .bind(&request.product_name)
        .bind(request.parent_id.map(i64::from))
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| classify_instance_write(error, &request.instance_name))?;
        let instance_id = u32::try_from(instance_id).map_err(|_| {
            AutomationError::DatabaseError("created instance id is outside u32 range".to_string())
        })?;

        self.manager
            .write_properties_tx(
                &mut transaction,
                instance_id,
                &request.product_name,
                &request.properties,
            )
            .await
            .map_err(|error| AutomationError::InvalidData(error.to_string()))?;
        transaction.commit().await.map_err(database_error)?;

        Ok(CommittedMutation {
            kind: InstanceConfigurationMutationKind::Create,
            payload: InstanceConfigurationPayload::Created(Instance {
                core: crate::config::InstanceCore {
                    instance_id,
                    instance_name: request.instance_name,
                    product_name: request.product_name,
                    parent_id: request.parent_id,
                    properties: request.properties,
                },
                created_at: Some(chrono::Utc::now()),
            }),
            resulting_revision,
        })
    }

    async fn commit_update(
        &self,
        instance_id: u32,
        instance_name: Option<String>,
        properties: Option<HashMap<String, serde_json::Value>>,
        expected_revision: InstanceConfigurationRevision,
    ) -> Result<CommittedMutation, AutomationError> {
        if instance_name.is_none() && properties.is_none() {
            return Err(AutomationError::InvalidData(
                "at least one of instance_name or properties is required".to_string(),
            ));
        }
        if let Some(name) = instance_name.as_deref() {
            validate_name(name)?;
        }

        let mut transaction = self.manager.pool().begin().await.map_err(database_error)?;
        let (current_name, product_name) = sqlx::query_as::<_, (String, String)>(
            "SELECT instance_name, product_name FROM instances WHERE instance_id = ?",
        )
        .bind(i64::from(instance_id))
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| AutomationError::InstanceNotFound(instance_id.to_string()))?;

        let resulting_revision =
            compare_and_advance_revision(&mut transaction, expected_revision).await?;
        let final_name = instance_name.as_deref().unwrap_or(&current_name);
        if final_name != current_name {
            sqlx::query(
                "UPDATE instances SET instance_name = ?, updated_at = CURRENT_TIMESTAMP \
                 WHERE instance_id = ?",
            )
            .bind(final_name)
            .bind(i64::from(instance_id))
            .execute(&mut *transaction)
            .await
            .map_err(|error| classify_instance_write(error, final_name))?;

            // These names are denormalized display projections only. Routing
            // identity remains (instance_id, point_id), so this transaction
            // deliberately does not advance `logical_routing`.
            for table in ["measurement_routing", "action_routing"] {
                let statement = format!(
                    "UPDATE {table} SET instance_name = ?, updated_at = CURRENT_TIMESTAMP \
                     WHERE instance_id = ?"
                );
                sqlx::query(&statement)
                    .bind(final_name)
                    .bind(i64::from(instance_id))
                    .execute(&mut *transaction)
                    .await
                    .map_err(database_error)?;
            }
        }

        if let Some(properties) = properties.as_ref() {
            sqlx::query("DELETE FROM instance_properties WHERE instance_id = ?")
                .bind(i64::from(instance_id))
                .execute(&mut *transaction)
                .await
                .map_err(database_error)?;
            self.manager
                .write_properties_tx(&mut transaction, instance_id, &product_name, properties)
                .await
                .map_err(|error| AutomationError::InvalidData(error.to_string()))?;
            sqlx::query(
                "UPDATE instances SET updated_at = CURRENT_TIMESTAMP WHERE instance_id = ?",
            )
            .bind(i64::from(instance_id))
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        }

        transaction.commit().await.map_err(database_error)?;
        Ok(CommittedMutation {
            kind: InstanceConfigurationMutationKind::Update,
            payload: InstanceConfigurationPayload::Updated {
                instance_id,
                instance_name: final_name.to_string(),
            },
            resulting_revision,
        })
    }

    async fn commit_upsert_property(
        &self,
        instance_id: u32,
        property_id: i32,
        value: serde_json::Value,
        expected_revision: InstanceConfigurationRevision,
    ) -> Result<CommittedMutation, AutomationError> {
        let mut transaction = self.manager.pool().begin().await.map_err(database_error)?;
        let (product_name, property) = self
            .validate_property(&mut transaction, instance_id, property_id)
            .await?;
        let resulting_revision =
            compare_and_advance_revision(&mut transaction, expected_revision).await?;
        let value_json = serde_json::to_string(&value)?;
        sqlx::query(
            "INSERT INTO instance_properties (instance_id, property_id, value_json) \
             VALUES (?, ?, ?) ON CONFLICT(instance_id, property_id) DO UPDATE SET \
             value_json = excluded.value_json, updated_at = CURRENT_TIMESTAMP",
        )
        .bind(i64::from(instance_id))
        .bind(i64::from(property_id))
        .bind(value_json)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        sqlx::query("UPDATE instances SET updated_at = CURRENT_TIMESTAMP WHERE instance_id = ?")
            .bind(i64::from(instance_id))
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        tracing::debug!(instance_id, property_id, %product_name, "instance property committed");

        Ok(CommittedMutation {
            kind: InstanceConfigurationMutationKind::UpsertProperty,
            payload: InstanceConfigurationPayload::Property(InstancePropertyPoint {
                property_id: property.property_id,
                name: property.name,
                unit: property.unit,
                description: property.description,
                value: Some(value),
            }),
            resulting_revision,
        })
    }

    async fn commit_delete_property(
        &self,
        instance_id: u32,
        property_id: i32,
        expected_revision: InstanceConfigurationRevision,
    ) -> Result<CommittedMutation, AutomationError> {
        let mut transaction = self.manager.pool().begin().await.map_err(database_error)?;
        let (_, property) = self
            .validate_property(&mut transaction, instance_id, property_id)
            .await?;
        let resulting_revision =
            compare_and_advance_revision(&mut transaction, expected_revision).await?;
        sqlx::query("DELETE FROM instance_properties WHERE instance_id = ? AND property_id = ?")
            .bind(i64::from(instance_id))
            .bind(i64::from(property_id))
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        sqlx::query("UPDATE instances SET updated_at = CURRENT_TIMESTAMP WHERE instance_id = ?")
            .bind(i64::from(instance_id))
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;

        Ok(CommittedMutation {
            kind: InstanceConfigurationMutationKind::DeleteProperty,
            payload: InstanceConfigurationPayload::Property(InstancePropertyPoint {
                property_id: property.property_id,
                name: property.name,
                unit: property.unit,
                description: property.description,
                value: None,
            }),
            resulting_revision,
        })
    }

    async fn validate_property(
        &self,
        transaction: &mut sqlx::Transaction<'_, Sqlite>,
        instance_id: u32,
        property_id: i32,
    ) -> Result<(String, crate::product_loader::PropertyTemplate), AutomationError> {
        let product_name = sqlx::query_scalar::<_, String>(
            "SELECT product_name FROM instances WHERE instance_id = ?",
        )
        .bind(i64::from(instance_id))
        .fetch_optional(&mut **transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| AutomationError::InstanceNotFound(instance_id.to_string()))?;
        let product = self
            .manager
            .product_loader()
            .get_product(&product_name)
            .map_err(|error| AutomationError::InvalidData(error.to_string()))?;
        let property = product
            .properties
            .into_iter()
            .find(|property| property.property_id == property_id)
            .ok_or_else(|| {
                AutomationError::InvalidData(format!(
                    "property_id {property_id} is not declared by product {product_name}"
                ))
            })?;
        Ok((product_name, property))
    }

    async fn commit_delete_subtree(
        &self,
        instance_id: u32,
        expected_revision: InstanceConfigurationRevision,
    ) -> Result<CommittedMutation, AutomationError> {
        let mut transaction = self.manager.pool().begin().await.map_err(database_error)?;
        let rows = sqlx::query_as::<_, (i64, String)>(
            "WITH RECURSIVE subtree(instance_id) AS (\
               SELECT instance_id FROM instances WHERE instance_id = ? \
               UNION SELECT child.instance_id FROM instances AS child \
               JOIN subtree AS parent ON child.parent_id = parent.instance_id\
             ) SELECT instance_id, instance_name FROM instances \
             WHERE instance_id IN (SELECT instance_id FROM subtree) ORDER BY instance_id",
        )
        .bind(i64::from(instance_id))
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?;
        if rows.is_empty() {
            return Err(AutomationError::InstanceNotFound(instance_id.to_string()));
        }
        let mut deleted_instance_ids = Vec::with_capacity(rows.len());
        for (id, _) in &rows {
            deleted_instance_ids.push(u32::try_from(*id).map_err(|_| {
                AutomationError::DatabaseError("instance id is outside u32 range".to_string())
            })?);
        }

        if let Some(routed_id) = sqlx::query_scalar::<_, i64>(
            "WITH RECURSIVE subtree(instance_id) AS (\
               SELECT instance_id FROM instances WHERE instance_id = ? \
               UNION SELECT child.instance_id FROM instances AS child \
               JOIN subtree AS parent ON child.parent_id = parent.instance_id\
             ), routed(instance_id) AS (\
               SELECT instance_id FROM measurement_routing \
               UNION SELECT instance_id FROM action_routing\
             ) SELECT instance_id FROM routed WHERE instance_id IN (\
               SELECT instance_id FROM subtree\
             ) ORDER BY instance_id LIMIT 1",
        )
        .bind(i64::from(instance_id))
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        {
            return Err(AutomationError::ConfigurationConflict(format!(
                "instance subtree contains routed instance {routed_id}; remove its logical routes first"
            )));
        }

        let resulting_revision =
            compare_and_advance_revision(&mut transaction, expected_revision).await?;
        let affected = sqlx::query(
            "WITH RECURSIVE subtree(instance_id) AS (\
               SELECT instance_id FROM instances WHERE instance_id = ? \
               UNION SELECT child.instance_id FROM instances AS child \
               JOIN subtree AS parent ON child.parent_id = parent.instance_id\
             ) DELETE FROM instances WHERE instance_id IN (\
               SELECT instance_id FROM subtree\
             )",
        )
        .bind(i64::from(instance_id))
        .execute(&mut *transaction)
        .await
        .map_err(|error| classify_delete_error(error, instance_id))?
        .rows_affected();
        if affected != deleted_instance_ids.len() as u64 {
            return Err(AutomationError::DatabaseError(format!(
                "subtree deletion affected {affected} rows, expected {}",
                deleted_instance_ids.len()
            )));
        }
        transaction.commit().await.map_err(database_error)?;

        Ok(CommittedMutation {
            kind: InstanceConfigurationMutationKind::DeleteSubtree,
            payload: InstanceConfigurationPayload::Deleted {
                root_instance_id: instance_id,
                deleted_instance_ids,
            },
            resulting_revision,
        })
    }

    async fn publish_committed_configuration(&self) -> InstanceConfigurationRuntimeStatus {
        let mut failures = Vec::new();
        if let Err(error) = self.manager.populate_name_cache().await {
            failures.push(format!("instance name index refresh failed: {error}"));
        }
        if let Err(error) = self.manager.refresh_routing().await {
            // Production runtime topology revokes command routing on failed
            // refresh and reconciliation retries from SQLite. This boundary
            // never writes the retired compatibility routing cache.
            failures.push(format!("logical routing refresh failed: {error}"));
        }
        if failures.is_empty() {
            InstanceConfigurationRuntimeStatus::Refreshed
        } else {
            let failure = failures.join("; ");
            tracing::error!(%failure, "committed instance configuration requires reconciliation");
            InstanceConfigurationRuntimeStatus::Degraded { failure }
        }
    }

    async fn record_audit(
        &self,
        context: &RequestContext,
        outcome: AuditOutcome,
        detail: &str,
        failure: Option<String>,
    ) -> Result<(), AutomationError> {
        self.record_audit_raw(context, outcome, detail, failure)
            .await
            .map_err(|error| AutomationError::AuditUnavailable(error.to_string()))
    }

    async fn record_audit_raw(
        &self,
        context: &RequestContext,
        outcome: AuditOutcome,
        detail: &str,
        failure: Option<String>,
    ) -> Result<(), PortError> {
        let detail = failure.map_or_else(
            || detail.to_string(),
            |failure| format!("{detail}; failure={failure}"),
        );
        self.audit
            .record(AuditRecord::new(
                context.request_id(),
                context.actor().id(),
                MANAGE_INSTANCE_CAPABILITY.name(),
                outcome,
                context.timestamp(),
                Some(detail),
            ))
            .await
    }
}

/// Idempotently installs the service-owned revision scope without claiming a
/// repository schema-migration number. The shared table may be initialized by
/// another service or installer first.
pub async fn initialize_instance_configuration_revision(
    pool: &sqlx::SqlitePool,
) -> Result<(), AutomationError> {
    sqlx::query(crate::config::CONFIGURATION_REVISIONS_TABLE)
        .execute(pool)
        .await
        .map_err(database_error)?;
    sqlx::query(
        "INSERT INTO configuration_revisions (scope, revision) VALUES ('instances', 1) \
         ON CONFLICT(scope) DO NOTHING",
    )
    .execute(pool)
    .await
    .map_err(database_error)?;
    Ok(())
}

async fn compare_and_advance_revision(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    expected_revision: InstanceConfigurationRevision,
) -> Result<InstanceConfigurationRevision, AutomationError> {
    let revision = sqlx::query_scalar::<_, i64>(
        "UPDATE configuration_revisions SET revision = revision + 1, \
         updated_at = CURRENT_TIMESTAMP WHERE scope = ? AND revision = ? RETURNING revision",
    )
    .bind(INSTANCES_REVISION_SCOPE)
    .bind(expected_revision.get() as i64)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?
    .ok_or_else(|| {
        AutomationError::ConfigurationConflict(format!(
            "instances changed concurrently; expected revision {}",
            expected_revision.get()
        ))
    })?;
    Ok(InstanceConfigurationRevision::new(
        u64::try_from(revision).map_err(|_| {
            AutomationError::DatabaseError("instances revision became negative".to_string())
        })?,
    ))
}

fn validate_expected_revision(
    revision: InstanceConfigurationRevision,
) -> Result<(), AutomationError> {
    if revision.get() == 0 || revision.get() >= i64::MAX as u64 {
        return Err(AutomationError::InvalidData(
            "expected instances revision must be in 1..i64::MAX".to_string(),
        ));
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<(), AutomationError> {
    validate_instance_name(name)
        .map_err(|error| AutomationError::InvalidData(format!("invalid instance name: {error}")))
}

fn database_error(error: sqlx::Error) -> AutomationError {
    AutomationError::DatabaseError(error.to_string())
}

fn classify_instance_write(error: sqlx::Error, name: &str) -> AutomationError {
    if error.to_string().contains("UNIQUE constraint failed") {
        AutomationError::InstanceExists(name.to_string())
    } else {
        database_error(error)
    }
}

fn classify_delete_error(error: sqlx::Error, instance_id: u32) -> AutomationError {
    let message = error.to_string();
    if message.contains("governed measurement-routing command")
        || message.contains("governed action-routing command")
    {
        AutomationError::ConfigurationConflict(format!(
            "instance subtree rooted at {instance_id} is still routed"
        ))
    } else {
        AutomationError::DatabaseError(message)
    }
}
