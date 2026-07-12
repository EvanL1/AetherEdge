//! SQLite and runtime-cache adapter for governed action-routing mutations.

use std::collections::HashMap;
use std::sync::Arc;

use aether_domain::PointKind;
use aether_ports::{
    ActionRoutingMutation, ActionRoutingMutationReceipt, AutomationActionRoutingMutator, PortError,
    PortErrorKind, PortResult,
};
use async_trait::async_trait;
use common::FourRemote;

use crate::instance_manager::InstanceManager;
use crate::routing_loader::ActionRoutingRow;

/// Applies governed action-routing changes to SQLite and publishes the exact
/// committed view to the in-process command dispatcher.
pub struct SqliteActionRoutingMutator {
    manager: Arc<InstanceManager>,
}

impl SqliteActionRoutingMutator {
    /// Creates an adapter over automation's authoritative configuration and
    /// atomically swappable routing cache.
    #[must_use]
    pub fn new(manager: Arc<InstanceManager>) -> Self {
        Self { manager }
    }

    async fn validate_upsert(&self, route: aether_ports::ActionRoute) -> PortResult<String> {
        let key = route.key();
        let destination = route.destination();
        let instance_id = key.instance_id().get();
        let action_id = key.action_id().get();
        let channel_id = destination.channel_id().get();
        let channel_point_id = destination.point_id().get();
        let (four_remote, physical_table) = match destination.kind() {
            PointKind::Command => (FourRemote::Control, "control_points"),
            PointKind::Action => (FourRemote::Adjustment, "adjustment_points"),
            PointKind::Telemetry | PointKind::Status => {
                return Err(PortError::new(
                    PortErrorKind::InvalidData,
                    "action route destination is not command-owned",
                ));
            },
        };

        let instance_name = sqlx::query_scalar::<_, String>(
            "SELECT instance_name FROM instances WHERE instance_id = ?",
        )
        .bind(i64::from(instance_id))
        .fetch_optional(self.manager.pool())
        .await
        .map_err(|error| storage_error("validate action-route instance", error))?
        .ok_or_else(|| {
            PortError::new(
                PortErrorKind::NotFound,
                format!("instance {instance_id} is not commissioned"),
            )
        })?;

        let validation = self
            .manager
            .validate_action_routing(
                &ActionRoutingRow {
                    action_id,
                    channel_id: Some(channel_id as i32),
                    channel_type: Some(four_remote),
                    channel_point_id: Some(channel_point_id),
                },
                &instance_name,
            )
            .await
            .map_err(|error| {
                tracing::error!(
                    instance_id,
                    action_id,
                    error = %error,
                    "action-route model validation failed"
                );
                PortError::new(
                    PortErrorKind::Unavailable,
                    "action-route model validation is unavailable",
                )
            })?;
        if !validation.is_valid {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                format!(
                    "logical action route is not valid for instance {instance_id}: {}",
                    validation.errors.join("; ")
                ),
            ));
        }

        let physical_sql =
            format!("SELECT 1 FROM {physical_table} WHERE channel_id = ? AND point_id = ?");
        let physical_exists = sqlx::query_scalar::<_, i64>(&physical_sql)
            .bind(i64::from(channel_id))
            .bind(i64::from(channel_point_id))
            .fetch_optional(self.manager.pool())
            .await
            .map_err(|error| storage_error("validate physical action-route target", error))?
            .is_some();
        if !physical_exists {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                format!(
                    "physical action target {channel_id}:{}:{channel_point_id} is not configured",
                    four_remote.as_str()
                ),
            ));
        }

        Ok(instance_name)
    }

    async fn publish_committed_routes(&self) -> PortResult<()> {
        match aether_routing::load_routing_maps(self.manager.pool()).await {
            Ok(maps) => {
                self.manager
                    .routing_cache()
                    .update(maps.c2m, maps.m2c, maps.c2c);
                Ok(())
            },
            Err(error) => {
                // Continuing with the previous M2C view after a committed
                // topology change could dispatch to the wrong physical point.
                // Revoke every local route until a complete snapshot can be
                // loaded again.
                self.manager
                    .routing_cache()
                    .update(HashMap::new(), HashMap::new(), HashMap::new());
                tracing::error!(
                    error = %error,
                    "committed action routing could not be published; routing cache revoked"
                );
                Err(PortError::new(
                    PortErrorKind::Unavailable,
                    "committed action routing could not be published; command routing is disabled",
                ))
            },
        }
    }
}

#[async_trait]
impl AutomationActionRoutingMutator for SqliteActionRoutingMutator {
    async fn mutate(
        &self,
        mutation: ActionRoutingMutation,
    ) -> PortResult<ActionRoutingMutationReceipt> {
        let kind = mutation.kind();
        let target = mutation.target();
        let upsert_instance_name = match mutation {
            ActionRoutingMutation::Upsert { route } => Some(self.validate_upsert(route).await?),
            ActionRoutingMutation::Delete { .. }
            | ActionRoutingMutation::SetEnabled { .. }
            | ActionRoutingMutation::DeleteActionsForInstance { .. }
            | ActionRoutingMutation::DeleteActionsForChannel { .. }
            | ActionRoutingMutation::DeleteAllActions => None,
        };

        let mut transaction = self
            .manager
            .pool()
            .begin()
            .await
            .map_err(|error| storage_error("begin action-routing mutation", error))?;

        let affected_routes = match mutation {
            ActionRoutingMutation::Upsert { route } => {
                let key = route.key();
                let destination = route.destination();
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
                .bind(upsert_instance_name.as_deref())
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
            ActionRoutingMutation::Delete { route_key } => {
                sqlx::query("DELETE FROM action_routing WHERE instance_id = ? AND action_id = ?")
                    .bind(i64::from(route_key.instance_id().get()))
                    .bind(i64::from(route_key.action_id().get()))
                    .execute(&mut *transaction)
                    .await
                    .map_err(|error| storage_error("delete action route", error))?
                    .rows_affected()
            },
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
            ActionRoutingMutation::DeleteAllActions => sqlx::query("DELETE FROM action_routing")
                .execute(&mut *transaction)
                .await
                .map_err(|error| storage_error("delete all action routes", error))?
                .rows_affected(),
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

        transaction
            .commit()
            .await
            .map_err(|error| storage_error("commit action-routing mutation", error))?;
        self.publish_committed_routes().await?;

        Ok(ActionRoutingMutationReceipt::new(
            kind,
            target,
            affected_routes,
        ))
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
