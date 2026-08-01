//! Canonical SQLite projection into the two SHM topology manifests.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use aether_domain::{ChannelCommandAddress, CommandConstraints, PointKind};
use aether_ports::{PortError, PortErrorKind, PortResult};
use aether_shm_bridge::{
    ChannelHealthManifest, ChannelPointManifest, PhysicalPointAddress, ShmClientConfig,
    ShmReadTopologyGeneration,
};
use rustc_hash::FxHasher;
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

/// Point and channel-health manifests observed from one SQLite read transaction.
#[derive(Debug, Clone)]
pub struct SqliteShmTopologySnapshot {
    point_manifest: ChannelPointManifest,
    health_manifest: ChannelHealthManifest,
}

/// Deterministically ordered logical instance route map.
pub type LogicalPointRoutes = BTreeMap<(u32, u32), PhysicalPointAddress>;

/// Validated physical target and limits for one logical command route.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RoutedCommandTarget {
    target: ChannelCommandAddress,
    constraints: CommandConstraints,
}

impl RoutedCommandTarget {
    const fn new(target: ChannelCommandAddress, constraints: CommandConstraints) -> Self {
        Self {
            target,
            constraints,
        }
    }

    /// Returns the command-owned physical target.
    #[must_use]
    pub const fn target(self) -> ChannelCommandAddress {
        self.target
    }

    /// Returns the limits loaded in the same SQLite transaction as the route.
    #[must_use]
    pub const fn constraints(self) -> CommandConstraints {
        self.constraints
    }

    /// Projects the command target into the shared physical topology address.
    #[must_use]
    pub const fn physical_target(self) -> PhysicalPointAddress {
        PhysicalPointAddress::new(
            self.target.channel_id(),
            self.target.kind(),
            self.target.point_id(),
        )
    }
}

/// Deterministically ordered logical command route map.
pub type LogicalCommandRoutes = BTreeMap<(u32, u32), RoutedCommandTarget>;

/// Point, health, and logical routing observed from one SQLite transaction.
#[derive(Debug)]
pub struct SqliteLiveTopologySnapshot {
    shm: SqliteShmTopologySnapshot,
    configured_physical_points: Vec<PhysicalPointAddress>,
    measurement_routes: LogicalPointRoutes,
    action_routes: LogicalCommandRoutes,
    digest: u64,
}

impl SqliteLiveTopologySnapshot {
    /// Returns the physical point manifest.
    #[must_use]
    pub const fn point_manifest(&self) -> &ChannelPointManifest {
        self.shm.point_manifest()
    }

    /// Returns the channel-health manifest.
    #[must_use]
    pub const fn health_manifest(&self) -> &ChannelHealthManifest {
        self.shm.health_manifest()
    }

    /// Returns every configured physical point in canonical SHM address order.
    ///
    /// Sparse manifest holes are omitted. The order is ascending channel id,
    /// then T/S/C/A kind, then point id.
    #[must_use]
    pub fn configured_physical_points(&self) -> &[PhysicalPointAddress] {
        &self.configured_physical_points
    }

    /// Resolves one logical measurement point.
    #[must_use]
    pub fn measurement_route(
        &self,
        instance_id: u32,
        point_id: u32,
    ) -> Option<PhysicalPointAddress> {
        self.measurement_routes
            .get(&(instance_id, point_id))
            .copied()
    }

    /// Resolves one logical action point.
    #[must_use]
    pub fn action_route(&self, instance_id: u32, point_id: u32) -> Option<PhysicalPointAddress> {
        self.command_route(instance_id, point_id)
            .map(RoutedCommandTarget::physical_target)
    }

    /// Resolves one logical command with the constraints from the same snapshot.
    #[must_use]
    pub fn command_route(&self, instance_id: u32, point_id: u32) -> Option<RoutedCommandTarget> {
        self.action_routes.get(&(instance_id, point_id)).copied()
    }

    /// Iterates measurement routes in deterministic logical-address order.
    pub fn measurement_routes(
        &self,
    ) -> impl Iterator<Item = (u32, u32, PhysicalPointAddress)> + '_ {
        self.measurement_routes
            .iter()
            .map(|(&(instance_id, point_id), &target)| (instance_id, point_id, target))
    }

    /// Iterates action routes in deterministic logical-address order.
    pub fn action_routes(&self) -> impl Iterator<Item = (u32, u32, PhysicalPointAddress)> + '_ {
        self.action_routes
            .iter()
            .map(|(&(instance_id, point_id), &target)| {
                (instance_id, point_id, target.physical_target())
            })
    }

    /// Returns the deterministic physical/logical topology digest.
    #[must_use]
    pub const fn digest(&self) -> u64 {
        self.digest
    }

    /// Splits the snapshot for composition roots without another DB read.
    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        ChannelPointManifest,
        ChannelHealthManifest,
        LogicalPointRoutes,
        LogicalCommandRoutes,
    ) {
        let (points, health) = self.shm.into_manifests();
        (points, health, self.measurement_routes, self.action_routes)
    }
}

impl SqliteShmTopologySnapshot {
    /// Returns the deterministic T/S/C/A slot manifest.
    #[must_use]
    pub const fn point_manifest(&self) -> &ChannelPointManifest {
        &self.point_manifest
    }

    /// Returns the configured-channel manifest for the health plane.
    #[must_use]
    pub const fn health_manifest(&self) -> &ChannelHealthManifest {
        &self.health_manifest
    }

    /// Splits the snapshot into owned manifests without another database read.
    #[must_use]
    pub fn into_manifests(self) -> (ChannelPointManifest, ChannelHealthManifest) {
        (self.point_manifest, self.health_manifest)
    }
}

/// Loads point and channel-health topology from one authoritative SQLite snapshot.
///
/// All configured point rows participate, including telemetry and signal rows
/// owned by commissioned channels. This keeps every SHM client on the writer's exact
/// layout hash.
pub async fn load_sqlite_shm_topology(pool: &SqlitePool) -> PortResult<SqliteShmTopologySnapshot> {
    let mut transaction = pool.begin().await.map_err(topology_unavailable)?;
    let snapshot = load_shm_topology(&mut transaction).await?;
    transaction.commit().await.map_err(topology_unavailable)?;
    Ok(snapshot)
}

/// Loads physical manifests and logical measurement/action routes from one
/// authoritative SQLite read transaction.
pub async fn load_sqlite_live_topology(
    pool: &SqlitePool,
) -> PortResult<SqliteLiveTopologySnapshot> {
    let mut transaction = pool.begin().await.map_err(topology_unavailable)?;
    let shm = load_shm_topology(&mut transaction).await?;
    let configured_physical_points = load_configured_physical_points(&mut transaction).await?;
    let measurement_routes = load_routes(
        &mut transaction,
        "measurement_routing",
        "measurement_id",
        false,
        shm.point_manifest(),
        &configured_physical_points,
    )
    .await?;
    let action_routes = load_routes(
        &mut transaction,
        "action_routing",
        "action_id",
        true,
        shm.point_manifest(),
        &configured_physical_points,
    )
    .await?;
    let action_routes = load_command_routes(&mut transaction, action_routes).await?;
    transaction.commit().await.map_err(topology_unavailable)?;
    let digest = live_topology_digest(
        &shm,
        &configured_physical_points,
        &measurement_routes,
        &action_routes,
    );
    Ok(SqliteLiveTopologySnapshot {
        shm,
        configured_physical_points,
        measurement_routes,
        action_routes,
        digest,
    })
}

async fn load_shm_topology(
    connection: &mut SqliteConnection,
) -> PortResult<SqliteShmTopologySnapshot> {
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

    Ok(SqliteShmTopologySnapshot {
        point_manifest: ChannelPointManifest::from_map(counts),
        health_manifest: ChannelHealthManifest::from_channel_ids(channel_ids),
    })
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
    for row in rows {
        let Some((logical, target)) = validate_route(
            row,
            table,
            logical_point_column,
            writable,
            manifest,
            configured_physical_points,
        )?
        else {
            continue;
        };
        if routes.insert(logical, target).is_some() {
            return Err(invalid_topology(format!(
                "{table} contains duplicate logical route {}:{}",
                logical.0, logical.1
            )));
        }
    }
    Ok(routes)
}

async fn load_command_routes(
    connection: &mut SqliteConnection,
    physical_routes: LogicalPointRoutes,
) -> PortResult<LogicalCommandRoutes> {
    let has_adjustment_route = physical_routes
        .values()
        .any(|target| target.kind() == PointKind::Action);
    let mut adjustment_constraints = BTreeMap::new();
    if has_adjustment_route {
        let rows = sqlx::query_as::<_, (i64, i64, Option<f64>, Option<f64>, Option<f64>)>(
            "SELECT DISTINCT adjustment.channel_id, adjustment.point_id, \
                    adjustment.min_value, adjustment.max_value, adjustment.step \
             FROM adjustment_points AS adjustment \
             INNER JOIN action_routing AS routing \
               ON routing.enabled = TRUE \
              AND routing.channel_type = 'A' \
              AND routing.channel_id = adjustment.channel_id \
              AND routing.channel_point_id = adjustment.point_id \
             ORDER BY adjustment.channel_id, adjustment.point_id",
        )
        .fetch_all(&mut *connection)
        .await
        .map_err(topology_unavailable)?;
        for (raw_channel_id, raw_point_id, minimum, maximum, step) in rows {
            let channel_id = stored_u32(
                raw_channel_id,
                "channel_id",
                "adjustment command constraints",
            )?;
            let point_id = stored_u32(raw_point_id, "point_id", "adjustment command constraints")?;
            let constraints = CommandConstraints::new(minimum, maximum, Some(step.unwrap_or(1.0)))
                .map_err(|_| {
                    invalid_topology(format!(
                        "adjustment point {channel_id}:{point_id} has invalid command constraints"
                    ))
                })?;
            adjustment_constraints.insert((channel_id, point_id), constraints);
        }
    }
    let binary_constraints = CommandConstraints::new(Some(0.0), Some(1.0), Some(1.0))
        .map_err(|_| invalid_topology("binary command constraints are invalid"))?;

    physical_routes
        .into_iter()
        .map(|(logical @ (instance_id, action_id), physical_target)| {
            let constraints = match physical_target.kind() {
                PointKind::Command => binary_constraints,
                PointKind::Action => adjustment_constraints
                    .get(&(
                        physical_target.channel_id().get(),
                        physical_target.point_id().get(),
                    ))
                    .copied()
                    .ok_or_else(|| {
                        invalid_topology(format!(
                            "action_routing logical route {instance_id}:{action_id} has no command constraints"
                        ))
                    })?,
                PointKind::Telemetry | PointKind::Status => {
                    return Err(invalid_topology(format!(
                        "action_routing logical route {instance_id}:{action_id} is not command-owned"
                    )));
                },
            };
            let command_target = ChannelCommandAddress::new(
                physical_target.channel_id(),
                physical_target.kind(),
                physical_target.point_id(),
            )
            .map_err(|_| {
                invalid_topology(format!(
                    "action_routing logical route {instance_id}:{action_id} is not command-owned"
                ))
            })?;
            Ok((
                logical,
                RoutedCommandTarget::new(command_target, constraints),
            ))
        })
        .collect()
}

type StoredRouteRow = (i64, Option<i64>, Option<String>, Option<i64>, i64);

fn validate_route(
    row: StoredRouteRow,
    table: &str,
    logical_point_column: &str,
    writable: bool,
    manifest: &ChannelPointManifest,
    configured_physical_points: &[PhysicalPointAddress],
) -> PortResult<Option<((u32, u32), PhysicalPointAddress)>> {
    let (raw_instance_id, raw_channel_id, raw_kind, raw_point_id, raw_logical_point_id) = row;
    let instance_id = stored_u32(raw_instance_id, "instance_id", table)?;
    let (raw_channel_id, raw_kind, raw_point_id) = match (raw_channel_id, raw_kind, raw_point_id) {
        (None, None, None) => return Ok(None),
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
    if configured_physical_points
        .binary_search_by_key(&physical_address_key(target), |candidate| {
            physical_address_key(*candidate)
        })
        .is_err()
    {
        return Err(invalid_topology(format!(
            "{table} route target {channel_id}:{raw_kind}:{point_id} is not a configured physical point"
        )));
    }
    Ok(Some(((instance_id, logical_point_id), target)))
}

fn physical_address_key(address: PhysicalPointAddress) -> (u32, u8, u32) {
    let kind = match address.kind() {
        PointKind::Telemetry => 0,
        PointKind::Status => 1,
        PointKind::Command => 2,
        PointKind::Action => 3,
    };
    (address.channel_id().get(), kind, address.point_id().get())
}

/// Opens the paired point and channel-health read planes for one snapshot.
///
/// `validate_physical` selects an eagerly validated publication; otherwise the
/// generation stays lazy so a reader can start before IO publishes either
/// plane. Both planes are tuned with the caller's staleness and identity
/// intervals so a service never mixes the two SHM client configurations.
pub fn open_read_topology(
    point_manifest: Arc<ChannelPointManifest>,
    health_manifest: Arc<ChannelHealthManifest>,
    point_path: &str,
    health_path: &str,
    writer_stale_after: Duration,
    identity_check_interval: Duration,
    validate_physical: bool,
) -> PortResult<Arc<ShmReadTopologyGeneration>> {
    let client_config = |path: &str, layout_hash: u64| {
        ShmClientConfig::new(path, layout_hash)
            .with_writer_stale_after(writer_stale_after)
            .with_identity_check_interval(identity_check_interval)
    };
    let point_config = client_config(point_path, point_manifest.layout_hash());
    let health_config = client_config(health_path, health_manifest.layout_hash());
    let generation = if validate_physical {
        ShmReadTopologyGeneration::open(
            point_config,
            health_config,
            point_manifest,
            health_manifest,
        )?
    } else {
        ShmReadTopologyGeneration::new_lazy(
            point_config,
            health_config,
            point_manifest,
            health_manifest,
        )?
    };
    Ok(Arc::new(generation))
}

/// Reports whether an open read topology still matches a snapshot's physical
/// layout, so a routing-only change can be published without reopening SHM.
#[must_use]
pub fn layouts_match(
    topology: &ShmReadTopologyGeneration,
    snapshot: &SqliteLiveTopologySnapshot,
) -> bool {
    topology.point_manifest().layout_hash() == snapshot.point_manifest().layout_hash()
        && topology.point_manifest().slot_count() == snapshot.point_manifest().slot_count()
        && topology.health_manifest().layout_hash() == snapshot.health_manifest().layout_hash()
        && topology.health_manifest().slot_count() == snapshot.health_manifest().slot_count()
}

/// Parses the stored T/S/C/A channel-type code used at the SQLite boundary.
#[must_use]
pub fn parse_point_kind(value: &str) -> Option<PointKind> {
    match value {
        "T" => Some(PointKind::Telemetry),
        "S" => Some(PointKind::Status),
        "C" => Some(PointKind::Command),
        "A" => Some(PointKind::Action),
        _ => None,
    }
}

/// Renders a point kind as the stored T/S/C/A channel-type code.
#[must_use]
pub const fn point_kind_code(kind: PointKind) -> &'static str {
    match kind {
        PointKind::Telemetry => "T",
        PointKind::Status => "S",
        PointKind::Command => "C",
        PointKind::Action => "A",
    }
}

fn live_topology_digest(
    shm: &SqliteShmTopologySnapshot,
    configured_physical_points: &[PhysicalPointAddress],
    measurements: &LogicalPointRoutes,
    actions: &LogicalCommandRoutes,
) -> u64 {
    let mut hasher = FxHasher::default();
    "aether.sqlite-live-topology.v3".hash(&mut hasher);
    shm.point_manifest().layout_hash().hash(&mut hasher);
    shm.point_manifest().slot_count().hash(&mut hasher);
    shm.health_manifest().layout_hash().hash(&mut hasher);
    shm.health_manifest().slot_count().hash(&mut hasher);
    configured_physical_points.hash(&mut hasher);
    hash_routes(0, measurements, &mut hasher);
    hash_command_routes(1, actions, &mut hasher);
    hasher.finish()
}

fn hash_command_routes(role: u8, routes: &LogicalCommandRoutes, hasher: &mut FxHasher) {
    role.hash(hasher);
    for (&(instance_id, point_id), &route) in routes {
        instance_id.hash(hasher);
        point_id.hash(hasher);
        route.physical_target().hash(hasher);
        hash_optional_f64(route.constraints().minimum(), hasher);
        hash_optional_f64(route.constraints().maximum(), hasher);
        hash_optional_f64(route.constraints().step(), hasher);
    }
}

fn hash_optional_f64(value: Option<f64>, hasher: &mut FxHasher) {
    value.is_some().hash(hasher);
    if let Some(value) = value {
        if value == 0.0 {
            0.0_f64.to_bits().hash(hasher);
        } else {
            value.to_bits().hash(hasher);
        }
    }
}

fn hash_routes(role: u8, routes: &LogicalPointRoutes, hasher: &mut FxHasher) {
    role.hash(hasher);
    for (&(instance_id, point_id), &target) in routes {
        instance_id.hash(hasher);
        point_id.hash(hasher);
        target.hash(hasher);
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
