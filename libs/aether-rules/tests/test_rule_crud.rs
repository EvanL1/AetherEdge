//! Integration tests for Rule CRUD operations
//!
//! Tests rule creation, retrieval, update, and deletion using in-memory SQLite.

#![allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable

use aether_rules::{Result, delete_rule, extract_rule_flow, get_rule, list_rules, upsert_rule};
use serde_json::json;
use sqlx::SqlitePool;

/// Create an in-memory SQLite pool and initialize tables
async fn setup_test_db() -> SqlitePool {
    let pool = SqlitePool::connect("sqlite::memory:")
        .await
        .expect("Failed to create in-memory database");

    // Create rules table (matches actual schema)
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS rules (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            description TEXT,
            enabled INTEGER NOT NULL DEFAULT 1,
            priority INTEGER NOT NULL DEFAULT 100,
            cooldown_ms INTEGER NOT NULL DEFAULT 0,
            trigger_config TEXT,
            format TEXT NOT NULL DEFAULT 'vue-flow',
            flow_json TEXT NOT NULL,
            nodes_json TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .execute(&pool)
    .await
    .expect("Failed to create rules table");

    pool
}

/// Sample Vue Flow JSON for testing
fn sample_flow_json() -> serde_json::Value {
    json!({
        "nodes": [
            {
                "id": "start",
                "type": "start",
                "position": { "x": 0, "y": 0 },
                "data": {
                    "config": {
                        "wires": { "default": ["switch-1"] }
                    }
                }
            },
            {
                "id": "switch-1",
                "type": "custom",
                "position": { "x": 100, "y": 100 },
                "data": {
                    "type": "function-switch",
                    "label": "Check Value",
                    "config": {
                        "variables": [
                            {
                                "name": "X1",
                                "type": "single",
                                "instance": "battery_01",
                                "pointType": "M",
                                "point": 3
                            }
                        ],
                        "rule": [
                            {
                                "name": "low",
                                "type": "default",
                                "rule": [
                                    {
                                        "type": "variable",
                                        "variables": "X1",
                                        "operator": "<=",
                                        "value": 5
                                    }
                                ]
                            }
                        ],
                        "wires": {
                            "low": ["action-1"]
                        }
                    }
                }
            },
            {
                "id": "action-1",
                "type": "custom",
                "position": { "x": 300, "y": 100 },
                "data": {
                    "type": "action-changeValue",
                    "config": {
                        "variables": [
                            { "name": "Y1", "type": "single", "instance": "pv_01", "pointType": "A", "point": 1 }
                        ],
                        "rule": [
                            { "Variables": "Y1", "value": 0 }
                        ],
                        "wires": { "default": ["end"] }
                    }
                }
            },
            {
                "id": "end",
                "type": "end",
                "position": { "x": 500, "y": 100 }
            }
        ],
        "edges": []
    })
}

/// Create a complete rule JSON with all required fields
fn create_rule_json(
    id: i64,
    name: &str,
    description: &str,
    enabled: bool,
    priority: u32,
    cooldown_ms: u64,
) -> serde_json::Value {
    json!({
        "id": id,
        "name": name,
        "description": description,
        "enabled": enabled,
        "priority": priority,
        "cooldown_ms": cooldown_ms,
        "flow_json": sample_flow_json()
    })
}

#[tokio::test]
async fn test_extract_rule_flow() {
    let flow_json = sample_flow_json();
    let result = extract_rule_flow(&flow_json);
    assert!(
        result.is_ok(),
        "Failed to extract rule flow: {:?}",
        result.err()
    );

    let flow = result.unwrap();
    assert_eq!(flow.start_node, "start");
    assert_eq!(flow.nodes.len(), 4);
    assert!(flow.nodes.contains_key("start"));
    assert!(flow.nodes.contains_key("switch-1"));
    assert!(flow.nodes.contains_key("action-1"));
    assert!(flow.nodes.contains_key("end"));
}

#[tokio::test]
async fn test_rule_crud_operations() -> Result<()> {
    let pool = setup_test_db().await;

    let rule_id: i64 = 1;

    // CREATE
    let rule_json = create_rule_json(rule_id, "Test Rule", "Test description", true, 100, 5000);
    upsert_rule(&pool, rule_id, &rule_json).await?;

    // READ
    let rule = get_rule(&pool, rule_id).await?;
    assert_eq!(rule["id"].as_i64().unwrap(), rule_id);
    assert_eq!(rule["name"].as_str().unwrap(), "Test Rule");
    assert_eq!(rule["description"].as_str().unwrap(), "Test description");
    assert!(rule["enabled"].as_bool().unwrap());
    assert_eq!(rule["priority"].as_u64().unwrap(), 100);
    assert_eq!(rule["cooldown_ms"].as_u64().unwrap(), 5000);

    // LIST
    let rules = list_rules(&pool).await?;
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0]["id"].as_i64().unwrap(), rule_id);

    // UPDATE
    let updated_rule = create_rule_json(
        rule_id,
        "Updated Rule",
        "Updated description",
        false,
        200,
        10000,
    );
    upsert_rule(&pool, rule_id, &updated_rule).await?;

    let updated = get_rule(&pool, rule_id).await?;
    assert_eq!(updated["name"].as_str().unwrap(), "Updated Rule");
    assert!(!updated["enabled"].as_bool().unwrap());
    assert_eq!(updated["priority"].as_u64().unwrap(), 200);
    assert_eq!(updated["cooldown_ms"].as_u64().unwrap(), 10000);

    // DELETE
    delete_rule(&pool, rule_id).await?;

    let rules_after_delete = list_rules(&pool).await?;
    assert!(rules_after_delete.is_empty());

    Ok(())
}

#[tokio::test]
async fn test_rule_flow_parsing_edge_cases() {
    // Test empty nodes array
    let empty_flow = json!({ "nodes": [] });
    let result = extract_rule_flow(&empty_flow);
    assert!(result.is_err(), "Should fail with empty nodes");

    // Test missing start node
    let no_start = json!({
        "nodes": [
            { "id": "end", "type": "end" }
        ]
    });
    let result = extract_rule_flow(&no_start);
    assert!(result.is_err(), "Should fail without start node");

    // Test minimal valid flow
    let minimal = json!({
        "nodes": [
            {
                "id": "start",
                "type": "start",
                "data": { "config": { "wires": { "default": ["end"] } } }
            },
            { "id": "end", "type": "end" }
        ]
    });
    let result = extract_rule_flow(&minimal);
    assert!(
        result.is_ok(),
        "Minimal flow should parse: {:?}",
        result.err()
    );
}
