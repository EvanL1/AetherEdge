//! SQLite adapter for commissioned physical topology and routing definitions.

use std::collections::BTreeMap;

use aether_domain::PointKind;
use aether_ports::{PortError, PortErrorKind, PortResult};
use aether_routing::{ChannelRoute, LogicalPointRoutes, PhysicalTopologySnapshot, RoutingSnapshot};
use aether_shm_bridge::{ChannelHealthManifest, ChannelPointManifest, PhysicalPointAddress};
use sqlx::{SqliteConnection, SqlitePool};

const POINT_COUNT_QUERIES: [(&str, &str, usize); 4] = [
    (
        "SELECT channel_id, MIN(point_id), MAX(point_id) + 1, COUNT(*), COUNT(DISTINCT point_id) FROM telemetry_points GROUP BY channel_id",
        "telemetry_points",
        0,
    ),
    (
        "SELECT channel_id, MIN(point_id), MAX(point_id) + 1, COUNT(*), COUNT(DISTINCT point_id) FROM signal_points GROUP BY channel_id",
        "signal_points",
        1,
    ),
    (
        "SELECT channel_id, MIN(point_id), MAX(point_id) + 1, COUNT(*), COUNT(DISTINCT point_id) FROM control_points GROUP BY channel_id",
        "control_points",
        2,
    ),
    (
        "SELECT channel_id, MIN(point_id), MAX(point_id) + 1, COUNT(*), COUNT(DISTINCT point_id) FROM adjustment_points GROUP BY channel_id",
        "adjustment_points",
        3,
    ),
];

const CONFIGURED_POINT_QUERY: &str = "SELECT channel_id, 0 AS kind_index, point_id FROM telemetry_points \
     UNION ALL \
     SELECT channel_id, 1 AS kind_index, point_id FROM signal_points \
     UNION ALL \
     SELECT channel_id, 2 AS kind_index, point_id FROM control_points \
     UNION ALL \
     SELECT channel_id, 3 AS kind_index, point_id FROM adjustment_points \
     ORDER BY channel_id, kind_index, point_id";

/// Loads point and channel-health topology from one local-store transaction.
pub async fn load_physical_topology(pool: &SqlitePool) -> PortResult<PhysicalTopologySnapshot> {
    let mut transaction = pool.begin().await.map_err(topology_unavailable)?;
    let snapshot = load_physical_topology_from(&mut transaction).await?;
    transaction.commit().await.map_err(topology_unavailable)?;
    Ok(snapshot)
}

/// Loads physical topology and logical C2M/M2C routes from one transaction.
pub async fn load_routing_snapshot(pool: &SqlitePool) -> PortResult<RoutingSnapshot> {
    let mut transaction = pool.begin().await.map_err(topology_unavailable)?;
    let physical = load_physical_topology_from(&mut transaction).await?;
    let configured_physical_points = load_configured_physical_points(&mut transaction).await?;
    let measurement_routes = load_routes(
        &mut transaction,
        "measurement_routing",
        "measurement_id",
        false,
        physical.point_manifest(),
        &configured_physical_points,
    )
    .await?;
    let action_routes = load_routes(
        &mut transaction,
        "action_routing",
        "action_id",
        true,
        physical.point_manifest(),
        &configured_physical_points,
    )
    .await?;
    transaction.commit().await.map_err(topology_unavailable)?;
    RoutingSnapshot::new(
        physical,
        configured_physical_points,
        measurement_routes,
        action_routes,
    )
}

async fn load_physical_topology_from(
    connection: &mut SqliteConnection,
) -> PortResult<PhysicalTopologySnapshot> {
    let mut counts = BTreeMap::<u32, [u32; 4]>::new();

    for (query, table, kind_index) in POINT_COUNT_QUERIES {
        let rows = sqlx::query_as::<_, (i64, i64, i64, i64, i64)>(query)
            .fetch_all(&mut *connection)
            .await
            .map_err(topology_unavailable)?;
        for (
            raw_channel_id,
            raw_min_point_id,
            raw_upper_bound,
            raw_row_count,
            raw_distinct_count,
        ) in rows
        {
            let channel_id = stored_u32(raw_channel_id, "channel_id", table)?;
            stored_u32(raw_min_point_id, "point_id", table)?;
            let count = stored_u32(raw_upper_bound, "point count", table)?;
            let row_count = stored_u32(raw_row_count, "point row count", table)?;
            let distinct_count = stored_u32(raw_distinct_count, "distinct point count", table)?;
            if row_count != distinct_count {
                return Err(invalid_topology(format!(
                    "{table} channel {channel_id} contains duplicate point identifiers"
                )));
            }
            counts.entry(channel_id).or_insert([0; 4])[kind_index] = count;
        }
    }

    let raw_channel_ids =
        sqlx::query_scalar::<_, i64>("SELECT channel_id FROM channels ORDER BY channel_id")
            .fetch_all(&mut *connection)
            .await
            .map_err(topology_unavailable)?;
    let channel_ids = raw_channel_ids
        .into_iter()
        .map(|channel_id| stored_u32(channel_id, "channel_id", "channels"))
        .collect::<PortResult<Vec<_>>>()?;
    if let Some(orphan_channel_id) = counts
        .keys()
        .copied()
        .find(|channel_id| channel_ids.binary_search(channel_id).is_err())
    {
        return Err(invalid_topology(format!(
            "point topology references channel {orphan_channel_id}, which is absent from channels"
        )));
    }

    Ok(PhysicalTopologySnapshot::new(
        ChannelPointManifest::from_map(counts),
        ChannelHealthManifest::from_channel_ids(channel_ids),
    ))
}

async fn load_configured_physical_points(
    connection: &mut SqliteConnection,
) -> PortResult<Vec<PhysicalPointAddress>> {
    let rows = sqlx::query_as::<_, (i64, i64, i64)>(CONFIGURED_POINT_QUERY)
        .fetch_all(&mut *connection)
        .await
        .map_err(topology_unavailable)?;
    rows.into_iter()
        .map(|(raw_channel_id, raw_kind_index, raw_point_id)| {
            let channel_id = stored_u32(raw_channel_id, "channel_id", "physical point tables")?;
            let point_id = stored_u32(raw_point_id, "point_id", "physical point tables")?;
            let kind = match raw_kind_index {
                0 => PointKind::Telemetry,
                1 => PointKind::Status,
                2 => PointKind::Command,
                3 => PointKind::Action,
                _ => {
                    return Err(invalid_topology(
                        "physical point query returned an unknown point kind",
                    ));
                },
            };
            Ok(PhysicalPointAddress::from_legacy_raw(
                channel_id, kind, point_id,
            ))
        })
        .collect()
}

async fn load_routes(
    connection: &mut SqliteConnection,
    table: &str,
    logical_point_column: &str,
    writable: bool,
    manifest: &ChannelPointManifest,
    configured_physical_points: &[PhysicalPointAddress],
) -> PortResult<LogicalPointRoutes> {
    let query = format!(
        "SELECT instance_id, channel_id, channel_type, channel_point_id, {logical_point_column} \
         FROM {table} WHERE enabled = TRUE \
         ORDER BY instance_id, {logical_point_column}, channel_id, channel_type, channel_point_id"
    );
    let rows = sqlx::query_as::<_, (i64, Option<i64>, Option<String>, Option<i64>, i64)>(&query)
        .fetch_all(&mut *connection)
        .await
        .map_err(topology_unavailable)?;
    let mut routes = BTreeMap::new();
    for (raw_instance_id, raw_channel_id, raw_kind, raw_point_id, raw_logical_point_id) in rows {
        let instance_id = stored_u32(raw_instance_id, "instance_id", table)?;
        let (raw_channel_id, raw_kind, raw_point_id) = match (
            raw_channel_id,
            raw_kind,
            raw_point_id,
        ) {
            (None, None, None) => continue,
            (Some(channel_id), Some(kind), Some(point_id)) => (channel_id, kind, point_id),
            _ => {
                return Err(invalid_topology(format!(
                    "{table} logical route {instance_id}:{raw_logical_point_id} has a partial physical binding"
                )));
            },
        };
        let channel_id = stored_u32(raw_channel_id, "channel_id", table)?;
        let point_id = stored_u32(raw_point_id, "channel_point_id", table)?;
        let logical_point_id = stored_u32(raw_logical_point_id, logical_point_column, table)?;
        let kind = parse_point_kind(&raw_kind).ok_or_else(|| {
            invalid_topology(format!(
                "stored channel_type in {table} is not one of T/S/C/A"
            ))
        })?;
        if kind.is_writable() != writable {
            return Err(invalid_topology(format!(
                "{table} route kind {raw_kind} violates its read/write ownership"
            )));
        }
        let target = PhysicalPointAddress::from_legacy_raw(channel_id, kind, point_id);
        if manifest.slot_for(target).is_none() {
            return Err(invalid_topology(format!(
                "{table} route target {channel_id}:{raw_kind}:{point_id} is absent from the point manifest"
            )));
        }
        if !configured_physical_points.contains(&target) {
            return Err(invalid_topology(format!(
                "{table} route target {channel_id}:{raw_kind}:{point_id} is not a configured physical point"
            )));
        }
        if routes
            .insert((instance_id, logical_point_id), target)
            .is_some()
        {
            return Err(invalid_topology(format!(
                "{table} contains duplicate logical route {instance_id}:{logical_point_id}"
            )));
        }
    }
    Ok(routes)
}

/// Loads the IO-owned C2C routes from one local-store transaction.
pub async fn load_channel_routes(pool: &SqlitePool) -> PortResult<Vec<ChannelRoute>> {
    let mut transaction = pool.begin().await.map_err(topology_unavailable)?;
    let physical = load_physical_topology_from(&mut transaction).await?;
    let configured = load_configured_physical_points(&mut transaction).await?;
    let rows = sqlx::query_as::<_, (i64, String, i64, i64, String, i64, f64, f64)>(
        "SELECT source_channel_id, source_type, source_point_id, \
         target_channel_id, target_type, target_point_id, scale, offset \
         FROM channel_routing WHERE enabled = TRUE \
         ORDER BY source_channel_id, source_type, source_point_id",
    )
    .fetch_all(&mut *transaction)
    .await;
    let rows = match rows {
        Ok(rows) => rows,
        Err(error) if error.to_string().contains("no such table") => Vec::new(),
        Err(error) => return Err(topology_unavailable(error)),
    };
    let mut routes = Vec::with_capacity(rows.len());
    for (
        raw_source_channel,
        source_kind,
        raw_source_point,
        raw_target_channel,
        target_kind,
        raw_target_point,
        scale,
        offset,
    ) in rows
    {
        let source = PhysicalPointAddress::from_legacy_raw(
            stored_u32(raw_source_channel, "source_channel_id", "channel_routing")?,
            parse_point_kind(&source_kind).ok_or_else(|| {
                invalid_topology("channel_routing source_type is not one of T/S/C/A")
            })?,
            stored_u32(raw_source_point, "source_point_id", "channel_routing")?,
        );
        let target = PhysicalPointAddress::from_legacy_raw(
            stored_u32(raw_target_channel, "target_channel_id", "channel_routing")?,
            parse_point_kind(&target_kind).ok_or_else(|| {
                invalid_topology("channel_routing target_type is not one of T/S/C/A")
            })?,
            stored_u32(raw_target_point, "target_point_id", "channel_routing")?,
        );
        for address in [source, target] {
            if physical.point_manifest().slot_for(address).is_none()
                || !configured.contains(&address)
            {
                return Err(invalid_topology(
                    "channel_routing references an unconfigured physical point",
                ));
            }
        }
        routes.push(ChannelRoute::new(source, target, scale, offset)?);
    }
    transaction.commit().await.map_err(topology_unavailable)?;
    Ok(routes)
}

fn parse_point_kind(value: &str) -> Option<PointKind> {
    match value {
        "T" => Some(PointKind::Telemetry),
        "S" => Some(PointKind::Status),
        "C" => Some(PointKind::Command),
        "A" => Some(PointKind::Action),
        _ => None,
    }
}

fn stored_u32(value: i64, field: &str, table: &str) -> PortResult<u32> {
    u32::try_from(value).map_err(|_| {
        PortError::new(
            PortErrorKind::InvalidData,
            format!("stored {field} in {table} is outside the u32 range"),
        )
    })
}

fn invalid_topology(message: impl Into<String>) -> PortError {
    PortError::new(PortErrorKind::InvalidData, message)
}

fn topology_unavailable(_error: sqlx::Error) -> PortError {
    PortError::new(
        PortErrorKind::Unavailable,
        "authoritative SQLite topology is unavailable",
    )
}
