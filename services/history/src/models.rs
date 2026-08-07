use chrono::{DateTime, Utc};

/// Upper bound for any day-denominated configuration value (100 years).
///
/// `chrono` panics when a `DateTime` +/- `Duration` leaves its representable
/// range, and the workspace release profile sets `panic = "abort"`, so an
/// unclamped value turns one request into a process kill. The persisted config
/// would then reproduce the crash on every restart.
pub const MAX_RANGE_DAYS: i64 = 36_500;

/// Upper bound for page sizes so `(page - 1) * page_size` cannot overflow `i64`.
pub const MAX_PAGE_SIZE_LIMIT: i64 = 100_000;

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
#[derive(Debug, Clone)]
pub struct HistoryRecord {
    pub timestamp: String,
    pub series_key: String,
    pub point_id: String,
    pub value: Option<f64>,
    /// Source prefix, derived from the first segment of the logical key.
    pub source: String,
}

/// One data point in a batch query response.
#[derive(Debug)]
pub struct SeriesPoint {
    pub time: String,
    pub value: Option<f64>,
}

/// Query result for one (`series_key`, `point_id`) series.
#[derive(Debug)]
pub struct SeriesResult {
    pub series_key: String,
    pub point_id: String,
    pub count: usize,
    pub data: Vec<SeriesPoint>,
}

/// Validated range query passed to storage adapters.
#[derive(Debug)]
pub struct HistoryRangeQuery {
    pub series_key: String,
    pub point_id: String,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub page: i64,
    pub page_size: i64,
}

/// Aggregate statistics returned by storage adapters.
#[derive(Debug)]
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

/// Service runtime configuration (`/hisApi/config`).
///
/// Controls collection frequency, write batch size, query limits, and
/// SHM logical-series selectors. Storage backend connection parameters are
/// managed separately via `/hisApi/storage`.
#[derive(Debug, Clone)]
pub struct ServiceConfig {
    /// Collection interval in seconds.
    ///
    /// How often the collector samples selected SHM series into the in-memory
    /// buffer. Shorter intervals increase data freshness and local I/O.
    /// Recommended range: 10–300.
    pub collection_interval_secs: u64,

    /// Flush interval in seconds.
    ///
    /// How often the in-memory buffer is batch-written to the database.
    /// Should not be shorter than `collection_interval_secs`.
    /// Recommended range: 30–600.
    pub flush_interval_secs: u64,

    /// Maximum records per flush batch.
    ///
    /// Records beyond this limit are deferred to the next flush cycle.
    /// Larger values increase single-transaction latency.
    /// Recommended range: 100–5000.
    pub batch_size: usize,

    /// Enable automatic data retention cleanup.
    ///
    /// When enabled, a daily job at 02:00 UTC deletes records older than
    /// `cleanup_older_than_days`.
    pub cleanup_enabled: bool,

    /// Data retention period in days.
    ///
    /// The cleanup job removes all records older than this value.
    /// Only effective when `cleanup_enabled = true`.
    /// Recommended range: 7–3650.
    pub cleanup_older_than_days: i32,

    /// Default page size (records per page).
    ///
    /// Used when the caller omits the `page_size` query parameter.
    pub default_page_size: i64,

    /// Maximum allowed page size (records per page).
    ///
    /// Client-supplied `page_size` values exceeding this limit are clamped
    /// to prevent oversized single queries.
    pub max_page_size: i64,

    /// Maximum query time span in days.
    ///
    /// A single query's `start_time`-to-`end_time` range may not exceed this
    /// value; requests exceeding it are rejected. Recommended range: 1–3650.
    pub max_time_range_days: i64,

    /// Logical series selectors using `*` and `?` glob syntax.
    ///
    /// Accepts two formats (backward compatible with the legacy array format):
    ///
    /// **Legacy format** (array): every pattern uses the global
    /// `collection_interval_secs`.
    /// ```json
    /// ["inst:*:M", "inst:*:A"]
    /// ```
    ///
    /// **New format** (object): each pattern may specify its own collection
    /// interval in seconds; `null`, `0`, or omission all mean "use the
    /// global default".
    /// ```json
    /// {"inst:*:M": null, "inst:4:M": 60}
    /// ```
    pub subscribe_patterns: Vec<PatternEntry>,

    /// Exclusion patterns (**regex syntax** — distinct from the glob syntax
    /// used in `subscribe_patterns`).
    ///
    /// A logical series matching any of these regexes is skipped and not
    /// collected.
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
        self.cleanup_older_than_days = self.cleanup_older_than_days.clamp(1, MAX_RANGE_DAYS as i32);
        self.default_page_size = self.default_page_size.clamp(1, MAX_PAGE_SIZE_LIMIT);
        self.max_page_size = self.max_page_size.clamp(1, MAX_PAGE_SIZE_LIMIT);
        self.max_time_range_days = self.max_time_range_days.clamp(1, MAX_RANGE_DAYS);
    }
}

#[cfg(test)]
mod config_tests {
    use chrono::Duration;

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
    fn normalize_clamps_day_ranges_that_would_overflow_chrono() {
        let mut cfg = ServiceConfig {
            cleanup_older_than_days: i32::MAX,
            max_time_range_days: i64::MAX,
            ..ServiceConfig::default()
        };

        cfg.normalize();

        assert_eq!(cfg.max_time_range_days, MAX_RANGE_DAYS);
        assert_eq!(cfg.cleanup_older_than_days, MAX_RANGE_DAYS as i32);
    }

    #[test]
    fn clamped_day_ranges_survive_the_arithmetic_the_query_path_performs() {
        let mut cfg = ServiceConfig {
            cleanup_older_than_days: i32::MAX,
            max_time_range_days: i64::MAX,
            ..ServiceConfig::default()
        };
        cfg.normalize();

        // These are exactly the operations dto.rs and the cleanup backends run;
        // before clamping they panic and, under `panic = "abort"`, kill the process.
        let now = Utc::now();
        assert!(
            now.checked_sub_signed(Duration::days(cfg.max_time_range_days))
                .is_some()
        );
        assert!(
            now.checked_sub_signed(Duration::days(i64::from(cfg.cleanup_older_than_days)))
                .is_some()
        );
    }

    #[test]
    fn normalize_clamps_page_sizes_so_offset_arithmetic_cannot_overflow() {
        let mut cfg = ServiceConfig {
            default_page_size: i64::MAX,
            max_page_size: i64::MAX,
            ..ServiceConfig::default()
        };

        cfg.normalize();

        assert_eq!(cfg.max_page_size, MAX_PAGE_SIZE_LIMIT);
        assert_eq!(cfg.default_page_size, MAX_PAGE_SIZE_LIMIT);
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

/// Distinct data-type codes carried by a set of logical series keys.
///
/// The type code is the *last* `:` segment — the first one is the source, which
/// [`source_from_key`] already reports separately.
pub fn data_types_from_keys(keys: &[String]) -> Vec<String> {
    keys.iter()
        .filter_map(|key| key.rsplit(':').next().map(String::from))
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect()
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

#[cfg(test)]
mod key_derivation_tests {
    use super::{data_types_from_keys, source_from_key};

    #[test]
    fn the_source_is_the_first_segment_and_the_data_type_is_the_last() {
        // Both backends must agree: `GET /hisApi/data/range` returned the source
        // list on PostgreSQL and the type list on SQLite for the same field.
        assert_eq!(source_from_key("inst:1:M"), "inst");

        let keys = vec![
            "inst:1:M".to_string(),
            "inst:2:A".to_string(),
            "io:3:M".to_string(),
        ];
        let mut types = data_types_from_keys(&keys);
        types.sort();

        assert_eq!(types, vec!["A".to_string(), "M".to_string()]);
    }

    #[test]
    fn a_key_without_separators_is_its_own_data_type() {
        assert_eq!(
            data_types_from_keys(&["plain".to_string()]),
            vec!["plain".to_string()]
        );
    }
}
