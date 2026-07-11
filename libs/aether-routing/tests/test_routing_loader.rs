//! Integration tests for the SQLite routing loader.
//!
//! Verifies that `load_routing_maps()` correctly reads `measurement_routing`
//! and `action_routing` tables and produces the expected key-value pairs in
//! `RoutingMaps`.

#![allow(clippy::disallowed_methods)] // Tests can use unwrap for clarity

use aether_routing::loader::load_routing_maps;
use sqlx::SqlitePool;

// ============================================================================
// Schema helpers (inlined — common is not a dev-dep of aether-routing)
// ============================================================================

/// Create all tables needed for the loader.
///
/// Foreign-key enforcement is disabled so we don't have to populate `channels`
/// or `instances` for every test.  The loader only reads routing tables.
async fn setup_schema(pool: &SqlitePool) {
    // Disable FK enforcement — simplifies seeding without parent rows
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(pool)
        .await
        .unwrap();

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS measurement_routing (
            routing_id       INTEGER PRIMARY KEY AUTOINCREMENT,
            instance_id      INTEGER NOT NULL,
            instance_name    TEXT    NOT NULL,
            channel_id       INTEGER,
            channel_type     TEXT,
            channel_point_id INTEGER,
            measurement_id   INTEGER NOT NULL,
            description      TEXT,
            enabled          INTEGER NOT NULL DEFAULT 1,
            created_at       TEXT    NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at       TEXT    NOT NULL DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS action_routing (
            routing_id       INTEGER PRIMARY KEY AUTOINCREMENT,
            instance_id      INTEGER NOT NULL,
            instance_name    TEXT    NOT NULL,
            action_id        INTEGER NOT NULL,
            channel_id       INTEGER,
            channel_type     TEXT,
            channel_point_id INTEGER,
            description      TEXT,
            enabled          INTEGER NOT NULL DEFAULT 1,
            created_at       TEXT    NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at       TEXT    NOT NULL DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .execute(pool)
    .await
    .unwrap();
}

/// Create an in-memory SQLite pool with all routing tables initialised.
async fn make_pool() -> SqlitePool {
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    setup_schema(&pool).await;
    pool
}

// ============================================================================
// Tests
// ============================================================================

/// An empty database (tables exist but no rows) should produce empty RoutingMaps.
#[tokio::test]
async fn test_load_empty_database() {
    let pool = make_pool().await;

    let maps = load_routing_maps(&pool).await.unwrap();

    assert!(maps.c2m.is_empty(), "c2m should be empty");
    assert!(maps.m2c.is_empty(), "m2c should be empty");
    assert!(maps.c2c.is_empty(), "c2c should be empty");
    assert_eq!(maps.total_routes(), 0);
}

/// A single measurement-routing row should produce one C2M entry with the
/// correct key format: `"{channel_id}:{type_char}:{channel_point_id}"` →
/// `"{instance_id}:M:{measurement_id}"`.
#[tokio::test]
async fn test_load_c2m_measurement_routing() {
    let pool = make_pool().await;

    sqlx::query(
        r#"
        INSERT INTO measurement_routing
            (instance_id, instance_name, channel_id, channel_type,
             channel_point_id, measurement_id, enabled)
        VALUES (10, 'TestInstance', 1001, 'T', 5, 42, 1)
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let maps = load_routing_maps(&pool).await.unwrap();

    assert_eq!(maps.c2m.len(), 1, "expected exactly one C2M route");
    assert!(maps.m2c.is_empty());

    // Key: channel_id:type_char:channel_point_id
    let from_key = "1001:T:5";
    // Value: instance_id:M:measurement_id
    let expected_to = "10:M:42";

    let actual = maps.c2m.get(from_key).expect("C2M key not found");
    assert_eq!(actual, expected_to, "C2M target mismatch");
}

/// A single action-routing row should produce one M2C entry.
///
/// The loader hardcodes `"A"` as the from-key segment (not the channel_type),
/// so the key format is `"{instance_id}:A:{action_id}"` →
/// `"{channel_id}:{channel_type_char}:{channel_point_id}"`.
#[tokio::test]
async fn test_load_m2c_action_routing() {
    let pool = make_pool().await;

    // Control action (channel_type = 'C')
    sqlx::query(
        r#"
        INSERT INTO action_routing
            (instance_id, instance_name, action_id, channel_id, channel_type,
             channel_point_id, enabled)
        VALUES (20, 'TestInstance', 3, 2001, 'C', 7, 1)
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let maps = load_routing_maps(&pool).await.unwrap();

    assert!(maps.c2m.is_empty());
    assert_eq!(maps.m2c.len(), 1, "expected exactly one M2C route");

    // Key: instance_id:A:action_id  (loader always uses literal "A")
    let from_key = "20:A:3";
    // Value: channel_id:channel_type_char:channel_point_id
    let expected_to = "2001:C:7";

    let actual = maps.m2c.get(from_key).expect("M2C key not found");
    assert_eq!(actual, expected_to, "M2C target mismatch");
}

/// An Adjustment action (channel_type = 'A') should produce the correct
/// key — verifying the type character appears in the value, not the key.
#[tokio::test]
async fn test_load_m2c_adjustment_type_key_format() {
    let pool = make_pool().await;

    sqlx::query(
        r#"
        INSERT INTO action_routing
            (instance_id, instance_name, action_id, channel_id, channel_type,
             channel_point_id, enabled)
        VALUES (30, 'AdjustInstance', 9, 3001, 'A', 12, 1)
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let maps = load_routing_maps(&pool).await.unwrap();

    // Key always uses "A" for action (instance side), value uses channel_type 'A'
    let actual = maps.m2c.get("30:A:9").expect("M2C key not found");
    assert_eq!(actual, "3001:A:12");
}

/// Rows with `enabled = 0` (or false) must be excluded from the loaded maps.
#[tokio::test]
async fn test_load_skips_disabled_routes() {
    let pool = make_pool().await;

    // One enabled C2M route
    sqlx::query(
        r#"
        INSERT INTO measurement_routing
            (instance_id, instance_name, channel_id, channel_type,
             channel_point_id, measurement_id, enabled)
        VALUES (11, 'Enabled', 1001, 'T', 1, 100, 1)
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    // One disabled C2M route — should be skipped
    sqlx::query(
        r#"
        INSERT INTO measurement_routing
            (instance_id, instance_name, channel_id, channel_type,
             channel_point_id, measurement_id, enabled)
        VALUES (12, 'Disabled', 1002, 'T', 2, 200, 0)
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    // One enabled M2C route
    sqlx::query(
        r#"
        INSERT INTO action_routing
            (instance_id, instance_name, action_id, channel_id, channel_type,
             channel_point_id, enabled)
        VALUES (11, 'Enabled', 1, 1001, 'C', 1, 1)
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    // One disabled M2C route — should be skipped
    sqlx::query(
        r#"
        INSERT INTO action_routing
            (instance_id, instance_name, action_id, channel_id, channel_type,
             channel_point_id, enabled)
        VALUES (12, 'Disabled', 2, 1002, 'C', 2, 0)
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let maps = load_routing_maps(&pool).await.unwrap();

    assert_eq!(maps.c2m.len(), 1, "only the enabled C2M route should load");
    assert!(maps.c2m.contains_key("1001:T:1"), "enabled C2M key absent");
    assert!(
        !maps.c2m.contains_key("1002:T:2"),
        "disabled C2M key present"
    );

    assert_eq!(maps.m2c.len(), 1, "only the enabled M2C route should load");
    assert!(maps.m2c.contains_key("11:A:1"), "enabled M2C key absent");
    assert!(!maps.m2c.contains_key("12:A:2"), "disabled M2C key present");
}

/// Multiple points on the same channel must all be loaded correctly.
#[tokio::test]
async fn test_load_multiple_routes_same_channel() {
    let pool = make_pool().await;

    // Three telemetry points on channel 5000 → instance 50
    for point_id in [1u32, 2, 3] {
        let measurement_id = point_id * 10;
        sqlx::query(
            r#"
            INSERT INTO measurement_routing
                (instance_id, instance_name, channel_id, channel_type,
                 channel_point_id, measurement_id, enabled)
            VALUES (?, 'MultiPoint', ?, 'T', ?, ?, 1)
            "#,
        )
        .bind(50i64)
        .bind(5000i64)
        .bind(point_id)
        .bind(measurement_id)
        .execute(&pool)
        .await
        .unwrap();
    }

    let maps = load_routing_maps(&pool).await.unwrap();

    assert_eq!(maps.c2m.len(), 3, "expected 3 C2M routes");

    assert_eq!(
        maps.c2m.get("5000:T:1").map(String::as_str),
        Some("50:M:10")
    );
    assert_eq!(
        maps.c2m.get("5000:T:2").map(String::as_str),
        Some("50:M:20")
    );
    assert_eq!(
        maps.c2m.get("5000:T:3").map(String::as_str),
        Some("50:M:30")
    );
}

/// Measurement routing supports T (Telemetry) and S (Signal) channel types.
/// Action routing supports C (Control) and A (Adjustment) channel types.
/// Verify the correct type character appears in each generated key.
#[tokio::test]
async fn test_load_mixed_point_types() {
    let pool = make_pool().await;

    // Telemetry (T) measurement
    sqlx::query(
        r#"
        INSERT INTO measurement_routing
            (instance_id, instance_name, channel_id, channel_type,
             channel_point_id, measurement_id, enabled)
        VALUES (1, 'Inst1', 100, 'T', 1, 1, 1)
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    // Signal (S) measurement
    sqlx::query(
        r#"
        INSERT INTO measurement_routing
            (instance_id, instance_name, channel_id, channel_type,
             channel_point_id, measurement_id, enabled)
        VALUES (2, 'Inst2', 200, 'S', 2, 2, 1)
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    // Control (C) action
    sqlx::query(
        r#"
        INSERT INTO action_routing
            (instance_id, instance_name, action_id, channel_id, channel_type,
             channel_point_id, enabled)
        VALUES (3, 'Inst3', 10, 300, 'C', 3, 1)
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    // Adjustment (A) action
    sqlx::query(
        r#"
        INSERT INTO action_routing
            (instance_id, instance_name, action_id, channel_id, channel_type,
             channel_point_id, enabled)
        VALUES (4, 'Inst4', 20, 400, 'A', 4, 1)
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let maps = load_routing_maps(&pool).await.unwrap();

    assert_eq!(maps.c2m.len(), 2);
    assert_eq!(maps.m2c.len(), 2);

    // C2M: type char is in the from-key (channel side)
    assert_eq!(
        maps.c2m.get("100:T:1").map(String::as_str),
        Some("1:M:1"),
        "Telemetry C2M key/value mismatch"
    );
    assert_eq!(
        maps.c2m.get("200:S:2").map(String::as_str),
        Some("2:M:2"),
        "Signal C2M key/value mismatch"
    );

    // M2C: from-key always uses 'A' (action); type char is in the to-value (channel side)
    assert_eq!(
        maps.m2c.get("3:A:10").map(String::as_str),
        Some("300:C:3"),
        "Control M2C key/value mismatch"
    );
    assert_eq!(
        maps.m2c.get("4:A:20").map(String::as_str),
        Some("400:A:4"),
        "Adjustment M2C key/value mismatch"
    );
}

/// Rows with an invalid `channel_type` (not T/S/C/A) are silently skipped.
/// The loader logs a warning and continues rather than returning an error.
#[tokio::test]
async fn test_load_skips_invalid_channel_type() {
    let pool = make_pool().await;

    // Valid row
    sqlx::query(
        r#"
        INSERT INTO measurement_routing
            (instance_id, instance_name, channel_id, channel_type,
             channel_point_id, measurement_id, enabled)
        VALUES (1, 'Valid', 100, 'T', 1, 1, 1)
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    // Row with invalid channel_type ('X') — must be skipped
    // Note: we bypass the CHECK constraint by inserting via a separate statement
    // with FK off (already disabled in setup_schema)
    sqlx::query(
        r#"
        INSERT INTO measurement_routing
            (instance_id, instance_name, channel_id, channel_type,
             channel_point_id, measurement_id, enabled)
        VALUES (2, 'Invalid', 200, 'X', 2, 2, 1)
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let maps = load_routing_maps(&pool).await.unwrap();

    // Only the valid row should appear
    assert_eq!(
        maps.c2m.len(),
        1,
        "invalid channel_type row must be skipped"
    );
    assert!(maps.c2m.contains_key("100:T:1"));
    assert!(!maps.c2m.contains_key("200:X:2"));
}

/// `total_routes()` should equal the sum of c2m + m2c + c2c counts.
#[tokio::test]
async fn test_total_routes_counts_all_maps() {
    let pool = make_pool().await;

    // 2 C2M + 1 M2C
    sqlx::query(
        r#"
        INSERT INTO measurement_routing
            (instance_id, instance_name, channel_id, channel_type,
             channel_point_id, measurement_id, enabled)
        VALUES
            (1, 'A', 100, 'T', 1, 1, 1),
            (2, 'B', 100, 'S', 2, 2, 1)
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query(
        r#"
        INSERT INTO action_routing
            (instance_id, instance_name, action_id, channel_id, channel_type,
             channel_point_id, enabled)
        VALUES (1, 'A', 1, 100, 'C', 1, 1)
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let maps = load_routing_maps(&pool).await.unwrap();

    // c2c is always 0 here (no channel_routing table)
    assert_eq!(
        maps.total_routes(),
        maps.c2m.len() + maps.m2c.len() + maps.c2c.len()
    );
    assert_eq!(maps.total_routes(), 3);
}
