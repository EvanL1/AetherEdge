//! Integration tests for aether-schema-macro
//!
//! These tests verify the macro works correctly when used from external code.

use aether_schema_macro::Schema;

#[test]
fn test_simple_channel_schema() {
    #[allow(dead_code)]
    #[derive(Schema)]
    #[table(name = "channels")]
    struct Channel {
        #[column(primary_key)]
        channel_id: u16,

        #[column(unique, not_null)]
        name: String,

        #[column(default = "modbus_tcp")]
        protocol: String,

        #[column(default = "true")]
        enabled: bool,
    }

    // Verify constants are generated
    assert!(Channel::CREATE_TABLE_SQL.contains("CREATE TABLE"));
    assert!(Channel::CREATE_TABLE_SQL.contains("channels"));
    assert_eq!(Channel::TABLE_NAME, "channels");
    assert_eq!(
        Channel::COLUMNS,
        &["channel_id", "name", "protocol", "enabled"]
    );

    // Verify constraints
    assert!(Channel::CREATE_TABLE_SQL.contains("PRIMARY KEY"));
    assert!(Channel::CREATE_TABLE_SQL.contains("UNIQUE"));
    assert!(Channel::CREATE_TABLE_SQL.contains("NOT NULL"));
    assert!(Channel::CREATE_TABLE_SQL.contains("DEFAULT 'modbus_tcp'"));
    assert!(Channel::CREATE_TABLE_SQL.contains("DEFAULT TRUE"));

    println!("Generated SQL:\n{}", Channel::CREATE_TABLE_SQL);
}

#[test]
fn test_measurement_point_schema() {
    #[allow(dead_code)]
    #[derive(Schema)]
    #[table(name = "telemetry_points", if_not_exists = true)]
    struct TelemetryPoint {
        #[column(primary_key)]
        point_id: u32,

        #[column(not_null)]
        signal_name: String,

        #[column(default = "1.0")]
        scale: f64,

        #[column(default = "0.0")]
        offset: f64,

        unit: Option<String>,

        #[column(default = "false")]
        reverse: bool,
    }

    assert!(TelemetryPoint::CREATE_TABLE_SQL.contains("IF NOT EXISTS"));
    assert!(TelemetryPoint::CREATE_TABLE_SQL.contains("telemetry_points"));
    assert_eq!(TelemetryPoint::TABLE_NAME, "telemetry_points");

    // Check column count
    assert_eq!(TelemetryPoint::COLUMNS.len(), 6);
    assert!(TelemetryPoint::COLUMNS.contains(&"point_id"));
    assert!(TelemetryPoint::COLUMNS.contains(&"signal_name"));
    assert!(TelemetryPoint::COLUMNS.contains(&"scale"));

    println!("Generated SQL:\n{}", TelemetryPoint::CREATE_TABLE_SQL);
}

#[test]
fn test_foreign_key_schema() {
    #[allow(dead_code)]
    #[derive(Schema)]
    #[table(name = "measurement_routing")]
    struct MeasurementRouting {
        #[column(primary_key, autoincrement)]
        routing_id: i64,

        #[column(not_null, references = "instances(instance_id)", on_delete = "CASCADE")]
        instance_id: u16,

        #[column(not_null, references = "channels(channel_id)", on_delete = "CASCADE")]
        channel_id: u16,
    }

    assert!(MeasurementRouting::CREATE_TABLE_SQL.contains("REFERENCES instances"));
    assert!(MeasurementRouting::CREATE_TABLE_SQL.contains("REFERENCES channels"));
    assert!(MeasurementRouting::CREATE_TABLE_SQL.contains("ON DELETE CASCADE"));
    assert!(MeasurementRouting::CREATE_TABLE_SQL.contains("AUTOINCREMENT"));

    println!("Generated SQL:\n{}", MeasurementRouting::CREATE_TABLE_SQL);
}

#[test]
fn test_skip_field() {
    #[allow(dead_code)]
    #[derive(Schema)]
    struct Config {
        #[column(primary_key)]
        id: u16,

        name: String,

        #[column(skip)]
        runtime_data: String,
    }

    // runtime_data should not be in SQL or COLUMNS
    assert!(!Config::CREATE_TABLE_SQL.contains("runtime_data"));
    assert!(!Config::COLUMNS.contains(&"runtime_data"));
    assert_eq!(Config::COLUMNS.len(), 2);

    println!("Generated SQL:\n{}", Config::CREATE_TABLE_SQL);
}

#[test]
fn test_optional_fields() {
    #[allow(dead_code)]
    #[derive(Schema)]
    struct Device {
        #[column(primary_key)]
        device_id: u16,

        name: String,

        description: Option<String>,

        ip_address: Option<String>,
    }

    // Optional fields should not have NOT NULL
    let sql = Device::CREATE_TABLE_SQL;
    assert!(sql.contains("device_id INTEGER PRIMARY KEY"));
    assert!(sql.contains("name TEXT NOT NULL"));

    // description and ip_address should be TEXT without NOT NULL
    assert!(sql.contains("description TEXT"));
    assert!(sql.contains("ip_address TEXT"));

    // But description line should not contain "NOT NULL"
    for line in sql.lines() {
        if line.contains("description") {
            assert!(
                !line.contains("NOT NULL"),
                "Optional field should not have NOT NULL"
            );
        }
        if line.contains("ip_address") {
            assert!(
                !line.contains("NOT NULL"),
                "Optional field should not have NOT NULL"
            );
        }
    }

    println!("Generated SQL:\n{}", Device::CREATE_TABLE_SQL);
}

#[test]
fn test_type_mapping() {
    #[allow(dead_code)]
    #[derive(Schema)]
    struct TypeTest {
        int_field: i32,
        float_field: f64,
        string_field: String,
        bool_field: bool,
    }

    let sql = TypeTest::CREATE_TABLE_SQL;
    assert!(sql.contains("int_field INTEGER"));
    assert!(sql.contains("float_field REAL"));
    assert!(sql.contains("string_field TEXT"));
    assert!(sql.contains("bool_field BOOLEAN"));

    println!("Generated SQL:\n{}", TypeTest::CREATE_TABLE_SQL);
}

#[test]
fn test_flatten_field() {
    // Define a "core" struct that would be flattened
    #[allow(dead_code)]
    struct DeviceCore {
        device_id: u16,
        name: String,
        enabled: bool,
    }

    #[allow(dead_code)]
    #[derive(Schema)]
    #[table(name = "devices")]
    struct Device {
        // Manually declare core fields (flatten not yet supported for nested structs)
        #[column(primary_key)]
        device_id: u16,

        #[column(not_null)]
        name: String,

        #[column(default = "true")]
        enabled: bool,

        // Flatten field would be skipped if marked with #[column(flatten)]
        // #[column(flatten)]
        // core: DeviceCore,

        // Additional fields
        ip_address: Option<String>,
    }

    assert_eq!(Device::TABLE_NAME, "devices");
    assert_eq!(Device::COLUMNS.len(), 4);
    assert!(Device::COLUMNS.contains(&"device_id"));
    assert!(Device::COLUMNS.contains(&"name"));
    assert!(Device::COLUMNS.contains(&"enabled"));
    assert!(Device::COLUMNS.contains(&"ip_address"));

    println!("Generated SQL:\n{}", Device::CREATE_TABLE_SQL);
}

#[test]
fn test_flatten_field_is_skipped() {
    // This test demonstrates that flatten fields are automatically skipped
    #[allow(dead_code)]
    struct CoreData {
        id: u16,
        name: String,
    }

    #[allow(dead_code)]
    #[derive(Schema)]
    struct ConfigWithFlatten {
        #[column(primary_key)]
        config_id: u16,

        #[column(flatten)]
        core: CoreData,

        setting: String,
    }

    // The flatten field "core" should not appear in COLUMNS or SQL
    assert!(!ConfigWithFlatten::COLUMNS.contains(&"core"));
    assert!(!ConfigWithFlatten::CREATE_TABLE_SQL.contains("core"));

    // But other fields should be present
    assert!(ConfigWithFlatten::COLUMNS.contains(&"config_id"));
    assert!(ConfigWithFlatten::COLUMNS.contains(&"setting"));
    assert_eq!(ConfigWithFlatten::COLUMNS.len(), 2);

    println!("Generated SQL:\n{}", ConfigWithFlatten::CREATE_TABLE_SQL);
}
