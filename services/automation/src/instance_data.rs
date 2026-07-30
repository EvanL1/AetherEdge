//! Instance Data Loading and Query Operations
//!
//! This module provides data loading, querying, and synchronization operations.
//! Extracted from instance_manager.rs for better code organization.

#![allow(clippy::disallowed_methods)] // json! macro used in multiple functions

use anyhow::{Result, anyhow};
use std::collections::{BTreeMap, HashMap};

use super::instance_manager::InstanceManager;
use crate::instance_query::{
    InstanceActionPointView, InstanceDataPlane, InstanceLiveDataView, InstanceLiveSample,
    InstanceMeasurementPointView, InstancePointsView, InstancePropertyView, PointRoutingView,
};
use crate::routing_loader::{RoutingRoute, RoutingScope};

impl RoutingRoute {
    fn into_point_routing(self) -> Result<PointRoutingView> {
        Ok(PointRoutingView {
            channel_id: Some(
                i32::try_from(self.channel_id)
                    .map_err(|_| anyhow!("Channel ID {} exceeds the API range", self.channel_id))?,
            ),
            channel_type: Some(self.channel_type),
            channel_point_id: Some(self.channel_point_id),
            enabled: self.enabled,
            channel_name: self.channel_name,
            channel_point_name: self.channel_point_name,
        })
    }
}

#[derive(Debug, sqlx::FromRow)]
struct StoredPointDetail {
    product_name: String,
    routing_revision: i64,
    channel_id: Option<i32>,
    channel_type: Option<String>,
    channel_point_id: Option<u32>,
    enabled: Option<bool>,
    channel_name: Option<String>,
    channel_point_name: Option<String>,
}

impl StoredPointDetail {
    fn into_snapshot(self) -> Result<PointDetailSnapshot> {
        let revision = crate::routing_loader::decode_revision(self.routing_revision)?;
        let routing = match (self.channel_type, self.enabled) {
            (Some(channel_type), Some(enabled)) => Some(PointRoutingView {
                channel_id: self.channel_id,
                channel_type: Some(channel_type),
                channel_point_id: self.channel_point_id,
                enabled,
                channel_name: self.channel_name,
                channel_point_name: self.channel_point_name,
            }),
            _ => None,
        };
        Ok(PointDetailSnapshot {
            product_name: self.product_name,
            routing,
            revision,
        })
    }
}

struct PointDetailSnapshot {
    product_name: String,
    routing: Option<PointRoutingView>,
    revision: u64,
}

#[derive(Clone, Copy)]
enum PointPlane {
    Measurement,
    Action,
}

impl PointPlane {
    const fn query(self) -> &'static str {
        match self {
            Self::Measurement => {
                r#"
                SELECT i.product_name, cr.revision AS routing_revision,
                       mr.channel_id, mr.channel_type, mr.channel_point_id, mr.enabled,
                       c.name AS channel_name,
                       COALESCE(tp.signal_name, sp.signal_name) AS channel_point_name
                FROM instances i
                JOIN configuration_revisions cr ON cr.scope = 'logical_routing'
                LEFT JOIN measurement_routing mr
                  ON mr.instance_id = i.instance_id AND mr.measurement_id = ?
                LEFT JOIN channels c ON c.channel_id = mr.channel_id
                LEFT JOIN telemetry_points tp
                  ON tp.channel_id = mr.channel_id
                 AND tp.point_id = mr.channel_point_id
                 AND mr.channel_type = 'T'
                LEFT JOIN signal_points sp
                  ON sp.channel_id = mr.channel_id
                 AND sp.point_id = mr.channel_point_id
                 AND mr.channel_type = 'S'
                WHERE i.instance_id = ?
                "#
            },
            Self::Action => {
                r#"
                SELECT i.product_name, cr.revision AS routing_revision,
                       ar.channel_id, ar.channel_type, ar.channel_point_id, ar.enabled,
                       c.name AS channel_name,
                       COALESCE(cp.signal_name, ajp.signal_name) AS channel_point_name
                FROM instances i
                JOIN configuration_revisions cr ON cr.scope = 'logical_routing'
                LEFT JOIN action_routing ar
                  ON ar.instance_id = i.instance_id AND ar.action_id = ?
                LEFT JOIN channels c ON c.channel_id = ar.channel_id
                LEFT JOIN control_points cp
                  ON cp.channel_id = ar.channel_id
                 AND cp.point_id = ar.channel_point_id
                 AND ar.channel_type = 'C'
                LEFT JOIN adjustment_points ajp
                  ON ajp.channel_id = ar.channel_id
                 AND ajp.point_id = ar.channel_point_id
                 AND ar.channel_type = 'A'
                WHERE i.instance_id = ?
                "#
            },
        }
    }
}

impl InstanceManager {
    async fn load_point_detail_snapshot(
        &self,
        instance_id: u32,
        point_id: u32,
        plane: PointPlane,
    ) -> Result<PointDetailSnapshot> {
        sqlx::query_as::<_, StoredPointDetail>(plane.query())
            .bind(point_id)
            .bind(instance_id as i64)
            .fetch_one(&self.pool)
            .await
            .map_err(|error| anyhow!("Instance {instance_id} not found: {error}"))?
            .into_snapshot()
    }

    /// Get instance real-time data from the authoritative SHM plane.
    pub async fn get_instance_data(
        &self,
        instance_id: u32,
        data_type: Option<InstanceDataPlane>,
    ) -> Result<InstanceLiveDataView> {
        let instance = self.get_instance(instance_id).await?;
        let product = self
            .product_loader
            .get_definition(instance.product_name())
            .map_err(|error| anyhow!("Product '{}' not found: {error}", instance.product_name()))?;
        // Pin one complete runtime generation for the whole HTTP query. This
        // prevents a response from resolving some points through old routing
        // and others through a newly-published SHM layout.
        let runtime = self.runtime_topology.load();

        let read_points = |point_ids: &[u32], is_action: bool| {
            let mut values = BTreeMap::new();
            for point_id in point_ids {
                let sample = runtime
                    .read_instance_point(instance_id, is_action, *point_id)
                    .ok()
                    .flatten();
                if let Some((value, timestamp_ms)) = sample
                    && value.is_finite()
                {
                    values.insert(
                        *point_id,
                        InstanceLiveSample {
                            value,
                            timestamp_ms,
                        },
                    );
                }
            }
            values
        };

        let measurement_points: Vec<_> =
            product.measurements.iter().map(|point| point.id).collect();
        let action_points: Vec<_> = product.actions.iter().map(|point| point.id).collect();

        match data_type {
            Some(InstanceDataPlane::Measurement) => Ok(InstanceLiveDataView::Values(read_points(
                &measurement_points,
                false,
            ))),
            Some(InstanceDataPlane::Action) => Ok(InstanceLiveDataView::Values(read_points(
                &action_points,
                true,
            ))),
            None => Ok(InstanceLiveDataView::Complete {
                measurements: read_points(&measurement_points, false),
                actions: read_points(&action_points, true),
            }),
        }
    }

    /// Merge one instance's selected product with its revisioned desired routing.
    pub(crate) async fn load_instance_points(
        &self,
        instance_id: u32,
    ) -> Result<InstancePointsView> {
        let identity_query = async {
            Ok::<_, anyhow::Error>(
                sqlx::query_as::<_, (String, String)>(
                    "SELECT instance_name, product_name FROM instances WHERE instance_id = ?",
                )
                .bind(instance_id as i64)
                .fetch_optional(&self.pool)
                .await?,
            )
        };
        let property_query = async {
            Ok::<_, anyhow::Error>(
                sqlx::query_as::<_, (i64, String)>(
                    "SELECT property_id, value_json FROM instance_properties WHERE instance_id = ?",
                )
                .bind(instance_id as i64)
                .fetch_all(&self.pool)
                .await?,
            )
        };
        let routing_query = self.routing_snapshot(RoutingScope::Instance(instance_id));

        let (identity, property_rows, routing_snapshot) =
            tokio::try_join!(identity_query, property_query, routing_query)?;
        let (instance_name, product_name) =
            identity.ok_or_else(|| anyhow!("Instance not found: {instance_id}"))?;

        let product = self
            .product_loader
            .get_product(&product_name)
            .map_err(|error| anyhow!("Product '{product_name}' not found: {error}"))?;

        let mut instance_props_by_id: HashMap<i32, serde_json::Value> =
            HashMap::with_capacity(property_rows.len());
        for (property_id, value_json) in property_rows {
            let property_id = i32::try_from(property_id).map_err(|_| {
                anyhow!("Property ID {property_id} exceeds the supported API range")
            })?;
            let value = serde_json::from_str(&value_json).map_err(|error| {
                anyhow!(
                    "Invalid value_json for instance {instance_id} property {property_id}: {error}"
                )
            })?;
            instance_props_by_id.insert(property_id, value);
        }

        let logical_routing_revision = routing_snapshot.revision();
        let (measurement_routes, action_routes) = routing_snapshot.into_parts();
        let mut measurement_routes: HashMap<_, _> = measurement_routes
            .into_iter()
            .map(|route| (route.point_id, route))
            .collect();
        let mut action_routes: HashMap<_, _> = action_routes
            .into_iter()
            .map(|route| (route.point_id, route))
            .collect();

        let measurements = product
            .measurements
            .into_iter()
            .map(|point| {
                Ok(InstanceMeasurementPointView {
                    routing: measurement_routes
                        .remove(&point.measurement_id)
                        .map(RoutingRoute::into_point_routing)
                        .transpose()?,
                    measurement_id: point.measurement_id,
                    name: point.name,
                    unit: point.unit,
                    description: point.description,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let actions = product
            .actions
            .into_iter()
            .map(|point| {
                Ok(InstanceActionPointView {
                    routing: action_routes
                        .remove(&point.action_id)
                        .map(RoutingRoute::into_point_routing)
                        .transpose()?,
                    action_id: point.action_id,
                    name: point.name,
                    unit: point.unit,
                    description: point.description,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let properties = product
            .properties
            .into_iter()
            .map(|property| InstancePropertyView {
                value: instance_props_by_id.remove(&property.property_id),
                property_id: property.property_id,
                name: property.name,
                unit: property.unit,
                description: property.description,
            })
            .collect();

        Ok(InstancePointsView {
            instance_name,
            measurements,
            actions,
            properties,
            logical_routing_revision,
        })
    }

    /// Load a single measurement point with routing configuration
    pub(crate) async fn load_single_measurement_point(
        &self,
        instance_id: u32,
        point_id: u32,
    ) -> Result<(InstanceMeasurementPointView, u64)> {
        let snapshot = self
            .load_point_detail_snapshot(instance_id, point_id, PointPlane::Measurement)
            .await?;
        let product = self
            .product_loader
            .get_definition(&snapshot.product_name)
            .map_err(|error| anyhow!("Product '{}' not found: {error}", snapshot.product_name))?;
        let mp = product
            .measurements
            .iter()
            .find(|m| m.id == point_id)
            .ok_or_else(|| {
                anyhow!(
                    "Measurement point {} not found in product '{}'",
                    point_id,
                    snapshot.product_name
                )
            })?;
        let mp = crate::product_loader::convert_point_to_measurement(mp);

        Ok((
            InstanceMeasurementPointView {
                measurement_id: mp.measurement_id,
                name: mp.name,
                unit: mp.unit,
                description: mp.description,
                routing: snapshot.routing,
            },
            snapshot.revision,
        ))
    }

    /// Load a single action point with routing configuration
    pub(crate) async fn load_single_action_point(
        &self,
        instance_id: u32,
        point_id: u32,
    ) -> Result<(InstanceActionPointView, u64)> {
        let snapshot = self
            .load_point_detail_snapshot(instance_id, point_id, PointPlane::Action)
            .await?;
        let product = self
            .product_loader
            .get_definition(&snapshot.product_name)
            .map_err(|error| anyhow!("Product '{}' not found: {error}", snapshot.product_name))?;
        let ap = product
            .actions
            .iter()
            .find(|a| a.id == point_id)
            .ok_or_else(|| {
                anyhow!(
                    "Action point {} not found in product '{}'",
                    point_id,
                    snapshot.product_name
                )
            })?;
        let ap = crate::product_loader::convert_point_to_action(ap);

        Ok((
            InstanceActionPointView {
                action_id: ap.action_id,
                name: ap.name,
                unit: ap.unit,
                description: ap.description,
                routing: snapshot.routing,
            },
            snapshot.revision,
        ))
    }
}
