//! Channel-Instance Point Routing Data Types
//!
//! Owns the one revision-consistent desired-routing read used by HTTP queries.

use sqlx::SqlitePool;

#[derive(Debug, Clone, Copy)]
pub(crate) enum RoutingScope {
    All,
    Instance(u32),
    Channel(u32),
}

impl RoutingScope {
    const fn filters(self) -> (Option<i64>, Option<i64>) {
        match self {
            Self::All => (None, None),
            Self::Instance(id) => (Some(id as i64), None),
            Self::Channel(id) => (None, Some(id as i64)),
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
struct StoredRoutingRow {
    routing_id: i64,
    instance_id: u32,
    instance_name: String,
    plane: String,
    point_id: u32,
    channel_id: u32,
    channel_type: String,
    channel_point_id: u32,
    channel_name: Option<String>,
    channel_point_name: Option<String>,
    enabled: bool,
}

/// Plane-neutral route fields; the containing snapshot vector owns direction.
#[derive(Debug)]
pub(crate) struct RoutingRoute {
    pub routing_id: i64,
    pub instance_id: u32,
    pub instance_name: String,
    pub point_id: u32,
    pub channel_id: u32,
    pub channel_type: String,
    pub channel_point_id: u32,
    pub channel_name: Option<String>,
    pub channel_point_name: Option<String>,
    pub enabled: bool,
}

/// Desired routing rows and their shared CAS head from one SQLite read snapshot.
#[derive(Debug)]
pub(crate) struct RoutingSnapshot {
    revision: u64,
    measurements: Vec<RoutingRoute>,
    actions: Vec<RoutingRoute>,
}

impl RoutingSnapshot {
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    pub fn into_parts(self) -> (Vec<RoutingRoute>, Vec<RoutingRoute>) {
        (self.measurements, self.actions)
    }
}

pub(crate) async fn load_routing_snapshot(
    pool: &SqlitePool,
    scope: RoutingScope,
) -> anyhow::Result<RoutingSnapshot> {
    let (instance_id, channel_id) = scope.filters();
    let mut transaction = pool.begin().await?;
    let revision: i64 = sqlx::query_scalar(
        "SELECT revision FROM configuration_revisions WHERE scope = 'logical_routing'",
    )
    .fetch_one(&mut *transaction)
    .await?;
    let revision = decode_revision(revision)?;
    let rows = sqlx::query_as::<_, StoredRoutingRow>(
        r#"
        SELECT mr.routing_id, mr.instance_id, mr.instance_name, 'measurement' AS plane,
               mr.measurement_id AS point_id, mr.channel_id, mr.channel_type,
               mr.channel_point_id, c.name AS channel_name,
               COALESCE(tp.signal_name, sp.signal_name) AS channel_point_name,
               mr.enabled
        FROM measurement_routing mr
        LEFT JOIN channels c ON c.channel_id = mr.channel_id
        LEFT JOIN telemetry_points tp
          ON tp.channel_id = mr.channel_id
         AND tp.point_id = mr.channel_point_id
         AND mr.channel_type = 'T'
        LEFT JOIN signal_points sp
          ON sp.channel_id = mr.channel_id
         AND sp.point_id = mr.channel_point_id
         AND mr.channel_type = 'S'
        WHERE (? IS NULL OR mr.instance_id = ?)
          AND (? IS NULL OR mr.channel_id = ?)
        UNION ALL
        SELECT ar.routing_id, ar.instance_id, ar.instance_name, 'action' AS plane,
               ar.action_id AS point_id, ar.channel_id, ar.channel_type,
               ar.channel_point_id, c.name AS channel_name,
               COALESCE(cp.signal_name, ajp.signal_name) AS channel_point_name,
               ar.enabled
        FROM action_routing ar
        LEFT JOIN channels c ON c.channel_id = ar.channel_id
        LEFT JOIN control_points cp
          ON cp.channel_id = ar.channel_id
         AND cp.point_id = ar.channel_point_id
         AND ar.channel_type = 'C'
        LEFT JOIN adjustment_points ajp
          ON ajp.channel_id = ar.channel_id
         AND ajp.point_id = ar.channel_point_id
         AND ar.channel_type = 'A'
        WHERE (? IS NULL OR ar.instance_id = ?)
          AND (? IS NULL OR ar.channel_id = ?)
        ORDER BY instance_id, plane, point_id
        "#,
    )
    .bind(instance_id)
    .bind(instance_id)
    .bind(channel_id)
    .bind(channel_id)
    .bind(instance_id)
    .bind(instance_id)
    .bind(channel_id)
    .bind(channel_id)
    .fetch_all(&mut *transaction)
    .await?;
    transaction.commit().await?;

    let mut measurements = Vec::new();
    let mut actions = Vec::new();
    for row in rows {
        let route = RoutingRoute {
            routing_id: row.routing_id,
            instance_id: row.instance_id,
            instance_name: row.instance_name,
            point_id: row.point_id,
            channel_id: row.channel_id,
            channel_type: row.channel_type,
            channel_point_id: row.channel_point_id,
            channel_name: row.channel_name,
            channel_point_name: row.channel_point_name,
            enabled: row.enabled,
        };
        match row.plane.as_str() {
            "measurement" => measurements.push(route),
            "action" => actions.push(route),
            plane => anyhow::bail!("unknown stored routing plane: {plane}"),
        }
    }

    Ok(RoutingSnapshot {
        revision,
        measurements,
        actions,
    })
}

pub(crate) fn decode_revision(revision: i64) -> anyhow::Result<u64> {
    u64::try_from(revision)
        .ok()
        .filter(|revision| *revision > 0)
        .ok_or_else(|| anyhow::anyhow!("logical-routing revision must be positive"))
}
