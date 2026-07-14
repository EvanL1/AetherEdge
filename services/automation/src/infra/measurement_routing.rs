//! SQLite and coherent-runtime adapter for governed measurement routing.

use std::sync::Arc;

use aether_domain::PointKind;
use aether_ports::{
    AutomationMeasurementRoutingMutator, MeasurementRoute, MeasurementRoutingMutation,
    MeasurementRoutingMutationReceipt, PortError, PortErrorKind, PortResult,
};
use async_trait::async_trait;

use crate::infra::runtime_topology::MeasurementRoutingMutationLease;
use crate::instance_manager::InstanceManager;

/// Applies validated measurement-route CAS mutations to authoritative SQLite.
pub struct SqliteMeasurementRoutingMutator {
    manager: Arc<InstanceManager>,
}

impl SqliteMeasurementRoutingMutator {
    /// Creates the adapter over automation's configuration and runtime topology.
    #[must_use]
    pub fn new(manager: Arc<InstanceManager>) -> Self {
        Self { manager }
    }

    async fn validate_upsert(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        route: MeasurementRoute,
    ) -> PortResult<String> {
        let key = route.key();
        let destination = route.destination();
        let instance_id = key.instance_id().get();
        let measurement_id = key.measurement_id().get();
        let channel_id = destination.channel_id().get();
        let point_id = destination.point_id().get();
        let (channel_type, physical_table) = match destination.kind() {
            PointKind::Telemetry => ("T", "telemetry_points"),
            PointKind::Status => ("S", "signal_points"),
            PointKind::Command | PointKind::Action => {
                return Err(invalid(
                    "measurement route destination is not acquisition-owned",
                ));
            },
        };

        let instance = sqlx::query_as::<_, (String, String)>(
            "SELECT instance_name, product_name FROM instances WHERE instance_id = ?",
        )
        .bind(i64::from(instance_id))
        .fetch_optional(&mut **transaction)
        .await
        .map_err(|error| storage_error("validate measurement-route instance", error))?
        .ok_or_else(|| {
            PortError::new(
                PortErrorKind::NotFound,
                format!("instance {instance_id} is not commissioned"),
            )
        })?;
        let (instance_name, product_name) = instance;
        let product = self
            .manager
            .product_loader()
            .get_product(&product_name)
            .map_err(|error| {
                tracing::error!(
                    instance_id,
                    measurement_id,
                    error = %error,
                    "measurement-route product validation failed"
                );
                PortError::new(
                    PortErrorKind::InvalidData,
                    "instance product is unavailable from the active Pack set",
                )
            })?;
        if !product
            .measurements
            .iter()
            .any(|measurement| measurement.measurement_id == measurement_id)
        {
            return Err(invalid(format!(
                "measurement point {measurement_id} is not declared by instance {instance_id}"
            )));
        }

        let physical_sql =
            format!("SELECT 1 FROM {physical_table} WHERE channel_id = ? AND point_id = ?");
        let physical_exists = sqlx::query_scalar::<_, i64>(&physical_sql)
            .bind(i64::from(channel_id))
            .bind(i64::from(point_id))
            .fetch_optional(&mut **transaction)
            .await
            .map_err(|error| storage_error("validate physical measurement target", error))?
            .is_some();
        if !physical_exists {
            return Err(invalid(format!(
                "physical measurement target {channel_id}:{channel_type}:{point_id} is not configured"
            )));
        }

        Ok(instance_name)
    }

    async fn publish_committed_routes(
        &self,
        topology_lease: Option<MeasurementRoutingMutationLease>,
    ) -> PortResult<()> {
        if let Some(topology_lease) = topology_lease {
            return topology_lease
                .publish(self.manager.pool())
                .await
                .map(|_| ());
        }

        self.manager
            .refresh_routing()
            .await
            .map(|_| ())
            .map_err(|error| {
                PortError::new(
                    PortErrorKind::Unavailable,
                    format!("committed measurement routing validation failed: {error}"),
                )
            })
    }
}

#[async_trait]
impl AutomationMeasurementRoutingMutator for SqliteMeasurementRoutingMutator {
    async fn mutate(
        &self,
        mutation: MeasurementRoutingMutation,
    ) -> PortResult<MeasurementRoutingMutationReceipt> {
        let kind = mutation.kind();
        let target = mutation.target();
        let expected = mutation.expected_revision();
        if expected.get() == 0 || expected.get() >= i64::MAX as u64 {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "expected logical-routing revision must be in 1..i64::MAX",
            ));
        }

        let mut topology_lease = match self.manager.runtime_topology() {
            Some(topology) => Some(
                Arc::clone(topology)
                    .begin_measurement_routing_mutation()
                    .await,
            ),
            None => None,
        };

        let staged: PortResult<_> =
            async {
                let mut transaction =
                    self.manager.pool().begin().await.map_err(|error| {
                        storage_error("begin measurement-routing mutation", error)
                    })?;
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
                MeasurementRoutingMutation::Upsert { route, .. } => {
                    let route_key = route.key();
                    let instance_id = route_key.instance_id().get();
                    let measurement_id = route_key.measurement_id().get();
                    let instance_name = self.validate_upsert(&mut transaction, route).await?;
                    let destination = route.destination();
                    let channel_type = match destination.kind() {
                        PointKind::Telemetry => "T",
                        PointKind::Status => "S",
                        PointKind::Command | PointKind::Action => {
                            return Err(invalid(
                                "measurement route destination is not acquisition-owned",
                            ));
                        },
                    };
                    sqlx::query(
                        "INSERT INTO measurement_routing \
                         (instance_id, instance_name, channel_id, channel_type, \
                          channel_point_id, measurement_id, enabled) \
                         VALUES (?, ?, ?, ?, ?, ?, ?) \
                         ON CONFLICT(instance_id, measurement_id) DO UPDATE SET \
                           instance_name = excluded.instance_name, \
                           channel_id = excluded.channel_id, \
                           channel_type = excluded.channel_type, \
                           channel_point_id = excluded.channel_point_id, \
                           enabled = excluded.enabled, updated_at = CURRENT_TIMESTAMP",
                    )
                    .bind(i64::from(instance_id))
                    .bind(instance_name)
                    .bind(i64::from(destination.channel_id().get()))
                    .bind(channel_type)
                    .bind(i64::from(destination.point_id().get()))
                    .bind(i64::from(measurement_id))
                    .bind(route.enabled())
                    .execute(&mut *transaction)
                    .await
                    .map_err(|error| storage_error("upsert measurement route", error))?
                    .rows_affected()
                },
                MeasurementRoutingMutation::Delete { route_key, .. } => sqlx::query(
                    "DELETE FROM measurement_routing WHERE instance_id = ? AND measurement_id = ?",
                )
                .bind(i64::from(route_key.instance_id().get()))
                .bind(i64::from(route_key.measurement_id().get()))
                .execute(&mut *transaction)
                .await
                .map_err(|error| storage_error("delete measurement route", error))?
                .rows_affected(),
                MeasurementRoutingMutation::SetEnabled {
                    route_key, enabled, ..
                } => sqlx::query(
                    "UPDATE measurement_routing SET enabled = ?, \
                       updated_at = CURRENT_TIMESTAMP \
                     WHERE instance_id = ? AND measurement_id = ?",
                )
                .bind(enabled)
                .bind(i64::from(route_key.instance_id().get()))
                .bind(i64::from(route_key.measurement_id().get()))
                .execute(&mut *transaction)
                .await
                .map_err(|error| storage_error("change measurement-route state", error))?
                .rows_affected(),
                MeasurementRoutingMutation::DeleteForInstance { instance_id, .. } => {
                    sqlx::query("DELETE FROM measurement_routing WHERE instance_id = ?")
                        .bind(i64::from(instance_id.get()))
                        .execute(&mut *transaction)
                        .await
                        .map_err(|error| {
                            storage_error("delete instance measurement routes", error)
                        })?
                        .rows_affected()
                },
                MeasurementRoutingMutation::DeleteForChannel { channel_id, .. } => {
                    sqlx::query("DELETE FROM measurement_routing WHERE channel_id = ?")
                        .bind(i64::from(channel_id.get()))
                        .execute(&mut *transaction)
                        .await
                        .map_err(|error| storage_error("delete channel measurement routes", error))?
                        .rows_affected()
                },
                MeasurementRoutingMutation::DeleteAll { .. } => {
                    sqlx::query("DELETE FROM measurement_routing")
                        .execute(&mut *transaction)
                        .await
                        .map_err(|error| storage_error("delete all measurement routes", error))?
                        .rows_affected()
                },
            };

                if matches!(
                    mutation,
                    MeasurementRoutingMutation::Delete { .. }
                        | MeasurementRoutingMutation::SetEnabled { .. }
                ) && affected_routes != 1
                {
                    return Err(PortError::new(
                        PortErrorKind::NotFound,
                        "measurement route is not commissioned",
                    ));
                }
                Ok((transaction, affected_routes, resulting_revision))
            }
            .await;

        let (transaction, affected_routes, resulting_revision) = match staged {
            Ok(staged) => staged,
            Err(error) => {
                if let Some(topology_lease) = topology_lease.take() {
                    topology_lease.restore().await;
                }
                return Err(error);
            },
        };

        if let Some(topology_lease) = topology_lease.as_mut() {
            topology_lease.commit_started();
        }
        transaction
            .commit()
            .await
            .map_err(|error| storage_error("commit measurement-routing mutation", error))?;

        match self.publish_committed_routes(topology_lease).await {
            Ok(()) => Ok(MeasurementRoutingMutationReceipt::new(
                kind,
                target,
                affected_routes,
                aether_ports::LogicalRoutingRevision::new(
                    u64::try_from(resulting_revision).map_err(|_| {
                        PortError::new(
                            PortErrorKind::Permanent,
                            "logical-routing revision became negative",
                        )
                    })?,
                ),
            )),
            Err(failure) => Ok(MeasurementRoutingMutationReceipt::measurements_revoked(
                kind,
                target,
                affected_routes,
                aether_ports::LogicalRoutingRevision::new(
                    u64::try_from(resulting_revision).map_err(|_| {
                        PortError::new(
                            PortErrorKind::Permanent,
                            "logical-routing revision became negative",
                        )
                    })?,
                ),
                failure,
            )),
        }
    }
}

fn invalid(message: impl Into<String>) -> PortError {
    PortError::new(PortErrorKind::InvalidData, message)
}

fn storage_error(operation: &'static str, error: sqlx::Error) -> PortError {
    tracing::error!(operation, error = %error, "measurement-routing storage operation failed");
    let kind = match error {
        sqlx::Error::RowNotFound => PortErrorKind::NotFound,
        sqlx::Error::Database(ref database_error) if database_error.is_unique_violation() => {
            PortErrorKind::Conflict
        },
        _ => PortErrorKind::Unavailable,
    };
    PortError::new(kind, format!("{operation} failed"))
}
