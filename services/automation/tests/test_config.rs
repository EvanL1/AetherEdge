//! AutomationConfig Unit Tests
//!
//! Tests for configuration parsing, validation, and default values.
//! These are pure unit tests with no external service requirements.

#![allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable

use aether_automation::config::AutomationConfig;
use serde_json::json;

// ============================================================================
// Default Value Tests
// ============================================================================

#[test]
fn test_automation_config_default() {
    let config = AutomationConfig::default();

    // Check default service name
    assert_eq!(config.service.name, "aether-automation");

    // Check default API port
    assert_eq!(config.api.port, 6002);
    assert_eq!(config.api.host, "127.0.0.1");

    // Check default paths
    assert_eq!(
        config.products_path,
        Some("config/automation/products".to_string())
    );
    assert_eq!(
        config.instances_path,
        Some("config/automation/instances.yaml".to_string())
    );

    // Check auto_load_instances defaults to true
    assert!(config.auto_load_instances);
}

#[test]
fn test_automation_config_is_local_only() {
    let AutomationConfig {
        service: _,
        api: _,
        products_path: _,
        instances_path: _,
        auto_load_instances: _,
    } = AutomationConfig::default();
}

// ============================================================================
// JSON/YAML Deserialization Tests
// ============================================================================

#[test]
fn test_config_from_minimal_json() {
    // Note: BaseServiceConfig uses "unnamed_service" as default name
    // The "service" field is flattened, so we set name directly
    let json_str = r#"{
        "name": "test-automation"
    }"#;

    let config: AutomationConfig = serde_json::from_str(json_str).expect("Failed to parse JSON");

    assert_eq!(config.service.name, "test-automation");
    // API should use default values
    assert_eq!(config.api.port, 6002);
    assert_eq!(config.api.host, "127.0.0.1");
}

#[test]
fn test_config_with_custom_api_port() {
    let json_str = r#"{
        "service": {
            "name": "automation-custom"
        },
        "api": {
            "host": "127.0.0.1",
            "port": 7002
        }
    }"#;

    let config: AutomationConfig = serde_json::from_str(json_str).expect("Failed to parse JSON");

    assert_eq!(config.api.port, 7002);
    assert_eq!(config.api.host, "127.0.0.1");
}

#[test]
fn test_config_with_custom_paths() {
    let json_str = r#"{
        "service": {
            "name": "aether-automation"
        },
        "products_path": "/custom/products",
        "instances_path": "/custom/instances.yaml"
    }"#;

    let config: AutomationConfig = serde_json::from_str(json_str).expect("Failed to parse JSON");

    assert_eq!(config.products_path, Some("/custom/products".to_string()));
    assert_eq!(
        config.instances_path,
        Some("/custom/instances.yaml".to_string())
    );
}

#[test]
fn test_config_with_null_paths() {
    let json_str = r#"{
        "service": {
            "name": "aether-automation"
        },
        "products_path": null,
        "instances_path": null
    }"#;

    let config: AutomationConfig = serde_json::from_str(json_str).expect("Failed to parse JSON");

    assert!(config.products_path.is_none());
    assert!(config.instances_path.is_none());
}

#[test]
fn test_config_disable_auto_load_instances() {
    let json_str = r#"{
        "service": {
            "name": "aether-automation"
        },
        "auto_load_instances": false
    }"#;

    let config: AutomationConfig = serde_json::from_str(json_str).expect("Failed to parse JSON");

    assert!(!config.auto_load_instances);
}

// ============================================================================
// Configuration Serialization Tests
// ============================================================================

#[test]
fn test_config_round_trip() {
    let original = AutomationConfig::default();

    // Serialize to JSON
    let json_str = serde_json::to_string(&original).expect("Failed to serialize");

    // Deserialize back
    let parsed: AutomationConfig = serde_json::from_str(&json_str).expect("Failed to parse");

    // Compare key fields
    assert_eq!(original.service.name, parsed.service.name);
    assert_eq!(original.api.port, parsed.api.port);
    assert_eq!(original.auto_load_instances, parsed.auto_load_instances);
}

// ============================================================================
// Product Type Tests
// ============================================================================

#[test]
fn test_product_deserialization() {
    use aether_automation::config::Product;

    let json_str = r#"{
        "product_name": "BatteryPack",
        "parent_name": null,
        "measurements": [
            {
                "measurement_id": 1,
                "name": "Voltage",
                "unit": "V",
                "description": "Battery voltage"
            },
            {
                "measurement_id": 2,
                "name": "Current",
                "unit": "A"
            }
        ],
        "actions": [
            {
                "action_id": 1,
                "name": "Start",
                "description": "Start charging"
            }
        ],
        "properties": []
    }"#;

    let product: Product = serde_json::from_str(json_str).expect("Failed to parse product");

    assert_eq!(product.product_name, "BatteryPack");
    assert!(product.parent_name.is_none());
    assert_eq!(product.measurements.len(), 2);
    assert_eq!(product.measurements[0].measurement_id, 1);
    assert_eq!(product.measurements[0].name, "Voltage");
    assert_eq!(product.measurements[0].unit, Some("V".to_string()));
    assert_eq!(product.actions.len(), 1);
    assert_eq!(product.actions[0].action_id, 1);
}

#[test]
fn test_product_with_parent() {
    use aether_automation::config::Product;

    let json_str = r#"{
        "product_name": "BatteryModule",
        "parent_name": "BatteryPack",
        "measurements": [],
        "actions": [],
        "properties": []
    }"#;

    let product: Product = serde_json::from_str(json_str).expect("Failed to parse product");

    assert_eq!(product.parent_name, Some("BatteryPack".to_string()));
}

#[test]
fn test_measurement_point_aliases() {
    use aether_automation::config::MeasurementPoint;

    // Test "id" alias
    let json_id = r#"{"id": 1, "name": "Test"}"#;
    let point: MeasurementPoint = serde_json::from_str(json_id).expect("Failed with id alias");
    assert_eq!(point.measurement_id, 1);

    // Test "index" alias
    let json_index = r#"{"index": 2, "name": "Test2"}"#;
    let point: MeasurementPoint =
        serde_json::from_str(json_index).expect("Failed with index alias");
    assert_eq!(point.measurement_id, 2);

    // Test canonical name
    let json_canonical = r#"{"measurement_id": 3, "name": "Test3"}"#;
    let point: MeasurementPoint =
        serde_json::from_str(json_canonical).expect("Failed with canonical name");
    assert_eq!(point.measurement_id, 3);
}

#[test]
fn test_action_point_aliases() {
    use aether_automation::config::ActionPoint;

    // Test "id" alias
    let json_id = r#"{"id": 1, "name": "Start"}"#;
    let point: ActionPoint = serde_json::from_str(json_id).expect("Failed with id alias");
    assert_eq!(point.action_id, 1);

    // Test "index" alias
    let json_index = r#"{"index": 2, "name": "Stop"}"#;
    let point: ActionPoint = serde_json::from_str(json_index).expect("Failed with index alias");
    assert_eq!(point.action_id, 2);
}

// ============================================================================
// Instance Type Tests
// ============================================================================

#[test]
fn test_instance_core_deserialization() {
    use aether_automation::config::InstanceCore;

    let json_str = r#"{
        "instance_id": 1001,
        "instance_name": "battery_pack_1",
        "product_name": "BatteryPack",
        "properties": {
            "capacity": 100,
            "voltage_rating": 48.0,
            "serial_number": "BP-2024-001"
        }
    }"#;

    let instance: InstanceCore = serde_json::from_str(json_str).expect("Failed to parse instance");

    assert_eq!(instance.instance_id, 1001);
    assert_eq!(instance.instance_name, "battery_pack_1");
    assert_eq!(instance.product_name, "BatteryPack");
    assert_eq!(instance.properties.len(), 3);
    assert_eq!(instance.properties["capacity"], json!(100));
    assert_eq!(instance.properties["voltage_rating"], json!(48.0));
}

#[test]
fn test_instance_core_empty_properties() {
    use aether_automation::config::InstanceCore;

    let json_str = r#"{
        "instance_id": 2001,
        "instance_name": "test_instance",
        "product_name": "TestProduct"
    }"#;

    let instance: InstanceCore = serde_json::from_str(json_str).expect("Failed to parse instance");

    assert_eq!(instance.instance_id, 2001);
    assert!(instance.properties.is_empty());
}

// ============================================================================
// Schema SQL Generation Tests
// ============================================================================

#[test]
fn test_instances_table_sql_not_empty() {
    let sql = aether_automation::config::INSTANCES_TABLE;
    assert!(!sql.is_empty());
    assert!(sql.contains("CREATE TABLE"));
    assert!(sql.contains("instances"));
}

#[test]
fn test_measurement_routing_table_sql_not_empty() {
    let sql = aether_automation::config::MEASUREMENT_ROUTING_TABLE;
    assert!(!sql.is_empty());
    assert!(sql.contains("CREATE TABLE"));
    assert!(sql.contains("measurement_routing"));
}

#[test]
fn test_action_routing_table_sql_not_empty() {
    let sql = aether_automation::config::ACTION_ROUTING_TABLE;
    assert!(!sql.is_empty());
    assert!(sql.contains("CREATE TABLE"));
    assert!(sql.contains("action_routing"));
}

// ============================================================================
// Edge Cases and Error Handling
// ============================================================================

#[test]
fn test_config_invalid_json_returns_error() {
    let invalid_json = r#"{ this is not valid json }"#;
    let result: Result<AutomationConfig, _> = serde_json::from_str(invalid_json);
    assert!(result.is_err());
}

#[test]
fn test_config_missing_service_name_uses_default() {
    // Empty object should still work with defaults
    let json_str = r#"{}"#;
    let config: AutomationConfig =
        serde_json::from_str(json_str).expect("Failed to parse empty JSON");

    // BaseServiceConfig defaults to "unnamed_service" when name is not provided
    assert_eq!(
        config.service.name, "unnamed_service",
        "Expected 'unnamed_service' as default service name from BaseServiceConfig"
    );
}

#[test]
fn test_product_with_empty_arrays() {
    use aether_automation::config::Product;

    let json_str = r#"{
        "product_name": "EmptyProduct",
        "measurements": [],
        "actions": [],
        "properties": []
    }"#;

    let product: Product = serde_json::from_str(json_str).expect("Failed to parse product");

    assert_eq!(product.product_name, "EmptyProduct");
    assert!(product.measurements.is_empty());
    assert!(product.actions.is_empty());
    assert!(product.properties.is_empty());
}
