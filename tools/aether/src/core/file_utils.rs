//! Utility functions for configuration loading and processing

use anyhow::{Context, Result};
use common::validation::{CsvFields, CsvHeaderValidator};
use csv::Reader;
use serde::de::DeserializeOwned;
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::path::Path;
use tracing::{debug, warn};

/// Error that occurred while parsing a specific CSV row
#[derive(Debug, Clone)]
pub struct CsvRowError {
    /// Row number (1-indexed, excluding header)
    pub row_number: usize,
    /// Error message
    pub error: String,
}

/// Type alias for CSV loading result with error recovery
pub type CsvResult<T> = Result<(Vec<T>, Vec<CsvRowError>)>;

/// Load CSV file and return as vector of hashmaps
pub fn load_csv<P: AsRef<Path>>(path: P) -> Result<Vec<HashMap<String, String>>> {
    let path = path.as_ref();
    debug!("Loading CSV file: {:?}", path);

    let file = std::fs::File::open(path)
        .with_context(|| format!("Failed to open CSV file: {:?}", path))?;

    let mut reader = Reader::from_reader(file);
    let headers = reader
        .headers()
        .with_context(|| format!("Failed to read CSV headers: {:?}", path))?
        .clone();

    let mut records = Vec::new();
    for result in reader.records() {
        let record = result.with_context(|| format!("Failed to read CSV record: {:?}", path))?;

        let mut row = HashMap::new();
        for (i, field) in record.iter().enumerate() {
            if let Some(header) = headers.get(i) {
                row.insert(header.to_string(), field.to_string());
            }
        }
        records.push(row);
    }

    debug!("Loaded {} records from CSV: {:?}", records.len(), path);
    Ok(records)
}

/// Load CSV file with error recovery - returns successful rows and errors separately
///
/// This version does not fail on individual row errors, instead collecting them
/// for reporting while continuing to process valid rows.
pub fn load_csv_with_errors<P: AsRef<Path>>(path: P) -> CsvResult<HashMap<String, String>> {
    let path = path.as_ref();
    debug!("Loading CSV file with error recovery: {:?}", path);

    let file = std::fs::File::open(path)
        .with_context(|| format!("Failed to open CSV file: {:?}", path))?;

    let mut reader = Reader::from_reader(file);
    let headers = reader
        .headers()
        .with_context(|| format!("Failed to read CSV headers: {:?}", path))?
        .clone();

    let mut records = Vec::new();
    let mut errors = Vec::new();

    for (row_number, result) in reader.records().enumerate() {
        let row_number = row_number + 1; // 1-indexed

        match result {
            Ok(record) => {
                let mut row = HashMap::new();
                for (i, field) in record.iter().enumerate() {
                    if let Some(header) = headers.get(i) {
                        row.insert(header.to_string(), field.to_string());
                    }
                }
                records.push(row);
            },
            Err(e) => {
                errors.push(CsvRowError {
                    row_number,
                    error: e.to_string(),
                });
            },
        }
    }

    debug!(
        "Loaded {} valid records and encountered {} errors from CSV: {:?}",
        records.len(),
        errors.len(),
        path
    );
    Ok((records, errors))
}

/// Load CSV file and deserialize with error recovery
///
/// This version does not fail on individual row deserialization errors,
/// instead collecting them for reporting while continuing to process valid rows.
///
/// Additionally validates CSV header against expected fields defined by CsvFields trait.
pub fn load_csv_typed_with_errors<T, P>(path: P) -> CsvResult<T>
where
    T: DeserializeOwned + CsvFields,
    P: AsRef<Path>,
{
    let path = path.as_ref();
    debug!(
        "Loading CSV file with typed deserialization and error recovery: {:?}",
        path
    );

    let mut errors = Vec::new();

    // Step 1: Validate CSV header before processing
    match CsvHeaderValidator::validate_csv_header::<T>(path) {
        Ok(validation_result) => {
            // Add validation errors as CSV row errors
            for error in validation_result.errors {
                errors.push(CsvRowError {
                    row_number: 0, // 0 indicates header error
                    error,
                });
            }

            // Log warnings but don't fail
            for warning in validation_result.warnings {
                tracing::warn!("CSV header warning: {}", warning);
            }

            // If header validation failed, we still try to parse (best effort)
            // but the errors are already recorded
        },
        Err(e) => {
            // If we can't even read the file for validation, add as error
            errors.push(CsvRowError {
                row_number: 0,
                error: format!("Header validation failed: {}", e),
            });
        },
    }

    // Step 2: Attempt to load and deserialize records
    let file = std::fs::File::open(path)
        .with_context(|| format!("Failed to open CSV file: {:?}", path))?;

    let mut reader = Reader::from_reader(file);
    let mut records = Vec::new();

    for (row_number, result) in reader.deserialize().enumerate() {
        let row_number = row_number + 1; // 1-indexed

        match result {
            Ok(record) => {
                records.push(record);
            },
            Err(e) => {
                errors.push(CsvRowError {
                    row_number,
                    error: e.to_string(),
                });
            },
        }
    }

    debug!(
        "Loaded and deserialized {} valid typed records and encountered {} errors from CSV: {:?}",
        records.len(),
        errors.len(),
        path
    );
    Ok((records, errors))
}

/// Flatten nested JSON object into key-value pairs
pub fn flatten_json(value: &JsonValue, prefix: Option<String>) -> HashMap<String, JsonValue> {
    let mut result = HashMap::new();

    match value {
        JsonValue::Object(map) => {
            for (key, val) in map {
                let new_key = match &prefix {
                    Some(p) => format!("{}.{}", p, key),
                    None => key.clone(),
                };

                match val {
                    JsonValue::Object(_) => {
                        // Recursively flatten nested objects
                        let nested = flatten_json(val, Some(new_key));
                        result.extend(nested);
                    },
                    _ => {
                        // Store leaf values directly
                        result.insert(new_key, val.clone());
                    },
                }
            }
        },
        _ => {
            // If not an object, store the value with the given prefix
            if let Some(p) = prefix {
                result.insert(p, value.clone());
            }
        },
    }

    result
}

/// Set database file permissions for Docker compatibility
/// Sets permissions to 664 (rw-rw-r--) to allow owner and group access
/// while preventing world write access for security
///
/// Note: This is a best-effort operation. If the current user is not the file owner,
/// the permission change will be skipped with a warning (not an error).
pub fn set_database_permissions<P: AsRef<Path>>(path: P) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = path.as_ref();
        if path.exists() {
            let mut perms = std::fs::metadata(path)?.permissions();
            // Set permissions to 664 (rw-rw-r--) - owner and group can read/write, others read-only
            perms.set_mode(0o664);
            if let Err(e) = std::fs::set_permissions(path, perms) {
                // Permission denied is common when not file owner - warn instead of fail
                warn!(
                    "Could not set permissions for {:?}: {} (this is usually safe to ignore)",
                    path, e
                );
            } else {
                debug!("Set permissions to 664 for {:?}", path);
            }
        }
    }
    Ok(())
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;
    use serde_json::json;

    // ========================================================================
    // flatten_json() Tests
    // ========================================================================

    #[test]
    fn test_flatten_json_simple_object() {
        let value = json!({
            "name": "test",
            "count": 42
        });

        let result = flatten_json(&value, None);

        assert_eq!(result.len(), 2);
        assert_eq!(result.get("name").unwrap(), &json!("test"));
        assert_eq!(result.get("count").unwrap(), &json!(42));
    }

    #[test]
    fn test_flatten_json_nested_object() {
        let value = json!({
            "service": {
                "name": "aether-io",
                "port": 6001
            }
        });

        let result = flatten_json(&value, None);

        assert_eq!(result.len(), 2);
        assert_eq!(result.get("service.name").unwrap(), &json!("aether-io"));
        assert_eq!(result.get("service.port").unwrap(), &json!(6001));
    }

    #[test]
    fn test_flatten_json_deeply_nested() {
        let value = json!({
            "level1": {
                "level2": {
                    "level3": {
                        "value": "deep"
                    }
                }
            }
        });

        let result = flatten_json(&value, None);

        assert_eq!(result.len(), 1);
        assert_eq!(
            result.get("level1.level2.level3.value").unwrap(),
            &json!("deep")
        );
    }

    #[test]
    fn test_flatten_json_with_prefix() {
        let value = json!({
            "host": "localhost",
            "port": 8080
        });

        let result = flatten_json(&value, Some("api".to_string()));

        assert_eq!(result.len(), 2);
        assert_eq!(result.get("api.host").unwrap(), &json!("localhost"));
        assert_eq!(result.get("api.port").unwrap(), &json!(8080));
    }

    #[test]
    fn test_flatten_json_mixed_types() {
        let value = json!({
            "string": "hello",
            "number": 123,
            "float": 1.234,
            "boolean": true,
            "null": null,
            "array": [1, 2, 3]
        });

        let result = flatten_json(&value, None);

        assert_eq!(result.len(), 6);
        assert_eq!(result.get("string").unwrap(), &json!("hello"));
        assert_eq!(result.get("number").unwrap(), &json!(123));
        assert_eq!(result.get("float").unwrap(), &json!(1.234));
        assert_eq!(result.get("boolean").unwrap(), &json!(true));
        assert_eq!(result.get("null").unwrap(), &json!(null));
        assert_eq!(result.get("array").unwrap(), &json!([1, 2, 3]));
    }

    #[test]
    fn test_flatten_json_empty_object() {
        let value = json!({});

        let result = flatten_json(&value, None);

        assert!(result.is_empty());
    }

    #[test]
    fn test_flatten_json_non_object_root() {
        // Test with non-object values at root
        let string_val = json!("hello");
        let result = flatten_json(&string_val, Some("key".to_string()));
        assert_eq!(result.len(), 1);
        assert_eq!(result.get("key").unwrap(), &json!("hello"));

        let number_val = json!(42);
        let result = flatten_json(&number_val, Some("num".to_string()));
        assert_eq!(result.len(), 1);
        assert_eq!(result.get("num").unwrap(), &json!(42));
    }

    #[test]
    fn test_flatten_json_non_object_root_no_prefix() {
        // Non-object without prefix should return empty
        let string_val = json!("hello");
        let result = flatten_json(&string_val, None);
        assert!(result.is_empty());
    }

    #[test]
    fn test_flatten_json_complex_config() {
        // Simulate real config structure
        let value = json!({
            "service": {
                "name": "aether-io",
                "description": "Communication Service"
            },
            "api": {
                "host": "0.0.0.0",
                "port": 6001
            },
            "shared_memory": {
                "path": "/dev/shm/aether-rtdb.shm"
            },
            "logging": {
                "level": "info",
                "format": "json"
            }
        });

        let result = flatten_json(&value, None);

        assert_eq!(result.len(), 7);
        assert_eq!(result.get("service.name").unwrap(), &json!("aether-io"));
        assert_eq!(result.get("api.host").unwrap(), &json!("0.0.0.0"));
        assert_eq!(result.get("api.port").unwrap(), &json!(6001));
        assert_eq!(
            result.get("shared_memory.path").unwrap(),
            &json!("/dev/shm/aether-rtdb.shm")
        );
        assert_eq!(result.get("logging.level").unwrap(), &json!("info"));
    }

    #[test]
    fn test_flatten_json_special_keys() {
        let value = json!({
            "key.with.dots": "value1",
            "key-with-dashes": "value2",
            "key_with_underscores": "value3"
        });

        let result = flatten_json(&value, None);

        // Note: keys with dots create ambiguity but should still work
        assert_eq!(result.len(), 3);
        assert_eq!(result.get("key.with.dots").unwrap(), &json!("value1"));
        assert_eq!(result.get("key-with-dashes").unwrap(), &json!("value2"));
        assert_eq!(
            result.get("key_with_underscores").unwrap(),
            &json!("value3")
        );
    }

    // ========================================================================
    // CsvRowError Tests
    // ========================================================================

    #[test]
    fn test_csv_row_error_struct() {
        let error = CsvRowError {
            row_number: 5,
            error: "Missing required field".to_string(),
        };

        assert_eq!(error.row_number, 5);
        assert_eq!(error.error, "Missing required field");
    }

    #[test]
    fn test_csv_row_error_header_row() {
        // Row 0 indicates header error
        let error = CsvRowError {
            row_number: 0,
            error: "Invalid header".to_string(),
        };

        assert_eq!(error.row_number, 0);
    }
}
