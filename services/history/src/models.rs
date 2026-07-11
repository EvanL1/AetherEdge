use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

// ── Core data types ───────────────────────────────────────────────────────────

/// One measurement point ready to be written to storage.
#[derive(Debug, Clone)]
pub struct DataPoint {
    pub time: DateTime<Utc>,
    /// Stable logical series key, e.g. `inst:1:M`.
    pub series_key: String,
    /// Point identifier inside the logical series, e.g. `"42"`.
    pub point_id: String,
    pub value: Option<f64>,
    pub string_value: Option<String>,
}

/// One row returned from a historical query.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HistoryRecord {
    pub timestamp: String,
    pub series_key: String,
    pub point_id: String,
    pub value: Option<f64>,
    /// Source prefix, derived from the first segment of the logical key.
    pub source: String,
}

// ── Query models ──────────────────────────────────────────────────────────────

// ── Batch query models ────────────────────────────────────────────────────────

/// One (`series_key`, `point_id`) pair in a batch query request.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct BatchSeriesItem {
    pub series_key: String,
    pub point_id: String,
}

/// Request body for `POST /hisApi/data/batch-query`.
#[derive(Debug, Deserialize, ToSchema)]
#[schema(example = json!({
    "start_time": "2026-05-11T02:14:34.712Z",
    "end_time":   "2026-05-11T08:14:34.712Z",
    "series": [
        { "series_key": "inst:9:M",  "point_id": "101" },
        { "series_key": "inst:9:M",  "point_id": "102" },
        { "series_key": "inst:12:M", "point_id": "201" }
    ],
    "limit_per_series": 500
}))]
pub struct BatchQueryRequest {
    pub start_time: String,
    pub end_time: String,
    /// 最多 20 条 series
    pub series: Vec<BatchSeriesItem>,
    /// 每条 series 最多返回的数据点数，默认 1000，最大 5000
    pub limit_per_series: Option<i64>,
}

/// One data point in a batch query response.
#[derive(Debug, Serialize, ToSchema)]
pub struct SeriesPoint {
    pub time: String,
    pub value: Option<f64>,
}

/// Query result for one (`series_key`, `point_id`) series.
#[derive(Debug, Serialize, ToSchema)]
pub struct SeriesResult {
    pub series_key: String,
    pub point_id: String,
    pub count: usize,
    pub data: Vec<SeriesPoint>,
}

/// Response body for `POST /hisApi/data/batch-query`.
#[derive(Debug, Serialize, ToSchema)]
pub struct BatchQueryResponse {
    pub start_time: String,
    pub end_time: String,
    pub series: Vec<SeriesResult>,
}

/// Query string parameters for `GET /hisApi/data/query`.
#[derive(Debug, Deserialize, IntoParams)]
pub struct QueryRangeParams {
    pub series_key: String,
    pub point_id: String,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub page: Option<i64>,
    pub page_size: Option<i64>,
}

/// Query string parameters for `GET /hisApi/data/latest`.
#[derive(Debug, Deserialize, IntoParams)]
pub struct LatestParams {
    pub series_key: String,
    pub point_id: String,
}

/// Paginated query result.
#[allow(dead_code)]
#[derive(Debug, Serialize, ToSchema)]
pub struct QueryResult {
    pub status: String,
    pub message: String,
    pub data: Vec<HistoryRecord>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub has_more: bool,
}

/// Response for `GET /hisApi/data/range`.
#[derive(Debug, Serialize, ToSchema)]
pub struct DataStats {
    pub earliest_timestamp: Option<String>,
    pub latest_timestamp: Option<String>,
    pub total_points: i64,
    pub channels: Vec<String>,
    pub data_types: Vec<String>,
}

// ── Dynamic service configuration ────────────────────────────────────────────

/// One entry in `subscribe_patterns`: a logical-series glob with an optional
/// per-pattern collection-interval override.
///
/// Serialized as a JSON object `{"pattern": interval_secs_or_null}`, e.g.:
/// ```json
/// { "inst:*:M": null, "inst:4:M": 60 }
/// ```
/// `null`, `""`, or `0` all mean "use the global `collection_interval_secs`".
#[derive(Debug, Clone)]
pub struct PatternEntry {
    pub pattern: String,
    /// Per-pattern override in seconds.  `None` or `0` → use global default.
    pub interval_secs: Option<u64>,
}

impl PatternEntry {
    pub fn new(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            interval_secs: None,
        }
    }

    /// Return the effective collection interval for this pattern.
    pub fn effective_interval(&self, default: u64) -> u64 {
        match self.interval_secs {
            Some(n) if n > 0 => n,
            _ => default,
        }
    }
}

/// Custom serde for `Vec<PatternEntry>` and plain-`Value` helpers used by
/// `db_config.rs` (which cannot use serde's generic machinery directly).
///
/// **Deserialises** both the legacy array-of-strings format and the new
/// object format:
/// - Legacy: `["inst:*:M", "inst:*:A"]`
/// - New:    `{"inst:*:M": null, "inst:4:M": 60}`
///
/// **Serialises** always as the object format.
pub mod pattern_serde {
    use super::PatternEntry;
    use serde::{
        Deserializer, Serializer,
        de::{MapAccess, SeqAccess, Visitor},
        ser::SerializeMap,
    };

    // ── serde `with` hooks (used by ServiceConfig) ────────────────────────────

    pub fn serialize<S>(patterns: &[PatternEntry], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(patterns.len()))?;
        for p in patterns {
            map.serialize_entry(&p.pattern, &p.interval_secs)?;
        }
        map.end()
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<PatternEntry>, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct PatternVisitor;

        impl<'de> Visitor<'de> for PatternVisitor {
            type Value = Vec<PatternEntry>;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str(
                    "an array of strings or an object mapping pattern to interval (seconds)",
                )
            }

            // Legacy: ["inst:*:M", "inst:*:A"]
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut result = Vec::new();
                while let Some(pattern) = seq.next_element::<String>()? {
                    result.push(PatternEntry {
                        pattern,
                        interval_secs: None,
                    });
                }
                Ok(result)
            }

            // New: {"inst:*:M": null, "inst:4:M": 60}
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut result = Vec::new();
                while let Some((pattern, raw)) =
                    map.next_entry::<String, Option<serde_json::Value>>()?
                {
                    let interval_secs = raw.and_then(value_to_interval);
                    result.push(PatternEntry {
                        pattern,
                        interval_secs,
                    });
                }
                Ok(result)
            }
        }

        deserializer.deserialize_any(PatternVisitor)
    }

    // ── Plain-Value helpers for db_config.rs ─────────────────────────────────

    /// Parse a JSON string (either old array or new object format) into
    /// `Vec<PatternEntry>`.  Returns defaults on parse failure.
    pub fn from_json_str(s: &str) -> Vec<PatternEntry> {
        let v: serde_json::Value = match serde_json::from_str(s) {
            Ok(v) => v,
            Err(_) => return vec![PatternEntry::new("inst:*:M"), PatternEntry::new("inst:*:A")],
        };
        from_value(v)
    }

    /// Serialize `Vec<PatternEntry>` to a JSON string (object format).
    pub fn to_json_str(patterns: &[PatternEntry]) -> serde_json::Result<String> {
        let map: serde_json::Map<String, serde_json::Value> = patterns
            .iter()
            .map(|p| {
                let v = match p.interval_secs {
                    Some(n) => serde_json::Value::Number(n.into()),
                    None => serde_json::Value::Null,
                };
                (p.pattern.clone(), v)
            })
            .collect();
        serde_json::to_string(&serde_json::Value::Object(map))
    }

    fn from_value(v: serde_json::Value) -> Vec<PatternEntry> {
        match v {
            serde_json::Value::Array(arr) => arr
                .into_iter()
                .filter_map(|item| item.as_str().map(PatternEntry::new))
                .collect(),
            serde_json::Value::Object(map) => map
                .into_iter()
                .map(|(pattern, val)| {
                    let interval_secs = value_to_interval(val);
                    PatternEntry {
                        pattern,
                        interval_secs,
                    }
                })
                .collect(),
            _ => vec![PatternEntry::new("inst:*:M"), PatternEntry::new("inst:*:A")],
        }
    }

    /// Convert a JSON value to a positive `u64` interval, or `None`.
    fn value_to_interval(v: serde_json::Value) -> Option<u64> {
        match v {
            serde_json::Value::Number(n) => n.as_u64().filter(|&x| x > 0),
            serde_json::Value::String(s) => s.trim().parse::<u64>().ok().filter(|&x| x > 0),
            _ => None,
        }
    }
}

/// 服务运行参数配置（`/hisApi/config`）
///
/// Controls collection frequency, write batch size, query limits, and
/// SHM logical-series selectors. Storage backend connection parameters are
/// managed separately via `/hisApi/storage`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({
    "collection_interval_secs": 30,
    "flush_interval_secs": 60,
    "batch_size": 1000,
    "cleanup_enabled": true,
    "cleanup_older_than_days": 30,
    "default_page_size": 100,
    "max_page_size": 1000,
    "max_time_range_days": 365,
    "subscribe_patterns": {"inst:*:M": null, "inst:4:M": 60},
    "exclude_patterns": []
}))]
pub struct ServiceConfig {
    /// Collection interval in seconds.
    ///
    /// How often the collector samples selected SHM series into the in-memory
    /// buffer. Shorter intervals increase data freshness and local I/O.
    /// Recommended range: 10–300.
    #[schema(example = 30, minimum = 1)]
    pub collection_interval_secs: u64,

    /// Flush interval in seconds.
    ///
    /// How often the in-memory buffer is batch-written to the database.
    /// Should not be shorter than `collection_interval_secs`.
    /// Recommended range: 30–600.
    #[schema(example = 60, minimum = 1)]
    pub flush_interval_secs: u64,

    /// Maximum records per flush batch.
    ///
    /// Records beyond this limit are deferred to the next flush cycle.
    /// Larger values increase single-transaction latency.
    /// Recommended range: 100–5000.
    #[schema(example = 1000, minimum = 1)]
    pub batch_size: usize,

    /// Enable automatic data retention cleanup.
    ///
    /// When enabled, a daily job at 02:00 UTC deletes records older than
    /// `cleanup_older_than_days`.
    #[schema(example = true)]
    pub cleanup_enabled: bool,

    /// Data retention period in days.
    ///
    /// The cleanup job removes all records older than this value.
    /// Only effective when `cleanup_enabled = true`.
    /// Recommended range: 7–3650.
    #[schema(example = 30, minimum = 1)]
    pub cleanup_older_than_days: i32,

    /// Default page size (records per page).
    ///
    /// Used when the caller omits the `page_size` query parameter.
    #[schema(example = 100, minimum = 1)]
    pub default_page_size: i64,

    /// Maximum allowed page size (records per page).
    ///
    /// Client-supplied `page_size` values exceeding this limit are clamped
    /// to prevent oversized single queries.
    #[schema(example = 1000, minimum = 1)]
    pub max_page_size: i64,

    /// Maximum query time span in days.
    ///
    /// A single query's `start_time`-to-`end_time` range may not exceed this
    /// value; requests exceeding it are rejected. Recommended range: 1–3650.
    #[schema(example = 365, minimum = 1)]
    pub max_time_range_days: i64,

    /// Logical series selectors using `*` and `?` glob syntax.
    ///
    /// 接受两种格式（向下兼容旧的数组格式）：
    ///
    /// **旧格式**（数组）：所有 pattern 使用全局 `collection_interval_secs`
    /// ```json
    /// ["inst:*:M", "inst:*:A"]
    /// ```
    ///
    /// **新格式**（对象）：可为每个 pattern 指定独立采集间隔（秒）；
    /// `null`、`0` 或省略均表示使用全局默认值。
    /// ```json
    /// {"inst:*:M": null, "inst:4:M": 60}
    /// ```
    #[serde(with = "pattern_serde")]
    #[schema(value_type = Object, example = json!({"inst:*:M": null, "inst:4:M": 60}))]
    pub subscribe_patterns: Vec<PatternEntry>,

    /// Exclusion patterns (**regex syntax** — distinct from the glob syntax
    /// used in `subscribe_patterns`).
    ///
    /// 命中任意一条正则的逻辑序列将被跳过，不采集。
    #[schema(example = json!([]))]
    pub exclude_patterns: Vec<String>,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            collection_interval_secs: 30,
            flush_interval_secs: 60,
            batch_size: 1000,
            cleanup_enabled: true,
            cleanup_older_than_days: 30,
            default_page_size: 100,
            max_page_size: 1000,
            max_time_range_days: 365,
            subscribe_patterns: vec![PatternEntry::new("inst:*:M"), PatternEntry::new("inst:*:A")],
            exclude_patterns: vec![],
        }
    }
}

impl ServiceConfig {
    pub fn normalize(&mut self) {
        self.collection_interval_secs = self.collection_interval_secs.max(1);
        self.flush_interval_secs = self.flush_interval_secs.max(1);
        self.batch_size = self.batch_size.max(1);
        self.cleanup_older_than_days = self.cleanup_older_than_days.max(1);
        self.default_page_size = self.default_page_size.max(1);
        self.max_page_size = self.max_page_size.max(1);
        self.max_time_range_days = self.max_time_range_days.max(1);
    }
}

#[cfg(test)]
mod config_tests {
    use super::*;

    #[test]
    fn normalize_clamps_zero_runtime_values() {
        let mut cfg = ServiceConfig {
            collection_interval_secs: 0,
            flush_interval_secs: 0,
            batch_size: 0,
            cleanup_older_than_days: 0,
            default_page_size: 0,
            max_page_size: 0,
            max_time_range_days: 0,
            ..ServiceConfig::default()
        };

        cfg.normalize();

        assert_eq!(cfg.collection_interval_secs, 1);
        assert_eq!(cfg.flush_interval_secs, 1);
        assert_eq!(cfg.batch_size, 1);
        assert_eq!(cfg.cleanup_older_than_days, 1);
        assert_eq!(cfg.default_page_size, 1);
        assert_eq!(cfg.max_page_size, 1);
        assert_eq!(cfg.max_time_range_days, 1);
    }

    #[test]
    fn batch_series_json_uses_domain_series_key() {
        let item = BatchSeriesItem {
            series_key: "inst:9:M".to_string(),
            point_id: "101".to_string(),
        };

        let value = serde_json::to_value(item).expect("serialize batch series item");

        assert_eq!(value["series_key"], "inst:9:M");
    }
}

// ── Internal storage connection settings ─────────────────────────────────────

/// Storage backend connection settings.  Persisted in the same `history_config`
/// table but **only** accessible via `/hisApi/storage` – never mixed into the
/// general service config API.
#[derive(Debug, Clone, Default)]
pub struct StorageSettings {
    pub enabled: bool,
    /// `sqlite` by default; `postgres` / `timescaledb` when the optional
    /// `postgres-storage` feature is enabled.
    pub backend: String,
    /// Local SQLite file path or external database DSN.
    pub url: String,
}

// ── Storage configuration request ────────────────────────────────────────────

/// Connectivity test request body (`POST /hisApi/storage/test`).
///
/// The probe **does not write any data or modify any runtime state**.
/// For PostgreSQL / TimescaleDB it connects to the built-in `postgres`
/// maintenance database and executes `SELECT 1`, so **the target business
/// database does not need to exist** for the test to pass.
#[derive(Debug, Deserialize, ToSchema)]
pub struct StorageTestRequest {
    /// Database backend type.
    ///
    /// - `postgres` — standard PostgreSQL
    /// - `timescaledb` — PostgreSQL + TimescaleDB extension (same connection params as postgres)
    /// - `influxdb` — InfluxDB (reserved; not yet implemented)
    #[schema(example = "timescaledb")]
    pub backend: String,

    /// Database host address (IP or hostname).
    #[schema(example = "192.168.20.21")]
    pub host: String,

    /// Database port.
    ///
    /// Default: `5432` for PostgreSQL / TimescaleDB; `8086` for InfluxDB.
    #[schema(example = 5432, minimum = 1, maximum = 65535)]
    pub port: Option<u16>,

    /// Database username (PostgreSQL / TimescaleDB).
    #[cfg_attr(not(feature = "postgres-storage"), allow(dead_code))]
    #[schema(example = "postgres")]
    pub username: String,

    /// Database password (PostgreSQL / TimescaleDB).
    #[cfg_attr(not(feature = "postgres-storage"), allow(dead_code))]
    #[schema(example = "secret")]
    pub password: String,
}

impl StorageTestRequest {
    /// Friendly `host:port` string for log / response messages.
    pub fn addr(&self) -> String {
        let default_port = match self.backend.as_str() {
            "influxdb" => 8086,
            _ => 5432,
        };
        format!("{}:{}", self.host, self.port.unwrap_or(default_port))
    }

    /// Build a PostgreSQL DSN pointing at the always-present `postgres`
    /// maintenance database (used for postgres / timescaledb probing).
    #[cfg(feature = "postgres-storage")]
    pub fn pg_probe_dsn(&self) -> String {
        build_dsn(
            &self.host,
            self.port,
            "postgres",
            &self.username,
            &self.password,
        )
    }
}

/// Request body for `PUT /hisApi/storage`.
///
/// This endpoint **only persists parameters**; it does not establish a
/// database connection immediately. After saving, apply and connect via
/// `POST /hisApi/storage/reconnect`, or verify connectivity first via
/// `POST /hisApi/storage/test`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[schema(example = json!({
    "enabled": true,
    "backend": "timescaledb",
    "host": "192.168.20.21",
    "port": 5432,
    "database": "history",
    "username": "postgres",
    "password": "postgres"
}))]
pub struct StorageConfigRequest {
    /// Enable historical storage.
    ///
    /// `true`: collection and writes begin after service startup or reconnect.
    /// `false`: writes are stopped (existing data is unaffected).
    #[schema(example = true)]
    pub enabled: bool,

    /// Database backend type.
    ///
    /// - `postgres`: standard PostgreSQL, suitable for general historical storage.
    /// - `timescaledb`: PostgreSQL + TimescaleDB extension, optimised for
    ///   time-series data; recommended for production.
    #[schema(example = "timescaledb")]
    pub backend: String,

    /// Database host address.
    ///
    /// IP address or hostname, e.g. `192.168.20.21` or `db.example.com`.
    #[schema(example = "192.168.20.21")]
    pub host: String,

    /// Database port (default `5432`).
    #[schema(example = 5432, minimum = 1, maximum = 65535)]
    pub port: Option<u16>,

    /// Database name.
    ///
    /// Historical data is written to this database. The database is created
    /// automatically on first connect; tables are initialised on first use.
    #[schema(example = "history")]
    pub database: String,

    /// Database username.
    #[schema(example = "postgres")]
    pub username: String,

    /// Database password.
    ///
    /// Special characters (`@`, `#`, `:`, etc.) do not need to be
    /// percent-encoded — the backend handles URL-encoding automatically.
    #[schema(example = "postgres")]
    pub password: String,
}

impl StorageConfigRequest {
    pub fn to_dsn(&self) -> String {
        build_dsn(
            &self.host,
            self.port,
            &self.database,
            &self.username,
            &self.password,
        )
    }
}

// ── Shared DSN builder ────────────────────────────────────────────────────────

pub fn build_dsn(
    host: &str,
    port: Option<u16>,
    database: &str,
    username: &str,
    password: &str,
) -> String {
    let port = port.unwrap_or(5432);
    let user = urlencoding::encode(username);
    let pass = urlencoding::encode(password);
    format!(
        "postgres://{}:{}@{}:{}/{}",
        user, pass, host, port, database
    )
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Convert a `DateTime<Utc>` to the ISO-8601 string format used in responses.
pub fn fmt_ts(dt: &DateTime<Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// Derive the source prefix from a logical series key (first `:` segment).
pub fn source_from_key(key: &str) -> String {
    key.split(':').next().unwrap_or(key).to_string()
}

/// Parse various time string formats into `DateTime<Utc>`.
pub fn parse_time(s: &str) -> anyhow::Result<DateTime<Utc>> {
    use chrono::NaiveDateTime;

    // Try RFC 3339 / ISO 8601 first
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }

    // `2025-08-21 23:59:59`
    if let Ok(dt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
        return Ok(dt.and_utc());
    }

    // `2025-08-21T23:59:59`
    if let Ok(dt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
        return Ok(dt.and_utc());
    }

    // Date only: `2025-08-21`
    if let Ok(d) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
        && let Some(dt) = d.and_hms_opt(0, 0, 0)
    {
        return Ok(dt.and_utc());
    }

    // Unix timestamp (integer)
    if let Ok(ts) = s.parse::<i64>()
        && let Some(dt) = DateTime::from_timestamp(ts, 0)
    {
        return Ok(dt);
    }

    anyhow::bail!("Unsupported time format: {}", s)
}
