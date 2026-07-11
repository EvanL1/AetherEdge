//! CSV and configuration validation utilities
//!
//! Provides automatic CSV header validation by comparing actual CSV files
//! against expected field names from Rust struct definitions.

use std::collections::HashSet;
use std::path::Path;

use crate::service_config::{ValidationLevel, ValidationResult};

/// Trait for types that can be deserialized from CSV files
///
/// Implementing this trait provides automatic CSV header validation
/// by defining the expected field names for CSV deserialization.
pub trait CsvFields {
    /// Returns the expected CSV header field names in order
    fn field_names() -> Vec<String>;

    /// Returns the required fields (cannot be empty/null)
    fn required_fields() -> Vec<String> {
        // By default, all fields are required
        Self::field_names()
    }

    /// Returns optional fields (can be empty/null)
    fn optional_fields() -> Vec<String> {
        vec![]
    }
}

/// CSV Header Validator
pub struct CsvHeaderValidator;

impl CsvHeaderValidator {
    /// Validate CSV file header against expected fields
    ///
    /// # Arguments
    /// * `csv_path` - Path to the CSV file to validate
    ///
    /// # Returns
    /// * `Ok(ValidationResult)` with validation status and detailed errors/warnings
    pub fn validate_csv_header<T>(csv_path: &Path) -> anyhow::Result<ValidationResult>
    where
        T: CsvFields,
    {
        // Read CSV file
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(true)
            .from_path(csv_path)?;

        // Get actual headers from CSV
        let headers = reader.headers()?;
        let actual_headers: Vec<String> = headers.iter().map(|s| s.to_string()).collect();

        // Get expected headers from trait
        let expected_headers = T::field_names();

        // Validate
        Self::validate_headers(&actual_headers, &expected_headers, csv_path)
    }

    /// Validate headers with detailed error reporting
    fn validate_headers(
        actual: &[String],
        expected: &[String],
        csv_path: &Path,
    ) -> anyhow::Result<ValidationResult> {
        let actual_set: HashSet<_> = actual.iter().collect();
        let expected_set: HashSet<_> = expected.iter().collect();

        let mut errors = Vec::new();
        let mut warnings = Vec::new();

        // Check for missing required fields
        let missing: Vec<_> = expected_set.difference(&actual_set).collect();
        if !missing.is_empty() {
            // Use clone() instead of to_string() to avoid extra deref chain
            let missing_str = missing
                .iter()
                .map(|s| (**s).clone())
                .collect::<Vec<_>>()
                .join(", ");
            errors.push(format!(
                "Missing required fields in {}: [{}]",
                csv_path.display(),
                missing_str
            ));
        }

        // Check for extra fields (warnings only)
        let extra: Vec<_> = actual_set.difference(&expected_set).collect();
        if !extra.is_empty() {
            // Use clone() instead of to_string() to avoid extra deref chain
            let extra_str = extra
                .iter()
                .map(|s| (**s).clone())
                .collect::<Vec<_>>()
                .join(", ");
            warnings.push(format!(
                "Extra fields found in {} (will be ignored): [{}]",
                csv_path.display(),
                extra_str
            ));
        }

        // Check field order (warning only - CSV allows any order)
        if actual != expected && missing.is_empty() && extra.is_empty() {
            warnings.push(format!(
                "Field order in {} differs from expected (this is OK, just informational)",
                csv_path.display()
            ));
        }

        Ok(ValidationResult {
            is_valid: errors.is_empty(),
            level: ValidationLevel::Schema,
            errors,
            warnings,
        })
    }
}
