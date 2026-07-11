//! Database operations for alert_rule, alert and alert_event tables.
//!
//! Uses runtime SQLx queries (no compile-time macros) as required by project conventions.

use anyhow::{Context, Result};
use chrono::{TimeZone, Utc};
use sqlx::SqlitePool;
use tracing::{debug, info};

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
            duration        INTEGER,
            FOREIGN KEY (rule_id) REFERENCES alert_rule(id)
        )
        "#,
    )
    .execute(pool)
    .await
    .context("create alert_event table")?;

    info!("Alert tables ready");
    Ok(())
}

// ============================================================================
// AlertRule CRUD
// ============================================================================

#[allow(clippy::too_many_arguments)]
pub async fn insert_rule(
    pool: &SqlitePool,
    service_type: &str,
    channel_id: i64,
    data_type: &str,
    point_id: i64,
    rule_name: &str,
    warning_level: i64,
    operator: &str,
    value: f64,
    enabled: bool,
    description: Option<&str>,
) -> Result<i64> {
    let now = Utc::now().timestamp();
    let enabled_int: i64 = if enabled { 1 } else { 0 };

    let id = sqlx::query_scalar::<_, i64>(
        r#"
        INSERT INTO alert_rule
            (service_type, channel_id, data_type, point_id, rule_name, warning_level,
             operator, value, enabled, description, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        RETURNING id
        "#,
    )
    .bind(service_type)
    .bind(channel_id)
    .bind(data_type)
    .bind(point_id)
    .bind(rule_name)
    .bind(warning_level)
    .bind(operator)
    .bind(value)
    .bind(enabled_int)
    .bind(description)
    .bind(now)
    .bind(now)
    .fetch_one(pool)
    .await
    .context("insert alert_rule")?;

    debug!("Created rule id={}", id);
    Ok(id)
}

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
pub async fn find_rule_by_name(pool: &SqlitePool, rule_name: &str) -> Result<Option<AlertRule>> {
    sqlx::query_as::<_, AlertRule>(
        "SELECT * FROM alert_rule WHERE LOWER(rule_name) = LOWER(?) LIMIT 1",
    )
    .bind(rule_name)
    .fetch_optional(pool)
    .await
    .context("find rule by name")
}

/// Check whether a rule already exists for the given (service_type, channel_id, data_type, point_id)
/// combination. Returns the first matching rule (enabled or disabled) if found.
pub async fn find_rule_by_point(
    pool: &SqlitePool,
    service_type: &str,
    channel_id: i64,
    data_type: &str,
    point_id: i64,
) -> Result<Option<AlertRule>> {
    sqlx::query_as::<_, AlertRule>(
        "SELECT * FROM alert_rule WHERE service_type = ? AND channel_id = ? AND data_type = ? AND point_id = ? LIMIT 1",
    )
    .bind(service_type)
    .bind(channel_id)
    .bind(data_type)
    .bind(point_id)
    .fetch_optional(pool)
    .await
    .context("find rule by point")
}

pub async fn get_all_enabled_rules(pool: &SqlitePool) -> Result<Vec<AlertRule>> {
    sqlx::query_as::<_, AlertRule>("SELECT * FROM alert_rule WHERE enabled = 1 ORDER BY id ASC")
        .fetch_all(pool)
        .await
        .context("get enabled rules")
}

#[allow(clippy::too_many_arguments)]
pub async fn update_rule(
    pool: &SqlitePool,
    id: i64,
    service_type: Option<&str>,
    channel_id: Option<i64>,
    data_type: Option<&str>,
    point_id: Option<i64>,
    rule_name: Option<&str>,
    warning_level: Option<i64>,
    operator: Option<&str>,
    value: Option<f64>,
    enabled: Option<bool>,
    description: Option<Option<&str>>,
) -> Result<bool> {
    let now = Utc::now().timestamp();
    let mut set_clauses: Vec<String> = Vec::new();

    if service_type.is_some() {
        set_clauses.push("service_type = ?".into());
    }
    if channel_id.is_some() {
        set_clauses.push("channel_id = ?".into());
    }
    if data_type.is_some() {
        set_clauses.push("data_type = ?".into());
    }
    if point_id.is_some() {
        set_clauses.push("point_id = ?".into());
    }
    if rule_name.is_some() {
        set_clauses.push("rule_name = ?".into());
    }
    if warning_level.is_some() {
        set_clauses.push("warning_level = ?".into());
    }
    if operator.is_some() {
        set_clauses.push("operator = ?".into());
    }
    if value.is_some() {
        set_clauses.push("value = ?".into());
    }
    if enabled.is_some() {
        set_clauses.push("enabled = ?".into());
    }
    if description.is_some() {
        set_clauses.push("description = ?".into());
    }

    if set_clauses.is_empty() {
        return Ok(false);
    }

    set_clauses.push("updated_at = ?".into());
    let sql = format!(
        "UPDATE alert_rule SET {} WHERE id = ?",
        set_clauses.join(", ")
    );

    let mut q = sqlx::query(&sql);
    if let Some(v) = service_type {
        q = q.bind(v);
    }
    if let Some(v) = channel_id {
        q = q.bind(v);
    }
    if let Some(v) = data_type {
        q = q.bind(v);
    }
    if let Some(v) = point_id {
        q = q.bind(v);
    }
    if let Some(v) = rule_name {
        q = q.bind(v);
    }
    if let Some(v) = warning_level {
        q = q.bind(v);
    }
    if let Some(v) = operator {
        q = q.bind(v);
    }
    if let Some(v) = value {
        q = q.bind(v);
    }
    if let Some(v) = enabled {
        q = q.bind(if v { 1i64 } else { 0i64 });
    }
    if let Some(v) = description {
        q = q.bind(v);
    }
    q = q.bind(now).bind(id);

    let result = q.execute(pool).await.context("update rule")?;
    Ok(result.rows_affected() > 0)
}

pub async fn set_rule_enabled(pool: &SqlitePool, id: i64, enabled: bool) -> Result<bool> {
    let now = Utc::now().timestamp();
    let result = sqlx::query("UPDATE alert_rule SET enabled = ?, updated_at = ? WHERE id = ?")
        .bind(if enabled { 1i64 } else { 0i64 })
        .bind(now)
        .bind(id)
        .execute(pool)
        .await
        .context("set rule enabled")?;
    Ok(result.rows_affected() > 0)
}

pub async fn delete_rule(pool: &SqlitePool, id: i64) -> Result<bool> {
    let result = sqlx::query("DELETE FROM alert_rule WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .context("delete rule")?;
    Ok(result.rows_affected() > 0)
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

/// Resolves all alerts for a rule (used when rule is disabled or deleted).
/// Returns the list of resolved alert IDs.
pub async fn resolve_alerts_by_rule_id(pool: &SqlitePool, rule_id: i64) -> Result<Vec<Alert>> {
    let alerts = sqlx::query_as::<_, Alert>("SELECT * FROM alert WHERE rule_id = ?")
        .bind(rule_id)
        .fetch_all(pool)
        .await
        .context("get alerts by rule_id")?;

    if alerts.is_empty() {
        return Ok(Vec::new());
    }

    let now = Utc::now().timestamp();
    let mut tx = pool.begin().await.context("begin transaction")?;

    for alert in &alerts {
        let duration = now - alert.triggered_at;

        sqlx::query(
            r#"
            INSERT INTO alert_event
                (rule_id, rule_snapshot, service_type, channel_id, data_type, point_id,
                 rule_name, warning_level, operator, threshold_value,
                 trigger_value, recovery_value, event_type,
                 triggered_at, recovered_at, duration)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, 'recovery', ?, ?, ?)
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
        .bind(alert.triggered_at)
        .bind(now)
        .bind(duration)
        .execute(&mut *tx)
        .await
        .context("insert alert_event in bulk resolve")?;

        sqlx::query("DELETE FROM alert WHERE id = ?")
            .bind(alert.id)
            .execute(&mut *tx)
            .await
            .context("delete alert in bulk resolve")?;
    }

    tx.commit().await.context("commit bulk resolve")?;
    Ok(alerts)
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
