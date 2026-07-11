//! Rust type to SQLite type mapping
//!
//! This module provides automatic type mapping from Rust types to SQLite types,
//! handling both basic types and complex cases like `Option<T>`.

use crate::utils::*;
use syn::Type;

/// Map Rust type to SQLite type
///
/// # Mapping Rules
///
/// | Rust Type | SQLite Type |
/// |-----------|-------------|
/// | u8, u16, u32, i8, i16, i32, i64, usize, isize | INTEGER |
/// | f32, f64 | REAL |
/// | String, &str | TEXT |
/// | bool | BOOLEAN |
/// | DateTime, NaiveDateTime | TIMESTAMP |
/// | Date, NaiveDate | DATE |
/// | serde_json::Value, HashMap, BTreeMap | TEXT (stored as JSON) |
/// | `Vec<u8>` | BLOB |
/// | Other | TEXT (default) |
///
/// # Examples
///
/// ```ignore
/// let ty: Type = parse_quote!(u16);
/// assert_eq!(rust_to_sqlite_type(&ty), "INTEGER");
///
/// let ty: Type = parse_quote!(String);
/// assert_eq!(rust_to_sqlite_type(&ty), "TEXT");
/// ```
pub fn rust_to_sqlite_type(ty: &Type) -> String {
    let type_name = extract_type_name(ty);

    match type_name.as_str() {
        // Integer types
        "u8" | "u16" | "u32" | "i8" | "i16" | "i32" | "i64" | "usize" | "isize" => {
            "INTEGER".to_string()
        },

        // Floating point types
        "f32" | "f64" => "REAL".to_string(),

        // String types
        "String" | "str" => "TEXT".to_string(),

        // Boolean type
        "bool" => "BOOLEAN".to_string(),

        // Date/Time types (chrono)
        "DateTime" => "TIMESTAMP".to_string(),
        "NaiveDateTime" => "TIMESTAMP".to_string(),
        "Date" | "NaiveDate" => "DATE".to_string(),

        // JSON types (stored as TEXT)
        "Value" => "TEXT".to_string(), // serde_json::Value
        _name if is_json_type(ty) => "TEXT".to_string(),

        // Binary types
        "Vec" if is_u8_vec(ty) => "BLOB".to_string(),

        // Default to TEXT for unknown types
        _ => {
            // If it's a Vec or other complex type, default to TEXT
            "TEXT".to_string()
        },
    }
}

/// Handle Optional types (`Option<T>`)
///
/// Returns: (SQL type, is_optional)
///
/// # Examples
///
/// ```ignore
/// let ty: Type = parse_quote!(Option<String>);
/// let (sql_type, is_optional) = handle_optional_type(&ty);
/// assert_eq!(sql_type, "TEXT");
/// assert!(is_optional);
///
/// let ty: Type = parse_quote!(String);
/// let (sql_type, is_optional) = handle_optional_type(&ty);
/// assert_eq!(sql_type, "TEXT");
/// assert!(!is_optional);
/// ```
pub fn handle_optional_type(ty: &Type) -> (String, bool) {
    if let Some(inner_ty) = extract_option_inner(ty) {
        let sql_type = rust_to_sqlite_type(inner_ty);
        (sql_type, true)
    } else {
        let sql_type = rust_to_sqlite_type(ty);
        (sql_type, false)
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;
    use syn::parse_quote;

    #[test]
    fn test_integer_types() {
        let ty: Type = parse_quote!(u8);
        assert_eq!(rust_to_sqlite_type(&ty), "INTEGER");

        let ty: Type = parse_quote!(u16);
        assert_eq!(rust_to_sqlite_type(&ty), "INTEGER");

        let ty: Type = parse_quote!(i32);
        assert_eq!(rust_to_sqlite_type(&ty), "INTEGER");

        let ty: Type = parse_quote!(i64);
        assert_eq!(rust_to_sqlite_type(&ty), "INTEGER");
    }

    #[test]
    fn test_float_types() {
        let ty: Type = parse_quote!(f32);
        assert_eq!(rust_to_sqlite_type(&ty), "REAL");

        let ty: Type = parse_quote!(f64);
        assert_eq!(rust_to_sqlite_type(&ty), "REAL");
    }

    #[test]
    fn test_string_types() {
        let ty: Type = parse_quote!(String);
        assert_eq!(rust_to_sqlite_type(&ty), "TEXT");

        let ty: Type = parse_quote!(&str);
        assert_eq!(rust_to_sqlite_type(&ty), "TEXT");
    }

    #[test]
    fn test_bool_type() {
        let ty: Type = parse_quote!(bool);
        assert_eq!(rust_to_sqlite_type(&ty), "BOOLEAN");
    }

    #[test]
    fn test_datetime_types() {
        let ty: Type = parse_quote!(DateTime<Utc>);
        assert_eq!(rust_to_sqlite_type(&ty), "TIMESTAMP");

        let ty: Type = parse_quote!(NaiveDateTime);
        assert_eq!(rust_to_sqlite_type(&ty), "TIMESTAMP");

        let ty: Type = parse_quote!(Date);
        assert_eq!(rust_to_sqlite_type(&ty), "DATE");
    }

    #[test]
    fn test_json_types() {
        let ty: Type = parse_quote!(serde_json::Value);
        assert_eq!(rust_to_sqlite_type(&ty), "TEXT");

        let ty: Type = parse_quote!(HashMap<String, String>);
        assert_eq!(rust_to_sqlite_type(&ty), "TEXT");
    }

    #[test]
    fn test_binary_types() {
        let ty: Type = parse_quote!(Vec<u8>);
        assert_eq!(rust_to_sqlite_type(&ty), "BLOB");

        // Vec of other types should be TEXT
        let ty: Type = parse_quote!(Vec<String>);
        assert_eq!(rust_to_sqlite_type(&ty), "TEXT");
    }

    #[test]
    fn test_handle_optional_type() {
        // Optional String
        let ty: Type = parse_quote!(Option<String>);
        let (sql_type, is_optional) = handle_optional_type(&ty);
        assert_eq!(sql_type, "TEXT");
        assert!(is_optional);

        // Non-optional String
        let ty: Type = parse_quote!(String);
        let (sql_type, is_optional) = handle_optional_type(&ty);
        assert_eq!(sql_type, "TEXT");
        assert!(!is_optional);

        // Optional integer
        let ty: Type = parse_quote!(Option<u16>);
        let (sql_type, is_optional) = handle_optional_type(&ty);
        assert_eq!(sql_type, "INTEGER");
        assert!(is_optional);
    }

    #[test]
    fn test_unknown_type_defaults_to_text() {
        let ty: Type = parse_quote!(SomeCustomType);
        assert_eq!(rust_to_sqlite_type(&ty), "TEXT");
    }
}
