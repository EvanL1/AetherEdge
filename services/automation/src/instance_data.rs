//! Instance Data Loading and Query Operations
//!
//! This module provides data loading, querying, and synchronization operations.
//! Extracted from instance_manager.rs for better code organization.

#![allow(clippy::disallowed_methods)] // json! macro used in multiple functions

use anyhow::{Result, anyhow};
use std::collections::HashMap;

use super::instance_manager::InstanceManager;

impl InstanceManager {
    /// Get instance real-time data from the authoritative SHM plane.
    pub async fn get_instance_data(
        &self,
        instance_id: u32,
        data_type: Option<&str>,
    ) -> Result<serde_json::Value> {
        let instance = self.get_instance(instance_id).await?;
        let product = self
            .product_loader
            .get_product(instance.product_name())
            .map_err(|error| anyhow!("Product '{}' not found: {error}", instance.product_name()))?;
        // Pin one complete runtime generation for the whole HTTP query. This
        // prevents a response from resolving some points through old routing
        // and others through a newly-published SHM layout.
        let runtime = self
            .runtime_topology
            .get()
            .ok_or_else(|| anyhow!("coherent runtime topology is unavailable"))?
            .load();

        let read_points = |points: &[(u32, &'static str)]| {
            let mut values = serde_json::Map::new();
            for (point_id, kind) in points {
                let instance_type = if *kind == "action" { 1 } else { 0 };
                let sample = runtime
                    .read_instance_point(instance_id, instance_type != 0, *point_id)
                    .ok()
                    .flatten();
                if let Some((value, timestamp_ms)) = sample
                    && value.is_finite()
                {
                    values.insert(
                        point_id.to_string(),
                        serde_json::json!({
                            "value": value,
                            "timestamp_ms": timestamp_ms,
                        }),
                    );
                }
            }
            values
        };

        let measurement_points: Vec<_> = product
            .measurements
            .iter()
            .map(|point| (point.measurement_id, "measurement"))
            .collect();
        let action_points: Vec<_> = product
            .actions
            .iter()
            .map(|point| (point.action_id, "action"))
            .collect();

        match data_type {
            Some("measurement") => Ok(serde_json::Value::Object(read_points(&measurement_points))),
            Some("action") => Ok(serde_json::Value::Object(read_points(&action_points))),
            None => Ok(serde_json::json!({
                "measurements": read_points(&measurement_points),
                "actions": read_points(&action_points),
            })),
            Some(other) => Err(anyhow!(
                "Unknown data type '{other}'; use 'measurement', 'action', or omit for both"
            )),
        }
    }

    /// Load instance points with routing configuration (runtime merge)
    ///
    /// Returns (measurements, actions, properties). Measurements/actions carry routing;
    /// properties carry per-instance values from the `instance_properties` table
    /// (no routing — properties are static metadata, not data-flow points).
    ///
    /// Query plan:
    /// 1. Fetch `product_name` from `instances`
    /// 2. Look up the product template from the validated active Pack set
    /// 3. Query routing data from `measurement_routing` / `action_routing` (parallel)
    /// 4. Query property values from `instance_properties`
    /// 5. Merge in application layer
    pub async fn load_instance_points(
        &self,
        instance_id: u32,
    ) -> Result<(
        Vec<crate::dto::InstanceMeasurementPoint>,
        Vec<crate::dto::InstanceActionPoint>,
        Vec<crate::dto::InstancePropertyPoint>,
    )> {
        use crate::dto::{
            InstanceActionPoint, InstanceMeasurementPoint, InstancePropertyPoint, PointRouting,
        };

        let product_name: String =
            sqlx::query_scalar("SELECT product_name FROM instances WHERE instance_id = ?")
                .bind(instance_id as i64)
                .fetch_one(&self.pool)
                .await
                .map_err(|e| anyhow!("Instance {} not found: {}", instance_id, e))?;

        let product = self
            .product_loader
            .get_product(&product_name)
            .map_err(|e| anyhow!("Product '{}' not found: {}", product_name, e))?;

        // Property values are keyed by property_id in the dedicated table.
        // Build property_id -> JSON value so the template-driven merge below
        // can look up directly (no name lookup needed).
        let prop_rows: Vec<(i64, String)> = sqlx::query_as(
            "SELECT property_id, value_json FROM instance_properties WHERE instance_id = ?",
        )
        .bind(instance_id as i64)
        .fetch_all(&self.pool)
        .await?;
        let mut instance_props_by_id: HashMap<i32, serde_json::Value> =
            HashMap::with_capacity(prop_rows.len());
        for (property_id, value_json) in prop_rows {
            let value: serde_json::Value = serde_json::from_str(&value_json).map_err(|e| {
                anyhow!(
                    "Invalid value_json for instance {} property {}: {}",
                    instance_id,
                    property_id,
                    e
                )
            })?;
            instance_props_by_id.insert(property_id as i32, value);
        }

        // 2. Query routing data from real tables (parallel)
        let m_routing_query = sqlx::query_as::<
            _,
            (
                u32,        // measurement_id
                Option<i32>,    // channel_id
                Option<String>, // channel_type
                Option<u32>,    // channel_point_id
                Option<bool>,   // enabled
                Option<String>, // channel_name
                Option<String>, // channel_point_name
            ),
        >(
            r#"SELECT mr.measurement_id, mr.channel_id, mr.channel_type, mr.channel_point_id, mr.enabled,
                    c.name AS channel_name,
                    COALESCE(tp.signal_name, sp.signal_name) AS channel_point_name
               FROM measurement_routing mr
               LEFT JOIN channels c ON c.channel_id = mr.channel_id
               LEFT JOIN telemetry_points tp ON tp.channel_id = mr.channel_id AND tp.point_id = mr.channel_point_id AND mr.channel_type = 'T'
               LEFT JOIN signal_points sp ON sp.channel_id = mr.channel_id AND sp.point_id = mr.channel_point_id AND mr.channel_type = 'S'
               WHERE mr.instance_id = ?"#,
        )
        .bind(instance_id as i64)
        .fetch_all(&self.pool);

        let a_routing_query = sqlx::query_as::<
            _,
            (
                u32,        // action_id
                Option<i32>,    // channel_id
                Option<String>, // channel_type
                Option<u32>,    // channel_point_id
                Option<bool>,   // enabled
                Option<String>, // channel_name
                Option<String>, // channel_point_name
            ),
        >(
            r#"SELECT ar.action_id, ar.channel_id, ar.channel_type, ar.channel_point_id, ar.enabled,
                    c.name AS channel_name,
                    COALESCE(cp.signal_name, ajp.signal_name) AS channel_point_name
               FROM action_routing ar
               LEFT JOIN channels c ON c.channel_id = ar.channel_id
               LEFT JOIN control_points cp ON cp.channel_id = ar.channel_id AND cp.point_id = ar.channel_point_id AND ar.channel_type = 'C'
               LEFT JOIN adjustment_points ajp ON ajp.channel_id = ar.channel_id AND ajp.point_id = ar.channel_point_id AND ar.channel_type = 'A'
               WHERE ar.instance_id = ?"#,
        )
        .bind(instance_id as i64)
        .fetch_all(&self.pool);

        let (m_routing_rows, a_routing_rows) = tokio::try_join!(m_routing_query, a_routing_query)?;

        // 3. Merge: product point definitions + routing data
        let mut m_routing_map: HashMap<u32, _> =
            m_routing_rows.into_iter().map(|r| (r.0, r)).collect();

        let measurements = product
            .measurements
            .iter()
            .map(|mp| {
                let routing = m_routing_map.remove(&mp.measurement_id).and_then(
                    |(_, cid, ctype, cpid, enabled, cname, cpname)| match (ctype, enabled) {
                        (Some(t), Some(e)) => Some(PointRouting {
                            channel_id: cid,
                            channel_type: Some(t),
                            channel_point_id: cpid,
                            enabled: e,
                            channel_name: cname,
                            channel_point_name: cpname,
                        }),
                        _ => None,
                    },
                );
                InstanceMeasurementPoint {
                    measurement_id: mp.measurement_id,
                    name: mp.name.clone(),
                    unit: mp.unit.clone(),
                    description: mp.description.clone(),
                    routing,
                }
            })
            .collect();

        let mut a_routing_map: HashMap<u32, _> =
            a_routing_rows.into_iter().map(|r| (r.0, r)).collect();

        let actions = product
            .actions
            .iter()
            .map(|ap| {
                let routing = a_routing_map.remove(&ap.action_id).and_then(
                    |(_, cid, ctype, cpid, enabled, cname, cpname)| match (ctype, enabled) {
                        (Some(t), Some(e)) => Some(PointRouting {
                            channel_id: cid,
                            channel_type: Some(t),
                            channel_point_id: cpid,
                            enabled: e,
                            channel_name: cname,
                            channel_point_name: cpname,
                        }),
                        _ => None,
                    },
                );
                InstanceActionPoint {
                    action_id: ap.action_id,
                    name: ap.name.clone(),
                    unit: ap.unit.clone(),
                    description: ap.description.clone(),
                    routing,
                }
            })
            .collect();

        // Properties: merge product template with per-instance value (no routing).
        let properties = product
            .properties
            .iter()
            .map(|pt| InstancePropertyPoint {
                property_id: pt.property_id,
                name: pt.name.clone(),
                unit: pt.unit.clone(),
                description: pt.description.clone(),
                value: instance_props_by_id.remove(&pt.property_id),
            })
            .collect();

        Ok((measurements, actions, properties))
    }

    /// Get instance points from the selected runtime product library.
    pub async fn get_instance_points(
        &self,
        instance_id: u32,
        data_type: Option<&str>,
    ) -> Result<serde_json::Value> {
        let product_name: Option<String> =
            sqlx::query_scalar("SELECT product_name FROM instances WHERE instance_id = ?")
                .bind(instance_id as i64)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| anyhow!("Failed to load instance {} metadata: {}", instance_id, e))?;

        let Some(product_name) = product_name else {
            return Err(anyhow!("Instance {} not found", instance_id));
        };

        // Resolve the product from the validated Pack-backed library.
        let product = self
            .product_loader
            .get_product(&product_name)
            .map_err(|e| anyhow!("Product '{}' not found: {}", product_name, e))?;

        // Load property values from instance_properties table when needed.
        // Returns a map keyed by property name so the JSON response mirrors
        // the legacy `instances.properties` JSON shape callers expect.
        let load_props_map = || async {
            let rows: Vec<(i64, String)> = sqlx::query_as(
                "SELECT property_id, value_json FROM instance_properties WHERE instance_id = ?",
            )
            .bind(instance_id as i64)
            .fetch_all(&self.pool)
            .await?;
            let mut props = serde_json::Map::new();
            for (property_id, value_json) in rows {
                let Some(tpl) = product
                    .properties
                    .iter()
                    .find(|p| i64::from(p.property_id) == property_id)
                else {
                    continue;
                };
                let value: serde_json::Value = serde_json::from_str(&value_json)?;
                props.insert(tpl.name.clone(), value);
            }
            Ok::<_, anyhow::Error>(props)
        };

        match data_type {
            Some("measurement") => {
                let mut result = serde_json::Map::new();
                for m in &product.measurements {
                    let point = serde_json::json!({
                        "measurement_id": m.measurement_id,
                        "name": &m.name,
                        "unit": &m.unit,
                        "description": &m.description
                    });
                    result.insert(m.name.clone(), point);
                }
                Ok(serde_json::Value::Object(result))
            },
            Some("action") => {
                let mut result = serde_json::Map::new();
                for a in &product.actions {
                    let point = serde_json::json!({
                        "action_id": a.action_id,
                        "name": &a.name,
                        "unit": &a.unit,
                        "description": &a.description
                    });
                    result.insert(a.name.clone(), point);
                }
                Ok(serde_json::Value::Object(result))
            },
            Some("property") => {
                let props = load_props_map().await?;
                Ok(serde_json::Value::Object(props))
            },
            None => {
                let mut m_map = serde_json::Map::new();
                for m in &product.measurements {
                    let point = serde_json::json!({
                        "measurement_id": m.measurement_id,
                        "name": &m.name,
                        "unit": &m.unit,
                        "description": &m.description
                    });
                    m_map.insert(m.name.clone(), point);
                }

                let mut a_map = serde_json::Map::new();
                for a in &product.actions {
                    let point = serde_json::json!({
                        "action_id": a.action_id,
                        "name": &a.name,
                        "unit": &a.unit,
                        "description": &a.description
                    });
                    a_map.insert(a.name.clone(), point);
                }

                let properties = load_props_map().await?;

                Ok(serde_json::json!({
                    "measurements": m_map,
                    "actions": a_map,
                    "properties": properties
                }))
            },
            Some(other) => Err(anyhow!(
                "Unknown data type '{}'; use 'measurement', 'action', 'property', or omit for all",
                other
            )),
        }
    }

    /// Load a single measurement point with routing configuration
    pub async fn load_single_measurement_point(
        &self,
        instance_id: u32,
        point_id: u32,
    ) -> Result<crate::dto::InstanceMeasurementPoint> {
        use crate::dto::{InstanceMeasurementPoint, PointRouting};

        // 1. Get product_name and product definition
        let product_name = sqlx::query_scalar::<_, String>(
            "SELECT product_name FROM instances WHERE instance_id = ?",
        )
        .bind(instance_id as i64)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| anyhow!("Instance {} not found: {}", instance_id, e))?;

        let product = self
            .product_loader
            .get_product(&product_name)
            .map_err(|e| anyhow!("Product '{}' not found: {}", product_name, e))?;

        // 2. Find the measurement point in the selected product.
        let mp = product
            .measurements
            .iter()
            .find(|m| m.measurement_id == point_id)
            .ok_or_else(|| {
                anyhow!(
                    "Measurement point {} not found in product '{}'",
                    point_id,
                    product_name
                )
            })?;

        // 3. Query routing for this specific point
        let routing_row = sqlx::query_as::<
            _,
            (
                Option<i32>,    // channel_id
                Option<String>, // channel_type
                Option<u32>,    // channel_point_id
                Option<bool>,   // enabled
                Option<String>, // channel_name
                Option<String>, // channel_point_name
            ),
        >(
            r#"SELECT mr.channel_id, mr.channel_type, mr.channel_point_id, mr.enabled,
                    c.name AS channel_name,
                    COALESCE(tp.signal_name, sp.signal_name) AS channel_point_name
               FROM measurement_routing mr
               LEFT JOIN channels c ON c.channel_id = mr.channel_id
               LEFT JOIN telemetry_points tp ON tp.channel_id = mr.channel_id AND tp.point_id = mr.channel_point_id AND mr.channel_type = 'T'
               LEFT JOIN signal_points sp ON sp.channel_id = mr.channel_id AND sp.point_id = mr.channel_point_id AND mr.channel_type = 'S'
               WHERE mr.instance_id = ? AND mr.measurement_id = ?"#,
        )
        .bind(instance_id as i64)
        .bind(point_id)
        .fetch_optional(&self.pool)
        .await?;

        let routing = routing_row.and_then(|(cid, ctype, cpid, enabled, cname, cpname)| {
            match (ctype, enabled) {
                (Some(t), Some(e)) => Some(PointRouting {
                    channel_id: cid,
                    channel_type: Some(t),
                    channel_point_id: cpid,
                    enabled: e,
                    channel_name: cname,
                    channel_point_name: cpname,
                }),
                _ => None,
            }
        });

        Ok(InstanceMeasurementPoint {
            measurement_id: mp.measurement_id,
            name: mp.name.clone(),
            unit: mp.unit.clone(),
            description: mp.description.clone(),
            routing,
        })
    }

    /// Load a single action point with routing configuration
    pub async fn load_single_action_point(
        &self,
        instance_id: u32,
        point_id: u32,
    ) -> Result<crate::dto::InstanceActionPoint> {
        use crate::dto::{InstanceActionPoint, PointRouting};

        // 1. Get product_name and product definition
        let product_name = sqlx::query_scalar::<_, String>(
            "SELECT product_name FROM instances WHERE instance_id = ?",
        )
        .bind(instance_id as i64)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| anyhow!("Instance {} not found: {}", instance_id, e))?;

        let product = self
            .product_loader
            .get_product(&product_name)
            .map_err(|e| anyhow!("Product '{}' not found: {}", product_name, e))?;

        // 2. Find the action point in the selected product.
        let ap = product
            .actions
            .iter()
            .find(|a| a.action_id == point_id)
            .ok_or_else(|| {
                anyhow!(
                    "Action point {} not found in product '{}'",
                    point_id,
                    product_name
                )
            })?;

        // 3. Query routing for this specific point
        let routing_row = sqlx::query_as::<
            _,
            (
                Option<i32>,    // channel_id
                Option<String>, // channel_type
                Option<u32>,    // channel_point_id
                Option<bool>,   // enabled
                Option<String>, // channel_name
                Option<String>, // channel_point_name
            ),
        >(
            r#"SELECT ar.channel_id, ar.channel_type, ar.channel_point_id, ar.enabled,
                    c.name AS channel_name,
                    COALESCE(cp.signal_name, ajp.signal_name) AS channel_point_name
               FROM action_routing ar
               LEFT JOIN channels c ON c.channel_id = ar.channel_id
               LEFT JOIN control_points cp ON cp.channel_id = ar.channel_id AND cp.point_id = ar.channel_point_id AND ar.channel_type = 'C'
               LEFT JOIN adjustment_points ajp ON ajp.channel_id = ar.channel_id AND ajp.point_id = ar.channel_point_id AND ar.channel_type = 'A'
               WHERE ar.instance_id = ? AND ar.action_id = ?"#,
        )
        .bind(instance_id as i64)
        .bind(point_id)
        .fetch_optional(&self.pool)
        .await?;

        let routing = routing_row.and_then(|(cid, ctype, cpid, enabled, cname, cpname)| {
            match (ctype, enabled) {
                (Some(t), Some(e)) => Some(PointRouting {
                    channel_id: cid,
                    channel_type: Some(t),
                    channel_point_id: cpid,
                    enabled: e,
                    channel_name: cname,
                    channel_point_name: cpname,
                }),
                _ => None,
            }
        });

        Ok(InstanceActionPoint {
            action_id: ap.action_id,
            name: ap.name.clone(),
            unit: ap.unit.clone(),
            description: ap.description.clone(),
            routing,
        })
    }
}
