//! Database operations for alert_rule, alert and alert_event tables.
//!
//! Uses runtime SQLx queries (no compile-time macros) as required by project conventions.

use anyhow::{Context, Result};
use chrono::{TimeZone, Utc};
use sqlx::SqlitePool;
use tracing::info;

use crate::models::{
    Alert, AlertEvent, AlertQueryParams, AlertRule, EventQueryParams, PagedData, RuleQueryParams,
    resolve_pagination,
};

// ============================================================================
// Schema creation
// ============================================================================

pub async fn create_tables(pool: &SqlitePool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS alert_rule (
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            service_type TEXT    NOT NULL,
            channel_id   INTEGER NOT NULL,
            data_type    TEXT    NOT NULL,
            point_id     INTEGER NOT NULL,
            rule_name    TEXT    NOT NULL,
            warning_level INTEGER NOT NULL DEFAULT 2,
            operator     TEXT    NOT NULL,
            value        REAL    NOT NULL,
            enabled      INTEGER NOT NULL DEFAULT 1,
            description  TEXT,
            created_at   INTEGER NOT NULL,
            updated_at   INTEGER NOT NULL
        )
        "#,
    )
    .execute(pool)
    .await
    .context("create alert_rule table")?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS alert (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            rule_id         INTEGER,
            rule_snapshot   TEXT,
            service_type    TEXT,
            channel_id      INTEGER,
            data_type       TEXT,
            point_id        INTEGER,
            rule_name       TEXT,
            warning_level   INTEGER,
            operator        TEXT,
            threshold_value REAL,
            current_value   REAL,
            status          TEXT    NOT NULL DEFAULT 'active',
            triggered_at    INTEGER NOT NULL,
            FOREIGN KEY (rule_id) REFERENCES alert_rule(id)
        )
        "#,
    )
    .execute(pool)
    .await
    .context("create alert table")?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS alert_event (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            rule_id         INTEGER,
            rule_snapshot   TEXT,
            service_type    TEXT,
            channel_id      INTEGER,
            data_type       TEXT,
            point_id        INTEGER,
            rule_name       TEXT,
            warning_level   INTEGER,
            operator        TEXT,
            threshold_value REAL,
            trigger_value   REAL,
            recovery_value  REAL,
            event_type      TEXT    NOT NULL,
            triggered_at    INTEGER,
            recovered_at    INTEGER,
            duration        INTEGER
        )
        "#,
    )
    .execute(pool)
    .await
    .context("create alert_event table")?;

    migrate_alert_event_to_retained_history(pool).await?;

    info!("Alert tables ready");
    Ok(())
}

/// Removes the legacy parent foreign key from historical alarm events.
///
/// Alarm-rule deletion intentionally retains `alert_event` rows. With
/// production `PRAGMA foreign_keys=ON`, the original parent constraint made
/// those two requirements mutually exclusive and rejected every deletion once
/// a rule had history. Rebuilding is safe at startup before the HTTP server or
/// monitor tasks begin.
async fn migrate_alert_event_to_retained_history(pool: &SqlitePool) -> Result<()> {
    let foreign_key_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM pragma_foreign_key_list('alert_event')")
            .fetch_one(pool)
            .await
            .context("inspect alert_event foreign keys")?;
    if foreign_key_count == 0 {
        return Ok(());
    }

    let mut transaction = pool
        .begin()
        .await
        .context("begin alert_event retention migration")?;
    sqlx::query(
        r#"
        CREATE TABLE alert_event_retained_history (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            rule_id         INTEGER,
            rule_snapshot   TEXT,
            service_type    TEXT,
            channel_id      INTEGER,
            data_type       TEXT,
            point_id        INTEGER,
            rule_name       TEXT,
            warning_level   INTEGER,
            operator        TEXT,
            threshold_value REAL,
            trigger_value   REAL,
            recovery_value  REAL,
            event_type      TEXT    NOT NULL,
            triggered_at    INTEGER,
            recovered_at    INTEGER,
            duration        INTEGER
        )
        "#,
    )
    .execute(&mut *transaction)
    .await
    .context("create retained alert_event table")?;
    sqlx::query(
        r#"
        INSERT INTO alert_event_retained_history
            (id, rule_id, rule_snapshot, service_type, channel_id, data_type,
             point_id, rule_name, warning_level, operator, threshold_value,
             trigger_value, recovery_value, event_type, triggered_at,
             recovered_at, duration)
        SELECT id, rule_id, rule_snapshot, service_type, channel_id, data_type,
               point_id, rule_name, warning_level, operator, threshold_value,
               trigger_value, recovery_value, event_type, triggered_at,
               recovered_at, duration
        FROM alert_event
        "#,
    )
    .execute(&mut *transaction)
    .await
    .context("copy retained alert_event rows")?;
    sqlx::query("DROP TABLE alert_event")
        .execute(&mut *transaction)
        .await
        .context("drop constrained alert_event table")?;
    sqlx::query("ALTER TABLE alert_event_retained_history RENAME TO alert_event")
        .execute(&mut *transaction)
        .await
        .context("activate retained alert_event table")?;
    transaction
        .commit()
        .await
        .context("commit alert_event retention migration")?;
    info!("Migrated alert_event to retained history without parent foreign key");
    Ok(())
}

// ============================================================================
// AlertRule CRUD
// ============================================================================

pub async fn get_rule_by_id(pool: &SqlitePool, id: i64) -> Result<Option<AlertRule>> {
    let row = sqlx::query_as::<_, AlertRule>("SELECT * FROM alert_rule WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
        .context("get rule by id")?;
    Ok(row)
}

pub async fn list_rules(
    pool: &SqlitePool,
    params: &RuleQueryParams,
) -> Result<PagedData<AlertRule>> {
    let mut cond_strings: Vec<String> = Vec::new();

    // keyword: fuzzy match across rule_name, description, channel_id, point_id
    if params.keyword.is_some() {
        cond_strings.push(
            "(rule_name LIKE ? OR COALESCE(description,'') LIKE ? \
             OR CAST(channel_id AS TEXT) LIKE ? OR CAST(point_id AS TEXT) LIKE ?)"
                .to_string(),
        );
    }
    if params.service_type.is_some() {
        cond_strings.push("service_type = ?".to_string());
    }
    if params.channel_id.is_some() {
        cond_strings.push("channel_id = ?".to_string());
    }
    if params.data_type.is_some() {
        cond_strings.push("data_type = ?".to_string());
    }
    if params.enabled.is_some() {
        cond_strings.push("enabled = ?".to_string());
    }
    if params.warning_level.is_some() {
        cond_strings.push("warning_level = ?".to_string());
    }

    let where_clause = if cond_strings.is_empty() {
        "1=1".to_string()
    } else {
        cond_strings.join(" AND ")
    };

    let count_sql = format!("SELECT COUNT(*) FROM alert_rule WHERE {}", where_clause);
    let data_sql = format!(
        "SELECT * FROM alert_rule WHERE {} ORDER BY id ASC LIMIT ? OFFSET ?",
        where_clause
    );

    // Bind parameters helper closure
    macro_rules! bind_params {
        ($q:expr_2021) => {{
            let mut q = $q;
            if let Some(ref kw) = params.keyword {
                let pat = format!("%{}%", kw);
                q = q
                    .bind(pat.clone())
                    .bind(pat.clone())
                    .bind(pat.clone())
                    .bind(pat);
            }
            if let Some(ref v) = params.service_type {
                q = q.bind(v.clone());
            }
            if let Some(v) = params.channel_id {
                q = q.bind(v);
            }
            if let Some(ref v) = params.data_type {
                q = q.bind(v.clone());
            }
            if let Some(v) = params.enabled {
                q = q.bind(if v { 1i64 } else { 0i64 });
            }
            if let Some(v) = params.warning_level {
                q = q.bind(v);
            }
            q
        }};
    }

    let (eff_limit, offset, page, page_size) =
        resolve_pagination(params.page, params.page_size, params.skip, params.limit);

    let total: i64 = bind_params!(sqlx::query_scalar::<_, i64>(&count_sql))
        .fetch_one(pool)
        .await
        .context("count rules")?;

    let list: Vec<AlertRule> = bind_params!(sqlx::query_as::<_, AlertRule>(&data_sql))
        .bind(eff_limit)
        .bind(offset)
        .fetch_all(pool)
        .await
        .context("list rules")?;

    Ok(PagedData {
        total,
        list,
        page,
        page_size,
    })
}

pub async fn get_rules_by_channel(pool: &SqlitePool, channel_id: i64) -> Result<Vec<AlertRule>> {
    sqlx::query_as::<_, AlertRule>("SELECT * FROM alert_rule WHERE channel_id = ? ORDER BY id DESC")
        .bind(channel_id)
        .fetch_all(pool)
        .await
        .context("get rules by channel")
}

/// Check whether a rule with the given name already exists (case-insensitive).
#[cfg(test)]
pub async fn find_rule_by_name(pool: &SqlitePool, rule_name: &str) -> Result<Option<AlertRule>> {
    sqlx::query_as::<_, AlertRule>(
        "SELECT * FROM alert_rule WHERE LOWER(rule_name) = LOWER(?) LIMIT 1",
    )
    .bind(rule_name)
    .fetch_optional(pool)
    .await
    .context("find rule by name")
}

pub async fn get_all_enabled_rules(pool: &SqlitePool) -> Result<Vec<AlertRule>> {
    sqlx::query_as::<_, AlertRule>("SELECT * FROM alert_rule WHERE enabled = 1 ORDER BY id ASC")
        .fetch_all(pool)
        .await
        .context("get enabled rules")
}

// ============================================================================
// Alert CRUD
// ============================================================================

pub async fn get_alert_by_id(pool: &SqlitePool, id: i64) -> Result<Option<Alert>> {
    sqlx::query_as::<_, Alert>("SELECT * FROM alert WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
        .context("get alert by id")
}

pub async fn get_alert_by_rule_id(pool: &SqlitePool, rule_id: i64) -> Result<Option<Alert>> {
    sqlx::query_as::<_, Alert>("SELECT * FROM alert WHERE rule_id = ? LIMIT 1")
        .bind(rule_id)
        .fetch_optional(pool)
        .await
        .context("get alert by rule_id")
}

pub async fn get_all_active_alerts(pool: &SqlitePool) -> Result<Vec<Alert>> {
    sqlx::query_as::<_, Alert>(
        "SELECT * FROM alert WHERE status = 'active' ORDER BY warning_level DESC, triggered_at DESC",
    )
    .fetch_all(pool)
    .await
    .context("get all active alerts")
}

pub async fn list_alerts(pool: &SqlitePool, params: &AlertQueryParams) -> Result<PagedData<Alert>> {
    let mut cond_strings: Vec<String> = Vec::new();
    cond_strings.push("status = 'active'".to_string());

    if params.service_type.is_some() {
        cond_strings.push("service_type = ?".to_string());
    }
    if params.channel_id.is_some() {
        cond_strings.push("channel_id = ?".to_string());
    }
    if params.warning_level.is_some() {
        cond_strings.push("warning_level = ?".to_string());
    }
    if params.keyword.is_some() {
        cond_strings.push(
            "(rule_name LIKE ? OR CAST(channel_id AS TEXT) LIKE ? OR CAST(point_id AS TEXT) LIKE ?)"
                .to_string(),
        );
    }

    let where_clause = cond_strings.join(" AND ");
    let count_sql = format!("SELECT COUNT(*) FROM alert WHERE {}", where_clause);
    let data_sql = format!(
        "SELECT * FROM alert WHERE {} ORDER BY warning_level DESC, triggered_at DESC LIMIT ? OFFSET ?",
        where_clause
    );

    macro_rules! bind_alert_params {
        ($q:expr_2021) => {{
            let mut q = $q;
            if let Some(ref v) = params.service_type {
                q = q.bind(v.clone());
            }
            if let Some(v) = params.channel_id {
                q = q.bind(v);
            }
            if let Some(v) = params.warning_level {
                q = q.bind(v);
            }
            if let Some(ref k) = params.keyword {
                let pat = format!("%{}%", k);
                q = q.bind(pat.clone()).bind(pat.clone()).bind(pat);
            }
            q
        }};
    }

    let total: i64 = bind_alert_params!(sqlx::query_scalar::<_, i64>(&count_sql))
        .fetch_one(pool)
        .await
        .context("count alerts")?;

    let (eff_limit, offset, page, page_size) =
        resolve_pagination(params.page, params.page_size, params.skip, params.limit);

    let list: Vec<Alert> = bind_alert_params!(sqlx::query_as::<_, Alert>(&data_sql))
        .bind(eff_limit)
        .bind(offset)
        .fetch_all(pool)
        .await
        .context("list alerts")?;

    Ok(PagedData {
        total,
        list,
        page,
        page_size,
    })
}

pub async fn insert_alert(pool: &SqlitePool, rule: &AlertRule, current_value: f64) -> Result<i64> {
    let now = Utc::now().timestamp();
    let snapshot = rule.snapshot();

    let id = sqlx::query_scalar::<_, i64>(
        r#"
        INSERT INTO alert
            (rule_id, rule_snapshot, service_type, channel_id, data_type, point_id,
             rule_name, warning_level, operator, threshold_value, current_value,
             status, triggered_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'active', ?)
        RETURNING id
        "#,
    )
    .bind(rule.id)
    .bind(&snapshot)
    .bind(&rule.service_type)
    .bind(rule.channel_id)
    .bind(&rule.data_type)
    .bind(rule.point_id)
    .bind(&rule.rule_name)
    .bind(rule.warning_level)
    .bind(&rule.operator)
    .bind(rule.value)
    .bind(current_value)
    .bind(now)
    .fetch_one(pool)
    .await
    .context("insert alert")?;

    Ok(id)
}

pub async fn update_alert_value(
    pool: &SqlitePool,
    alert_id: i64,
    current_value: f64,
) -> Result<()> {
    sqlx::query("UPDATE alert SET current_value = ? WHERE id = ?")
        .bind(current_value)
        .bind(alert_id)
        .execute(pool)
        .await
        .context("update alert value")?;
    Ok(())
}

/// Resolves an alert: inserts an alert_event record then deletes the alert row.
pub async fn resolve_alert(pool: &SqlitePool, alert: &Alert, recovery_value: f64) -> Result<i64> {
    let now = Utc::now().timestamp();
    let duration = now - alert.triggered_at;

    let mut tx = pool.begin().await.context("begin transaction")?;

    let event_id = sqlx::query_scalar::<_, i64>(
        r#"
        INSERT INTO alert_event
            (rule_id, rule_snapshot, service_type, channel_id, data_type, point_id,
             rule_name, warning_level, operator, threshold_value,
             trigger_value, recovery_value, event_type,
             triggered_at, recovered_at, duration)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'recovery', ?, ?, ?)
        RETURNING id
        "#,
    )
    .bind(alert.rule_id)
    .bind(&alert.rule_snapshot)
    .bind(&alert.service_type)
    .bind(alert.channel_id)
    .bind(&alert.data_type)
    .bind(alert.point_id)
    .bind(&alert.rule_name)
    .bind(alert.warning_level)
    .bind(&alert.operator)
    .bind(alert.threshold_value)
    .bind(alert.current_value)
    .bind(recovery_value)
    .bind(alert.triggered_at)
    .bind(now)
    .bind(duration)
    .fetch_one(&mut *tx)
    .await
    .context("insert alert_event")?;

    sqlx::query("DELETE FROM alert WHERE id = ?")
        .bind(alert.id)
        .execute(&mut *tx)
        .await
        .context("delete alert")?;

    tx.commit().await.context("commit resolve_alert")?;
    Ok(event_id)
}

// ============================================================================
// AlertEvent queries
// ============================================================================

pub async fn list_events(
    pool: &SqlitePool,
    params: &EventQueryParams,
) -> Result<PagedData<AlertEvent>> {
    let mut cond_strings: Vec<String> = Vec::new();

    // keyword: fuzzy match across rule_name, channel_id, point_id
    if params.keyword.is_some() {
        cond_strings.push(
            "(rule_name LIKE ? OR CAST(channel_id AS TEXT) LIKE ? OR CAST(point_id AS TEXT) LIKE ?)"
                .to_string(),
        );
    }
    if params.rule_id.is_some() {
        cond_strings.push("rule_id = ?".to_string());
    }
    if params.event_type.is_some() {
        cond_strings.push("event_type = ?".to_string());
    }
    if params.service_type.is_some() {
        cond_strings.push("service_type = ?".to_string());
    }
    if params.warning_level.is_some() {
        cond_strings.push("warning_level = ?".to_string());
    }
    if params.start_time.is_some() {
        cond_strings.push("triggered_at >= ?".to_string());
    }
    if params.end_time.is_some() {
        cond_strings.push("triggered_at <= ?".to_string());
    }

    let where_clause = if cond_strings.is_empty() {
        "1=1".to_string()
    } else {
        cond_strings.join(" AND ")
    };

    let count_sql = format!("SELECT COUNT(*) FROM alert_event WHERE {}", where_clause);
    let data_sql = format!(
        "SELECT * FROM alert_event WHERE {} ORDER BY triggered_at DESC LIMIT ? OFFSET ?",
        where_clause
    );

    macro_rules! bind_event_params {
        ($q:expr_2021) => {{
            let mut q = $q;
            if let Some(ref kw) = params.keyword {
                let pat = format!("%{}%", kw);
                q = q.bind(pat.clone()).bind(pat.clone()).bind(pat);
            }
            if let Some(v) = params.rule_id {
                q = q.bind(v);
            }
            if let Some(ref v) = params.event_type {
                q = q.bind(v.clone());
            }
            if let Some(ref v) = params.service_type {
                q = q.bind(v.clone());
            }
            if let Some(v) = params.warning_level {
                q = q.bind(v);
            }
            if let Some(v) = params.start_time {
                q = q.bind(v);
            }
            if let Some(v) = params.end_time {
                q = q.bind(v);
            }
            q
        }};
    }

    let total: i64 = bind_event_params!(sqlx::query_scalar::<_, i64>(&count_sql))
        .fetch_one(pool)
        .await
        .context("count events")?;

    let (eff_limit, offset, page, page_size) =
        resolve_pagination(params.page, params.page_size, params.skip, params.limit);

    let list: Vec<AlertEvent> = bind_event_params!(sqlx::query_as::<_, AlertEvent>(&data_sql))
        .bind(eff_limit)
        .bind(offset)
        .fetch_all(pool)
        .await
        .context("list events")?;

    Ok(PagedData {
        total,
        list,
        page,
        page_size,
    })
}

pub async fn get_all_events_for_export(
    pool: &SqlitePool,
    params: &EventQueryParams,
) -> Result<Vec<AlertEvent>> {
    let mut cond_strings: Vec<String> = Vec::new();
    if params.keyword.is_some() {
        cond_strings.push(
            "(rule_name LIKE ? OR CAST(channel_id AS TEXT) LIKE ? OR CAST(point_id AS TEXT) LIKE ?)"
                .to_string(),
        );
    }
    if params.rule_id.is_some() {
        cond_strings.push("rule_id = ?".to_string());
    }
    if params.event_type.is_some() {
        cond_strings.push("event_type = ?".to_string());
    }
    if params.service_type.is_some() {
        cond_strings.push("service_type = ?".to_string());
    }
    if params.warning_level.is_some() {
        cond_strings.push("warning_level = ?".to_string());
    }
    if params.start_time.is_some() {
        cond_strings.push("triggered_at >= ?".to_string());
    }
    if params.end_time.is_some() {
        cond_strings.push("triggered_at <= ?".to_string());
    }

    let where_clause = if cond_strings.is_empty() {
        "1=1".to_string()
    } else {
        cond_strings.join(" AND ")
    };

    let sql = format!(
        "SELECT * FROM alert_event WHERE {} ORDER BY triggered_at DESC",
        where_clause
    );

    let mut q = sqlx::query_as::<_, AlertEvent>(&sql);
    if let Some(ref kw) = params.keyword {
        let pat = format!("%{}%", kw);
        q = q.bind(pat.clone()).bind(pat.clone()).bind(pat);
    }
    if let Some(v) = params.rule_id {
        q = q.bind(v);
    }
    if let Some(ref v) = params.event_type {
        q = q.bind(v.clone());
    }
    if let Some(ref v) = params.service_type {
        q = q.bind(v.clone());
    }
    if let Some(v) = params.warning_level {
        q = q.bind(v);
    }
    if let Some(v) = params.start_time {
        q = q.bind(v);
    }
    if let Some(v) = params.end_time {
        q = q.bind(v);
    }

    q.fetch_all(pool).await.context("export events")
}

#[cfg(test)]
mod retention_migration_tests {
    use super::*;

    #[tokio::test]
    async fn legacy_event_foreign_key_is_removed_without_losing_history() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory database");
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&pool)
            .await
            .expect("enable foreign keys");
        sqlx::query(
            "CREATE TABLE alert_rule (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                service_type TEXT NOT NULL, channel_id INTEGER NOT NULL,
                data_type TEXT NOT NULL, point_id INTEGER NOT NULL,
                rule_name TEXT NOT NULL, warning_level INTEGER NOT NULL,
                operator TEXT NOT NULL, value REAL NOT NULL,
                enabled INTEGER NOT NULL, description TEXT,
                created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("legacy parent table");
        sqlx::query(
            "CREATE TABLE alert_event (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                rule_id INTEGER, rule_snapshot TEXT, service_type TEXT,
                channel_id INTEGER, data_type TEXT, point_id INTEGER,
                rule_name TEXT, warning_level INTEGER, operator TEXT,
                threshold_value REAL, trigger_value REAL, recovery_value REAL,
                event_type TEXT NOT NULL, triggered_at INTEGER,
                recovered_at INTEGER, duration INTEGER,
                FOREIGN KEY (rule_id) REFERENCES alert_rule(id)
            )",
        )
        .execute(&pool)
        .await
        .expect("legacy event table");
        sqlx::query(
            "INSERT INTO alert_rule
             (id, service_type, channel_id, data_type, point_id, rule_name,
              warning_level, operator, value, enabled, created_at, updated_at)
             VALUES (7, 'io', 1, 'T', 2, 'temperature', 2, '>', 80, 0, 1, 1)",
        )
        .execute(&pool)
        .await
        .expect("legacy rule");
        sqlx::query("INSERT INTO alert_event (rule_id, event_type) VALUES (7, 'recovery')")
            .execute(&pool)
            .await
            .expect("legacy history");

        create_tables(&pool).await.expect("migrate schema");
        sqlx::query("DELETE FROM alert_rule WHERE id = 7")
            .execute(&pool)
            .await
            .expect("delete parent after migration");

        let retained: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM alert_event")
            .fetch_one(&pool)
            .await
            .expect("retained history");
        let foreign_keys: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM pragma_foreign_key_list('alert_event')")
                .fetch_one(&pool)
                .await
                .expect("foreign-key count");
        assert_eq!((retained, foreign_keys), (1, 0));
    }
}

// ============================================================================
// Statistics
// ============================================================================

#[derive(Debug, Default)]
pub struct AlarmCounts {
    pub total: i64,
    pub low: i64,
    pub medium: i64,
    pub high: i64,
}

pub async fn get_active_alarm_counts(pool: &SqlitePool) -> Result<AlarmCounts> {
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM alert WHERE status = 'active'")
        .fetch_one(pool)
        .await
        .unwrap_or(0);

    let rows: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT warning_level, COUNT(*) FROM alert WHERE status = 'active' GROUP BY warning_level",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let mut counts = AlarmCounts {
        total,
        ..Default::default()
    };
    for (level, cnt) in rows {
        match level {
            1 => counts.low = cnt,
            2 => counts.medium = cnt,
            3 => counts.high = cnt,
            _ => {},
        }
    }
    Ok(counts)
}

pub async fn get_statistics(pool: &SqlitePool) -> Result<serde_json::Value> {
    let counts = get_active_alarm_counts(pool).await?;

    let today_events: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM alert_event WHERE triggered_at >= ?")
            .bind(today_start_timestamp())
            .fetch_one(pool)
            .await
            .unwrap_or(0);

    Ok(serde_json::json!({
        "active_count": counts.total,
        "by_level": {
            "1": counts.low,
            "2": counts.medium,
            "3": counts.high,
        },
        "today_events": today_events,
    }))
}

fn today_start_timestamp() -> i64 {
    let now = chrono::Local::now();
    let Some(today) = now.date_naive().and_hms_opt(0, 0, 0) else {
        return now.timestamp();
    };
    chrono::Local
        .from_local_datetime(&today)
        .single()
        .map(|dt| dt.timestamp())
        .unwrap_or_else(|| now.timestamp())
}
