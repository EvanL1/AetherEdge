//! SQLite and coherent-runtime adapter for governed action-routing mutations.

use std::sync::Arc;

use aether_domain::PointKind;
use aether_ports::{
    ActionRoutingMutation, ActionRoutingMutationReceipt, AutomationActionRoutingMutator,
    LogicalRoutingRevision, PortError, PortErrorKind, PortResult, RevisionedActionRoutingMutation,
};
use async_trait::async_trait;

use crate::infra::runtime_topology::ActionRoutingMutationLease;
use crate::instance_manager::InstanceManager;

/// Applies governed action-routing changes to SQLite and publishes the exact
/// committed view to the in-process command dispatcher.
pub struct SqliteActionRoutingMutator {
    manager: Arc<InstanceManager>,
}

impl SqliteActionRoutingMutator {
    /// Creates an adapter over automation's authoritative configuration and
    /// atomically swappable service topology.
    #[must_use]
    pub fn new(manager: Arc<InstanceManager>) -> Self {
        Self { manager }
    }

    async fn validate_upsert(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        route: aether_ports::ActionRoute,
    ) -> PortResult<String> {
        let key = route.key();
        let destination = route.destination();
        let instance_id = key.instance_id().get();
        let action_id = key.action_id().get();
        let channel_id = destination.channel_id().get();
        let channel_point_id = destination.point_id().get();
        let (channel_type, physical_table) = match destination.kind() {
            PointKind::Command => ("C", "control_points"),
            PointKind::Action => ("A", "adjustment_points"),
            PointKind::Telemetry | PointKind::Status => {
                return Err(PortError::new(
                    PortErrorKind::InvalidData,
                    "action route destination is not command-owned",
                ));
            },
        };

        let (instance_name, product_name) = sqlx::query_as::<_, (String, String)>(
            "SELECT instance_name, product_name FROM instances WHERE instance_id = ?",
        )
        .bind(i64::from(instance_id))
        .fetch_optional(&mut **transaction)
        .await
        .map_err(|error| storage_error("validate action-route instance", error))?
        .ok_or_else(|| {
            PortError::new(
                PortErrorKind::NotFound,
                format!("instance {instance_id} is not commissioned"),
            )
        })?;

        let product = self
            .manager
            .product_loader()
            .get_product(&product_name)
            .map_err(|error| {
                tracing::error!(
                    instance_id,
                    action_id,
                    error = %error,
                    "action-route product validation failed"
                );
                PortError::new(
                    PortErrorKind::InvalidData,
                    "instance product is unavailable from the active Pack set",
                )
            })?;
        if !product
            .actions
            .iter()
            .any(|action| action.action_id == action_id)
        {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                format!("action point {action_id} is not declared by instance {instance_id}"),
            ));
        }

        let physical_sql =
            format!("SELECT 1 FROM {physical_table} WHERE channel_id = ? AND point_id = ?");
        let physical_exists = sqlx::query_scalar::<_, i64>(&physical_sql)
            .bind(i64::from(channel_id))
            .bind(i64::from(channel_point_id))
            .fetch_optional(&mut **transaction)
            .await
            .map_err(|error| storage_error("validate physical action-route target", error))?
            .is_some();
        if !physical_exists {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                format!(
                    "physical action target {channel_id}:{}:{channel_point_id} is not configured",
                    channel_type
                ),
            ));
        }

        Ok(instance_name)
    }

    async fn publish_committed_routes(
        &self,
        topology_lease: Option<ActionRoutingMutationLease>,
    ) -> PortResult<()> {
        if let Some(topology_lease) = topology_lease {
            return match topology_lease.publish(self.manager.pool()).await {
                Ok(_) => Ok(()),
                Err(error) => {
                    // Commands were revoked before the SQLite transaction and
                    // remain revoked after this failed publication. Periodic
                    // topology refresh may restore them only from a complete
                    // later snapshot.
                    tracing::error!(
                        error = %error,
                        "committed action routing could not be published; command routes revoked"
                    );
                    Err(error)
                },
            };
        }

        self.manager
            .refresh_routing()
            .await
            .map(|_| ())
            .map_err(|error| {
                PortError::new(
                    PortErrorKind::Unavailable,
                    format!("committed action routing validation failed: {error}"),
                )
            })
    }
}

#[async_trait]
impl AutomationActionRoutingMutator for SqliteActionRoutingMutator {
    async fn mutate(
        &self,
        mutation: ActionRoutingMutation,
    ) -> PortResult<ActionRoutingMutationReceipt> {
        let revision = sqlx::query_scalar::<_, i64>(
            "SELECT revision FROM configuration_revisions WHERE scope = 'logical_routing'",
        )
        .fetch_one(self.manager.pool())
        .await
        .map_err(|error| storage_error("read logical-routing revision", error))?;
        let revision = LogicalRoutingRevision::new(u64::try_from(revision).map_err(|_| {
            PortError::new(
                PortErrorKind::Permanent,
                "logical-routing revision became negative",
            )
        })?);
        self.mutate_revisioned(RevisionedActionRoutingMutation::new(mutation, revision))
            .await
    }

    async fn mutate_revisioned(
        &self,
        mutation: RevisionedActionRoutingMutation,
    ) -> PortResult<ActionRoutingMutationReceipt> {
        let kind = mutation.kind();
        let target = mutation.target();
        let expected = mutation.expected_revision();
        let mutation = mutation.mutation();
        if expected.get() == 0 || expected.get() >= i64::MAX as u64 {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "expected logical-routing revision must be in 1..i64::MAX",
            ));
        }

        // Production revokes M2C before opening the mutation transaction and
        // retains the refresh lease through commit + publication. Therefore a
        // command can observe either the old generation before revocation or
        // the complete committed generation after publication, never an old
        // route during the commit-to-publish window.
        let mut topology_lease = match self.manager.runtime_topology() {
            Some(topology) => Some(Arc::clone(topology).begin_action_routing_mutation().await),
            None => None,
        };

        let staged_mutation: PortResult<_> = async {
            let mut transaction = self
                .manager
                .pool()
                .begin()
                .await
                .map_err(|error| storage_error("begin action-routing mutation", error))?;

            let resulting_revision = sqlx::query_scalar::<_, i64>(
                "UPDATE configuration_revisions \
                 SET revision = revision + 1, updated_at = CURRENT_TIMESTAMP \
                 WHERE scope = 'logical_routing' AND revision = ? \
                 RETURNING revision",
            )
            .bind(expected.get() as i64)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|error| storage_error("compare logical-routing revision", error))?
            .ok_or_else(|| {
                PortError::new(
                    PortErrorKind::Conflict,
                    format!(
                        "logical routing changed concurrently; expected revision {}",
                        expected.get()
                    ),
                )
            })?;

            let affected_routes = match mutation {
                ActionRoutingMutation::Upsert { route } => {
                    let key = route.key();
                    let destination = route.destination();
                    let instance_name = self.validate_upsert(&mut transaction, route).await?;
                    let channel_type = match destination.kind() {
                        PointKind::Command => "C",
                        PointKind::Action => "A",
                        PointKind::Telemetry | PointKind::Status => {
                            return Err(PortError::new(
                                PortErrorKind::InvalidData,
                                "action route destination is not command-owned",
                            ));
                        },
                    };
                    sqlx::query(
                        "INSERT INTO action_routing \
                     (instance_id, instance_name, action_id, channel_id, channel_type, \
                      channel_point_id, enabled) \
                     VALUES (?, ?, ?, ?, ?, ?, ?) \
                     ON CONFLICT(instance_id, action_id) DO UPDATE SET \
                       instance_name = excluded.instance_name, \
                       channel_id = excluded.channel_id, \
                       channel_type = excluded.channel_type, \
                       channel_point_id = excluded.channel_point_id, \
                       enabled = excluded.enabled, \
                       updated_at = CURRENT_TIMESTAMP",
                    )
                    .bind(i64::from(key.instance_id().get()))
                    .bind(instance_name)
                    .bind(i64::from(key.action_id().get()))
                    .bind(i64::from(destination.channel_id().get()))
                    .bind(channel_type)
                    .bind(i64::from(destination.point_id().get()))
                    .bind(route.enabled())
                    .execute(&mut *transaction)
                    .await
                    .map_err(|error| storage_error("upsert action route", error))?
                    .rows_affected()
                },
                ActionRoutingMutation::Delete { route_key } => sqlx::query(
                    "DELETE FROM action_routing WHERE instance_id = ? AND action_id = ?",
                )
                .bind(i64::from(route_key.instance_id().get()))
                .bind(i64::from(route_key.action_id().get()))
                .execute(&mut *transaction)
                .await
                .map_err(|error| storage_error("delete action route", error))?
                .rows_affected(),
                ActionRoutingMutation::SetEnabled { route_key, enabled } => sqlx::query(
                    "UPDATE action_routing SET enabled = ?, updated_at = CURRENT_TIMESTAMP \
                 WHERE instance_id = ? AND action_id = ?",
                )
                .bind(enabled)
                .bind(i64::from(route_key.instance_id().get()))
                .bind(i64::from(route_key.action_id().get()))
                .execute(&mut *transaction)
                .await
                .map_err(|error| storage_error("change action-route state", error))?
                .rows_affected(),
                ActionRoutingMutation::DeleteActionsForInstance { instance_id } => {
                    sqlx::query("DELETE FROM action_routing WHERE instance_id = ?")
                        .bind(i64::from(instance_id.get()))
                        .execute(&mut *transaction)
                        .await
                        .map_err(|error| storage_error("delete instance action routes", error))?
                        .rows_affected()
                },
                ActionRoutingMutation::DeleteActionsForChannel { channel_id } => {
                    sqlx::query("DELETE FROM action_routing WHERE channel_id = ?")
                        .bind(i64::from(channel_id.get()))
                        .execute(&mut *transaction)
                        .await
                        .map_err(|error| storage_error("delete channel action routes", error))?
                        .rows_affected()
                },
                ActionRoutingMutation::DeleteAllActions => {
                    sqlx::query("DELETE FROM action_routing")
                        .execute(&mut *transaction)
                        .await
                        .map_err(|error| storage_error("delete all action routes", error))?
                        .rows_affected()
                },
            };

            if matches!(
                mutation,
                ActionRoutingMutation::Delete { .. } | ActionRoutingMutation::SetEnabled { .. }
            ) && affected_routes == 0
            {
                transaction
                    .rollback()
                    .await
                    .map_err(|error| storage_error("roll back missing action route", error))?;
                return Err(PortError::new(
                    PortErrorKind::NotFound,
                    "action route is not commissioned",
                ));
            }

            Ok((transaction, affected_routes, resulting_revision))
        }
        .await;

        let (transaction, affected_routes, resulting_revision) = match staged_mutation {
            Ok(staged) => staged,
            Err(error) => {
                if let Some(topology_lease) = topology_lease.take() {
                    topology_lease.restore().await;
                }
                return Err(error);
            },
        };

        // A commit error has an uncertain durable outcome. Disarm restoration
        // before attempting it so neither `?` nor cancellation can republish a
        // pre-commit generation over data that SQLite may already have applied.
        if let Some(topology_lease) = topology_lease.as_mut() {
            topology_lease.commit_started();
        }
        transaction
            .commit()
            .await
            .map_err(|error| storage_error("commit action-routing mutation", error))?;

        let resulting_revision = aether_ports::LogicalRoutingRevision::new(
            u64::try_from(resulting_revision).map_err(|_| {
                PortError::new(
                    PortErrorKind::Permanent,
                    "logical-routing revision became negative",
                )
            })?,
        );

        match self.publish_committed_routes(topology_lease).await {
            Ok(()) => Ok(ActionRoutingMutationReceipt::new_at_revision(
                kind,
                target,
                affected_routes,
                resulting_revision,
            )),
            Err(failure) => Ok(ActionRoutingMutationReceipt::commands_revoked_at_revision(
                kind,
                target,
                affected_routes,
                resulting_revision,
                failure,
            )),
        }
    }
}

fn storage_error(operation: &'static str, error: sqlx::Error) -> PortError {
    tracing::error!(operation, error = %error, "action-routing storage operation failed");
    let kind = match error {
        sqlx::Error::RowNotFound => PortErrorKind::NotFound,
        _ => PortErrorKind::Unavailable,
    };
    PortError::new(kind, format!("{operation} failed"))
}
