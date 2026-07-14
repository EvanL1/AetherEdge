use std::sync::Arc;

use aether_application::{Actor, ApplicationError, RequestContext};
use aether_domain::TimestampMs;
use aether_io::point_topology::{
    PointDefinitionMutation, PointKind, PointMappingMutation, PointMutation,
    PointTopologyApplication, PointTopologyMutation,
};
use aether_ports::{
    AuditOutcome, AuditRecord, AuditSink, ChannelRevision, PortError, PortErrorKind, PortResult,
};
use async_trait::async_trait;
use sqlx::SqlitePool;

async fn configured_pool() -> SqlitePool {
    let pool = SqlitePool::connect("sqlite::memory:")
        .await
        .expect("in-memory SQLite");
    common::test_utils::schema::init_io_schema(&pool)
        .await
        .expect("I/O schema");
    common::test_utils::schema::init_automation_schema(&pool)
        .await
        .expect("logical routing schema");
    sqlx::query(
        "INSERT INTO channels (channel_id, name, protocol, enabled, config, revision) \
         VALUES (7, 'governed points', 'modbus_tcp', 0, '{}', 1)",
    )
    .execute(&pool)
    .await
    .expect("channel fixture");
    pool
}

fn context() -> RequestContext {
    RequestContext::new(
        "018f0000-0000-7000-8000-000000000071",
        Actor::new("operator:7").with_permission("io.channel.manage"),
        true,
        TimestampMs::new(7_000),
    )
}

fn create_command(signal_name: &str) -> PointTopologyMutation {
    PointTopologyMutation::single(
        7,
        PointMutation::Create {
            kind: PointKind::Telemetry,
            definition: PointDefinitionMutation {
                point_id: 1,
                signal_name: signal_name.to_string(),
                scale: 1.0,
                offset: 0.0,
                unit: "C".to_string(),
                reverse: false,
                data_type: "f64".to_string(),
                description: String::new(),
                normal_state: 0,
                minimum: None,
                maximum: None,
                step: 1.0,
                protocol_mapping: Some(None),
            },
            force: false,
        },
    )
}

#[tokio::test]
async fn successful_command_audits_around_one_cas_transaction() {
    let pool = configured_pool().await;
    let audit = Arc::new(aether_store_local::MemoryAuditSink::new());
    let application = PointTopologyApplication::new(pool.clone(), audit.clone());

    let acceptance = application
        .mutate(
            &context(),
            Some(ChannelRevision::new(1)),
            create_command("temperature"),
        )
        .await
        .expect("governed point mutation");

    assert_eq!(acceptance.resulting_revision(), ChannelRevision::new(2));
    assert!(acceptance.completion_audit().is_recorded());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT revision FROM channels WHERE channel_id = 7")
            .fetch_one(&pool)
            .await
            .expect("revision"),
        2
    );
    assert_eq!(
        audit
            .records()
            .expect("audit records")
            .iter()
            .map(AuditRecord::outcome)
            .collect::<Vec<_>>(),
        vec![AuditOutcome::Attempted, AuditOutcome::Succeeded]
    );
}

#[tokio::test]
async fn missing_or_stale_revision_never_writes_a_point() {
    for expected_revision in [None, Some(ChannelRevision::new(9))] {
        let pool = configured_pool().await;
        let application = PointTopologyApplication::new(
            pool.clone(),
            Arc::new(aether_store_local::MemoryAuditSink::new()),
        );
        assert!(
            application
                .mutate(&context(), expected_revision, create_command("temperature"))
                .await
                .is_err()
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM telemetry_points WHERE channel_id = 7"
            )
            .fetch_one(&pool)
            .await
            .expect("point count"),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT revision FROM channels WHERE channel_id = 7")
                .fetch_one(&pool)
                .await
                .expect("unchanged revision"),
            1
        );
    }
}

#[tokio::test]
async fn invalid_definition_rolls_back_the_revision_and_records_failure() {
    let pool = configured_pool().await;
    let audit = Arc::new(aether_store_local::MemoryAuditSink::new());
    let application = PointTopologyApplication::new(pool.clone(), audit.clone());

    let result = application
        .mutate(
            &context(),
            Some(ChannelRevision::new(1)),
            create_command("  "),
        )
        .await;

    assert!(matches!(result, Err(ApplicationError::Port(_))));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT revision FROM channels WHERE channel_id = 7")
            .fetch_one(&pool)
            .await
            .expect("rolled-back revision"),
        1
    );
    assert_eq!(
        audit
            .records()
            .expect("audit records")
            .iter()
            .map(AuditRecord::outcome)
            .collect::<Vec<_>>(),
        vec![AuditOutcome::Attempted, AuditOutcome::Failed]
    );
}

struct UnavailableAudit;

#[async_trait]
impl AuditSink for UnavailableAudit {
    async fn record(&self, _record: AuditRecord) -> PortResult<()> {
        Err(PortError::new(
            PortErrorKind::Unavailable,
            "audit unavailable",
        ))
    }
}

#[tokio::test]
async fn unavailable_attempt_audit_fails_closed_before_sql() {
    let pool = configured_pool().await;
    let application = PointTopologyApplication::new(pool.clone(), Arc::new(UnavailableAudit));
    let result = application
        .mutate(
            &context(),
            Some(ChannelRevision::new(1)),
            create_command("temperature"),
        )
        .await;
    assert!(matches!(result, Err(ApplicationError::AuditUnavailable(_))));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM telemetry_points")
            .fetch_one(&pool)
            .await
            .expect("point count"),
        0
    );
}

#[tokio::test]
async fn deleting_a_logically_routed_measurement_fails_without_trigger_side_effects() {
    let pool = configured_pool().await;
    sqlx::query(
        "INSERT INTO telemetry_points \
         (channel_id, point_id, signal_name, scale, offset, unit, data_type, reverse, description) \
         VALUES (7, 1, 'temperature', 1, 0, 'C', 'f64', 0, '')",
    )
    .execute(&pool)
    .await
    .expect("point fixture");
    sqlx::query(
        "INSERT INTO instances (instance_id, instance_name, product_name) \
         VALUES (1, 'site', 'test')",
    )
    .execute(&pool)
    .await
    .expect("instance fixture");
    sqlx::query(
        "INSERT INTO measurement_routing \
         (instance_id, instance_name, channel_id, channel_type, channel_point_id, measurement_id) \
         VALUES (1, 'site', 7, 'T', 1, 1)",
    )
    .execute(&pool)
    .await
    .expect("route fixture");
    let application = PointTopologyApplication::new(
        pool.clone(),
        Arc::new(aether_store_local::MemoryAuditSink::new()),
    );
    let result = application
        .mutate(
            &context(),
            Some(ChannelRevision::new(1)),
            PointTopologyMutation::single(
                7,
                PointMutation::Delete {
                    kind: PointKind::Telemetry,
                    point_id: 1,
                },
            ),
        )
        .await;
    assert!(
        matches!(result, Err(ApplicationError::Port(ref error)) if error.kind() == PortErrorKind::Conflict)
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM telemetry_points")
            .fetch_one(&pool)
            .await
            .expect("point retained"),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM measurement_routing")
            .fetch_one(&pool)
            .await
            .expect("route retained"),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT revision FROM channels WHERE channel_id = 7")
            .fetch_one(&pool)
            .await
            .expect("revision rolled back"),
        1
    );
}

#[tokio::test]
async fn deleting_a_logically_routed_action_target_fails_without_id_reuse() {
    let pool = configured_pool().await;
    sqlx::query(
        "INSERT INTO control_points \
         (channel_id, point_id, signal_name, scale, offset, unit, data_type, reverse, description) \
         VALUES (7, 1, 'start', 1, 0, '', 'bool', 0, '')",
    )
    .execute(&pool)
    .await
    .expect("control fixture");
    sqlx::query(
        "INSERT INTO instances (instance_id, instance_name, product_name) \
         VALUES (1, 'site', 'test')",
    )
    .execute(&pool)
    .await
    .expect("instance fixture");
    sqlx::query(
        "INSERT INTO action_routing \
         (instance_id, instance_name, action_id, channel_id, channel_type, channel_point_id) \
         VALUES (1, 'site', 1, 7, 'C', 1)",
    )
    .execute(&pool)
    .await
    .expect("action route fixture");
    let application = PointTopologyApplication::new(
        pool.clone(),
        Arc::new(aether_store_local::MemoryAuditSink::new()),
    );
    let result = application
        .mutate(
            &context(),
            Some(ChannelRevision::new(1)),
            PointTopologyMutation::single(
                7,
                PointMutation::Delete {
                    kind: PointKind::Control,
                    point_id: 1,
                },
            ),
        )
        .await;
    assert!(
        matches!(result, Err(ApplicationError::Port(ref error)) if error.kind() == PortErrorKind::Conflict)
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM control_points")
            .fetch_one(&pool)
            .await
            .expect("control retained"),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM action_routing")
            .fetch_one(&pool)
            .await
            .expect("action route retained"),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT revision FROM channels WHERE channel_id = 7")
            .fetch_one(&pool)
            .await
            .expect("revision rolled back"),
        1
    );
}

#[tokio::test]
async fn replace_existing_checks_action_routes_before_clearing_any_plane() {
    let pool = configured_pool().await;
    sqlx::query(
        "INSERT INTO control_points \
         (channel_id, point_id, signal_name, scale, offset, unit, data_type, reverse, description) \
         VALUES (7, 1, 'start', 1, 0, '', 'bool', 0, '')",
    )
    .execute(&pool)
    .await
    .expect("control fixture");
    sqlx::query(
        "INSERT INTO instances (instance_id, instance_name, product_name) \
         VALUES (1, 'site', 'test')",
    )
    .execute(&pool)
    .await
    .expect("instance fixture");
    sqlx::query(
        "INSERT INTO action_routing \
         (instance_id, instance_name, action_id, channel_id, channel_type, channel_point_id) \
         VALUES (1, 'site', 1, 7, 'C', 1)",
    )
    .execute(&pool)
    .await
    .expect("action route fixture");
    let replacement = match create_command("replacement") {
        PointTopologyMutation::Single {
            mutation: PointMutation::Create { definition, .. },
            ..
        } => definition,
        _ => unreachable!("create helper returns a definition"),
    };
    let application = PointTopologyApplication::new(
        pool.clone(),
        Arc::new(aether_store_local::MemoryAuditSink::new()),
    );
    let result = application
        .mutate(
            &context(),
            Some(ChannelRevision::new(1)),
            PointTopologyMutation::Provision {
                channel_id: 7,
                replace_existing: true,
                upsert_existing: false,
                points: vec![(PointKind::Telemetry, replacement)],
            },
        )
        .await;
    assert!(
        matches!(result, Err(ApplicationError::Port(ref error)) if error.kind() == PortErrorKind::Conflict)
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM control_points")
            .fetch_one(&pool)
            .await
            .expect("control retained"),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM telemetry_points")
            .fetch_one(&pool)
            .await
            .expect("replacement absent"),
        0
    );
}

#[tokio::test]
async fn force_upsert_updates_signal_and_adjustment_specific_fields() {
    let pool = configured_pool().await;
    sqlx::query(
        "INSERT INTO signal_points \
         (channel_id, point_id, signal_name, scale, offset, unit, reverse, normal_state, data_type, description) \
         VALUES (7, 1, 'state', 1, 0, '', 0, 0, 'bool', '')",
    )
    .execute(&pool)
    .await
    .expect("signal fixture");
    sqlx::query(
        "INSERT INTO adjustment_points \
         (channel_id, point_id, signal_name, scale, offset, unit, reverse, data_type, description, min_value, max_value, step) \
         VALUES (7, 2, 'setpoint', 1, 0, 'kW', 0, 'f64', '', 0, 10, 1)",
    )
    .execute(&pool)
    .await
    .expect("adjustment fixture");
    let application = PointTopologyApplication::new(
        pool.clone(),
        Arc::new(aether_store_local::MemoryAuditSink::new()),
    );
    application
        .mutate(
            &context(),
            Some(ChannelRevision::new(1)),
            PointTopologyMutation::single(
                7,
                PointMutation::Create {
                    kind: PointKind::Signal,
                    definition: PointDefinitionMutation {
                        point_id: 1,
                        signal_name: "state".to_string(),
                        scale: 1.0,
                        offset: 0.0,
                        unit: String::new(),
                        reverse: false,
                        data_type: "bool".to_string(),
                        description: String::new(),
                        normal_state: 1,
                        minimum: None,
                        maximum: None,
                        step: 1.0,
                        protocol_mapping: None,
                    },
                    force: true,
                },
            ),
        )
        .await
        .expect("signal upsert");
    application
        .mutate(
            &context(),
            Some(ChannelRevision::new(2)),
            PointTopologyMutation::single(
                7,
                PointMutation::Create {
                    kind: PointKind::Adjustment,
                    definition: PointDefinitionMutation {
                        point_id: 2,
                        signal_name: "setpoint".to_string(),
                        scale: 1.0,
                        offset: 0.0,
                        unit: "kW".to_string(),
                        reverse: false,
                        data_type: "f64".to_string(),
                        description: String::new(),
                        normal_state: 0,
                        minimum: Some(-5.0),
                        maximum: Some(20.0),
                        step: 0.5,
                        protocol_mapping: None,
                    },
                    force: true,
                },
            ),
        )
        .await
        .expect("adjustment upsert");
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT normal_state FROM signal_points WHERE channel_id = 7 AND point_id = 1"
        )
        .fetch_one(&pool)
        .await
        .expect("normal_state"),
        1
    );
    let constraints: (f64, f64, f64) = sqlx::query_as(
        "SELECT min_value, max_value, step FROM adjustment_points \
         WHERE channel_id = 7 AND point_id = 2",
    )
    .fetch_one(&pool)
    .await
    .expect("adjustment constraints");
    assert_eq!(constraints, (-5.0, 20.0, 0.5));
}

#[tokio::test]
async fn batch_item_failure_rolls_back_every_point_and_the_revision() {
    let pool = configured_pool().await;
    let application = PointTopologyApplication::new(
        pool.clone(),
        Arc::new(aether_store_local::MemoryAuditSink::new()),
    );
    let create = match create_command("temperature") {
        PointTopologyMutation::Single { mutation, .. } => mutation,
        _ => unreachable!("create helper returns one mutation"),
    };
    let result = application
        .mutate(
            &context(),
            Some(ChannelRevision::new(1)),
            PointTopologyMutation::Batch {
                channel_id: 7,
                mutations: vec![
                    create,
                    PointMutation::Delete {
                        kind: PointKind::Telemetry,
                        point_id: 999,
                    },
                ],
            },
        )
        .await;
    assert!(matches!(result, Err(ApplicationError::Port(_))));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM telemetry_points")
            .fetch_one(&pool)
            .await
            .expect("rolled-back point count"),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT revision FROM channels WHERE channel_id = 7")
            .fetch_one(&pool)
            .await
            .expect("rolled-back revision"),
        1
    );
}

#[tokio::test]
async fn mapping_update_executes_inside_the_same_revision_transaction() {
    let pool = configured_pool().await;
    sqlx::query(
        "INSERT INTO telemetry_points \
         (channel_id, point_id, signal_name, scale, offset, unit, data_type, reverse, description) \
         VALUES (7, 1, 'temperature', 1, 0, 'C', 'f64', 0, '')",
    )
    .execute(&pool)
    .await
    .expect("point fixture");
    let application = PointTopologyApplication::new(
        pool.clone(),
        Arc::new(aether_store_local::MemoryAuditSink::new()),
    );
    application
        .mutate(
            &context(),
            Some(ChannelRevision::new(1)),
            PointTopologyMutation::Mappings {
                channel_id: 7,
                merge: false,
                mappings: vec![PointMappingMutation {
                    kind: PointKind::Telemetry,
                    point_id: 1,
                    protocol_data: serde_json::json!({
                        "slave_id": 1,
                        "function_code": 3,
                        "register_address": 17,
                        "data_type": "float32",
                        "byte_order": "ABCD"
                    }),
                }],
            },
        )
        .await
        .expect("mapping mutation");
    let stored: String = sqlx::query_scalar(
        "SELECT protocol_mappings FROM telemetry_points WHERE channel_id = 7 AND point_id = 1",
    )
    .fetch_one(&pool)
    .await
    .expect("stored mapping");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&stored).expect("mapping JSON"),
        serde_json::json!({
            "slave_id": 1,
            "function_code": 3,
            "register_address": 17,
            "data_type": "float32",
            "byte_order": "ABCD"
        })
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT revision FROM channels WHERE channel_id = 7")
            .fetch_one(&pool)
            .await
            .expect("revision"),
        2
    );
}

#[tokio::test]
async fn mqtt_jsonpath_mapping_uses_the_point_owned_cas_transaction() {
    let pool = configured_pool().await;
    sqlx::query("UPDATE channels SET protocol = 'mqtt' WHERE channel_id = 7")
        .execute(&pool)
        .await
        .expect("MQTT channel fixture");
    sqlx::query(
        "INSERT INTO telemetry_points \
         (channel_id, point_id, signal_name, scale, offset, unit, data_type, reverse, description) \
         VALUES (7, 1, 'temperature', 1, 0, 'C', 'f64', 0, '')",
    )
    .execute(&pool)
    .await
    .expect("point fixture");
    let application = PointTopologyApplication::new(
        pool.clone(),
        Arc::new(aether_store_local::MemoryAuditSink::new()),
    );

    application
        .mutate(
            &context(),
            Some(ChannelRevision::new(2)),
            PointTopologyMutation::Mappings {
                channel_id: 7,
                merge: false,
                mappings: vec![PointMappingMutation {
                    kind: PointKind::Telemetry,
                    point_id: 1,
                    protocol_data: serde_json::json!({
                        "json_path": "$.measurements.temperature",
                        "data_type": "float",
                        "scale": 0.1,
                        "offset": -5.0
                    }),
                }],
            },
        )
        .await
        .expect("governed MQTT mapping");
    let stored: String = sqlx::query_scalar(
        "SELECT protocol_mappings FROM telemetry_points WHERE channel_id = 7 AND point_id = 1",
    )
    .fetch_one(&pool)
    .await
    .expect("stored inline mapping");

    let invalid = application
        .mutate(
            &context(),
            Some(ChannelRevision::new(3)),
            PointTopologyMutation::Mappings {
                channel_id: 7,
                merge: false,
                mappings: vec![PointMappingMutation {
                    kind: PointKind::Telemetry,
                    point_id: 1,
                    protocol_data: serde_json::json!({"json_path": "invalid[[["}),
                }],
            },
        )
        .await;

    assert!(matches!(
        invalid,
        Err(ApplicationError::Port(ref error)) if error.kind() == PortErrorKind::InvalidData
    ));
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT protocol_mappings FROM telemetry_points WHERE channel_id = 7 AND point_id = 1"
        )
        .fetch_one(&pool)
        .await
        .expect("valid mapping retained"),
        stored
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT revision FROM channels WHERE channel_id = 7")
            .fetch_one(&pool)
            .await
            .expect("failed mutation revision rolled back"),
        3
    );
}

#[tokio::test]
async fn merge_rejects_corrupt_existing_mapping_without_overwriting_it() {
    let pool = configured_pool().await;
    sqlx::query(
        "INSERT INTO telemetry_points \
         (channel_id, point_id, signal_name, scale, offset, unit, data_type, reverse, description, protocol_mappings) \
         VALUES (7, 1, 'temperature', 1, 0, 'C', 'f64', 0, '', '{broken')",
    )
    .execute(&pool)
    .await
    .expect("corrupt point fixture");
    let application = PointTopologyApplication::new(
        pool.clone(),
        Arc::new(aether_store_local::MemoryAuditSink::new()),
    );
    let result = application
        .mutate(
            &context(),
            Some(ChannelRevision::new(1)),
            PointTopologyMutation::Mappings {
                channel_id: 7,
                merge: true,
                mappings: vec![PointMappingMutation {
                    kind: PointKind::Telemetry,
                    point_id: 1,
                    protocol_data: serde_json::json!({
                        "slave_id": 1,
                        "function_code": 3,
                        "register_address": 17
                    }),
                }],
            },
        )
        .await;
    assert!(
        matches!(result, Err(ApplicationError::Port(ref error)) if error.kind() == PortErrorKind::InvalidData)
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT protocol_mappings FROM telemetry_points WHERE channel_id = 7 AND point_id = 1"
        )
        .fetch_one(&pool)
        .await
        .expect("mapping retained"),
        "{broken"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT revision FROM channels WHERE channel_id = 7")
            .fetch_one(&pool)
            .await
            .expect("revision rolled back"),
        1
    );
}

#[tokio::test]
async fn merge_protocol_validation_failure_rolls_back_mapping_and_revision() {
    let pool = configured_pool().await;
    let original = serde_json::json!({
        "slave_id": 1,
        "function_code": 3,
        "register_address": 10,
        "data_type": "float32",
        "byte_order": "ABCD"
    })
    .to_string();
    sqlx::query(
        "INSERT INTO telemetry_points \
         (channel_id, point_id, signal_name, scale, offset, unit, data_type, reverse, description, protocol_mappings) \
         VALUES (7, 1, 'temperature', 1, 0, 'C', 'f64', 0, '', ?)",
    )
    .bind(&original)
    .execute(&pool)
    .await
    .expect("mapped point fixture");
    let application = PointTopologyApplication::new(
        pool.clone(),
        Arc::new(aether_store_local::MemoryAuditSink::new()),
    );
    let result = application
        .mutate(
            &context(),
            Some(ChannelRevision::new(1)),
            PointTopologyMutation::Mappings {
                channel_id: 7,
                merge: true,
                mappings: vec![PointMappingMutation {
                    kind: PointKind::Telemetry,
                    point_id: 1,
                    protocol_data: serde_json::json!({"function_code": 99}),
                }],
            },
        )
        .await;
    assert!(
        matches!(result, Err(ApplicationError::Port(ref error)) if error.kind() == PortErrorKind::InvalidData)
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT protocol_mappings FROM telemetry_points WHERE channel_id = 7 AND point_id = 1"
        )
        .fetch_one(&pool)
        .await
        .expect("mapping retained"),
        original
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT revision FROM channels WHERE channel_id = 7")
            .fetch_one(&pool)
            .await
            .expect("revision rolled back"),
        1
    );
}
