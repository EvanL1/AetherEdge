//! SQL code generation
//!
//! This module generates CREATE TABLE statements from parsed struct definitions.

use crate::{attributes::*, types::*};
use heck::ToSnakeCase;

/// Generate CREATE TABLE SQL statement
///
/// # Arguments
///
/// * `table_args` - Parsed table-level attributes and struct data
///
/// # Returns
///
/// A formatted CREATE TABLE statement string
///
/// # Example Output
///
/// ```sql
/// CREATE TABLE IF NOT EXISTS channels (
///     channel_id INTEGER PRIMARY KEY,
///     name TEXT NOT NULL UNIQUE,
///     protocol TEXT DEFAULT 'modbus_tcp',
///     enabled BOOLEAN DEFAULT TRUE
/// )
/// ```
#[allow(clippy::panic_in_result_fn)]
pub fn generate_create_table(table_args: &TableArgs) -> String {
    // Determine table name
    let table_name = table_args
        .name
        .clone()
        .unwrap_or_else(|| table_args.ident.to_string())
        .to_snake_case();

    // Extract struct fields - proc-macro panics are compile-time errors
    let fields = table_args
        .data
        .as_ref()
        .take_struct()
        .unwrap_or_else(|| panic!("Schema only supports named structs"))
        .fields;

    let mut columns = Vec::new();
    let mut primary_keys = Vec::new();

    // First pass: collect primary keys
    for field in &fields {
        if field.skip || field.flatten {
            continue;
        }
        if field.primary_key {
            let column_name = get_column_name(field);
            primary_keys.push(column_name);
        }
    }

    let has_composite_key = primary_keys.len() > 1;

    // Second pass: generate column definitions
    for field in fields {
        // Skip fields marked with skip or flatten
        // Note: flatten fields are skipped because we don't have access to
        // nested struct definitions at compile time. Users should manually
        // declare fields that need to be in the table.
        if field.skip || field.flatten {
            continue;
        }

        let column_def = generate_column_definition(field, has_composite_key);
        columns.push(column_def);
    }

    // Build CREATE TABLE statement
    let if_not_exists = if table_args.if_not_exists {
        "IF NOT EXISTS "
    } else {
        ""
    };

    let mut sql = format!(
        "CREATE TABLE {}{} (\n    {}",
        if_not_exists,
        table_name,
        columns.join(",\n    ")
    );

    // Add composite primary key constraint if needed
    if primary_keys.len() > 1 {
        sql.push_str(&format!(",\n    PRIMARY KEY ({})", primary_keys.join(", ")));
    }

    // Add custom suffix (table constraints) if specified
    // Suffix should be added inside the parentheses as a table constraint
    if let Some(suffix) = &table_args.suffix {
        sql.push_str(",\n    ");
        sql.push_str(suffix);
    }

    sql.push_str("\n)");

    sql
}

/// Generate SQL column definition
///
/// # Arguments
///
/// * `field` - Field attributes
/// * `has_composite_key` - True if table has a composite primary key
///
/// # Examples
///
/// - `channel_id INTEGER PRIMARY KEY` (single key)
/// - `service_name TEXT NOT NULL` (composite key - no PRIMARY KEY in column)
/// - `name TEXT NOT NULL UNIQUE`
/// - `enabled BOOLEAN DEFAULT TRUE`
/// - `instance_id INTEGER NOT NULL REFERENCES instances(instance_id) ON DELETE CASCADE`
fn generate_column_definition(field: &FieldArgs, has_composite_key: bool) -> String {
    let column_name = get_column_name(field);
    let (sql_type, is_optional) = handle_optional_type(&field.ty);

    let mut parts = vec![column_name, sql_type.clone()];

    // PRIMARY KEY (only add to column if NOT a composite key)
    if field.primary_key && !has_composite_key {
        parts.push("PRIMARY KEY".to_string());

        if field.autoincrement {
            parts.push("AUTOINCREMENT".to_string());
        }
    }

    // NOT NULL
    // Rules:
    // - Explicit #[column(not_null)] always adds NOT NULL
    // - Non-optional fields (not Option<T>) are NOT NULL unless primary key
    // - Optional fields (Option<T>) are nullable by default
    if field.not_null || (!is_optional && !field.primary_key) {
        parts.push("NOT NULL".to_string());
    }

    // UNIQUE
    if field.unique {
        parts.push("UNIQUE".to_string());
    }

    // DEFAULT
    if let Some(default_val) = &field.default {
        let formatted = format_default_value(default_val, &sql_type);
        parts.push(format!("DEFAULT {}", formatted));
    }

    // REFERENCES (foreign key)
    if let Some(ref_table) = &field.references {
        parts.push(format!("REFERENCES {}", ref_table));

        if let Some(on_delete) = &field.on_delete {
            parts.push(format!("ON DELETE {}", on_delete));
        }

        if let Some(on_update) = &field.on_update {
            parts.push(format!("ON UPDATE {}", on_update));
        }
    }

    parts.join(" ")
}

/// Get column name from field
///
/// Uses explicit #[column(name = "...")] if provided, otherwise field name
#[allow(clippy::panic_in_result_fn)]
fn get_column_name(field: &FieldArgs) -> String {
    let name = field
        .name
        .clone()
        .or_else(|| field.ident.as_ref().map(|i| i.to_string()))
        .unwrap_or_else(|| panic!("Field must have a name"));

    // Remove r# prefix for raw identifiers
    name.strip_prefix("r#").unwrap_or(&name).to_string()
}

/// Format default value for SQL
///
/// # Rules
///
/// - SQL functions (CURRENT_TIMESTAMP, etc.) → no quotes
/// - Boolean values → TRUE/FALSE
/// - Numbers → no quotes
/// - Strings → single quotes, escape internal quotes
///
/// # Examples
///
/// - `"CURRENT_TIMESTAMP"` → `CURRENT_TIMESTAMP`
/// - `"true"` for BOOLEAN → `TRUE`
/// - `"42"` for INTEGER → `42`
/// - `"hello"` for TEXT → `'hello'`
/// - `"it's"` for TEXT → `'it''s'` (escaped quote)
fn format_default_value(value: &str, sql_type: &str) -> String {
    match (value, sql_type) {
        // SQL functions (no quotes)
        ("CURRENT_TIMESTAMP", _) | ("CURRENT_DATE", _) | ("CURRENT_TIME", _) => value.to_string(),

        // Boolean values
        ("true", "BOOLEAN") => "TRUE".to_string(),
        ("false", "BOOLEAN") => "FALSE".to_string(),

        // Numeric values (no quotes if parseable as number)
        (v, "INTEGER") | (v, "REAL") if v.parse::<f64>().is_ok() => v.to_string(),

        // String values (single quotes, escape internal quotes)
        (v, "TEXT") => {
            // Escape single quotes by doubling them
            let escaped = v.replace('\'', "''");
            format!("'{}'", escaped)
        },

        // Default: pass through as-is
        _ => value.to_string(),
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;

    #[test]
    fn test_format_default_value() {
        // SQL functions
        assert_eq!(
            format_default_value("CURRENT_TIMESTAMP", "TIMESTAMP"),
            "CURRENT_TIMESTAMP"
        );
        assert_eq!(format_default_value("CURRENT_DATE", "DATE"), "CURRENT_DATE");

        // Boolean values
        assert_eq!(format_default_value("true", "BOOLEAN"), "TRUE");
        assert_eq!(format_default_value("false", "BOOLEAN"), "FALSE");

        // Numeric values
        assert_eq!(format_default_value("42", "INTEGER"), "42");
        assert_eq!(format_default_value("3.14", "REAL"), "3.14");

        // String values
        assert_eq!(format_default_value("hello", "TEXT"), "'hello'");
        assert_eq!(format_default_value("it's great", "TEXT"), "'it''s great'");
        assert_eq!(format_default_value("modbus_tcp", "TEXT"), "'modbus_tcp'");
    }

    #[test]
    fn test_get_column_name() {
        use darling::FromField;
        use syn::{Field, parse_quote};

        // Field with explicit name
        let field: Field = parse_quote! {
            #[column(name = "custom_name")]
            pub my_field: String
        };
        let args = FieldArgs::from_field(&field).unwrap();
        assert_eq!(get_column_name(&args), "custom_name");

        // Field without explicit name (uses field name)
        let field: Field = parse_quote! {
            pub my_field: String
        };
        let args = FieldArgs::from_field(&field).unwrap();
        assert_eq!(get_column_name(&args), "my_field");
    }
}
