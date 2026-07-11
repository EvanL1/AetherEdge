//! Core macro implementation
//!
//! This module ties together attribute parsing and SQL generation to implement
//! the `#[derive(Schema)]` macro.

use crate::{attributes::TableArgs, codegen::generate_create_table};
use darling::FromDeriveInput;
use proc_macro2::TokenStream;
use quote::quote;
use syn::DeriveInput;

/// Main implementation of the Schema derive macro
///
/// # Flow
///
/// 1. Parse attributes using darling → `TableArgs`
/// 2. Generate CREATE TABLE SQL → `String`
/// 3. Extract metadata (table name, columns)
/// 4. Generate impl block with constants
///
/// # Generated Constants
///
/// - `CREATE_TABLE_SQL: &'static str` - Full CREATE TABLE statement
/// - `TABLE_NAME: &'static str` - Table name
/// - `COLUMNS: &'static [&'static str]` - Column names array
#[allow(clippy::panic_in_result_fn)]
pub fn derive_schema_impl(input: DeriveInput) -> darling::Result<TokenStream> {
    // Parse table-level attributes
    let table_args = TableArgs::from_derive_input(&input)?;

    // Generate CREATE TABLE SQL
    let sql = generate_create_table(&table_args);

    // Extract metadata
    let table_name = table_args
        .name
        .clone()
        .unwrap_or_else(|| table_args.ident.to_string())
        .to_lowercase()
        .replace(' ', "_");

    // Extract column names - proc-macro panics are compile-time errors
    let fields = table_args
        .data
        .as_ref()
        .take_struct()
        .unwrap_or_else(|| panic!("Schema only supports named structs"))
        .fields;

    let column_names: Vec<String> = fields
        .iter()
        .filter(|f| !f.skip && !f.flatten)
        .map(|f| {
            f.name
                .clone()
                .or_else(|| f.ident.as_ref().map(|i| i.to_string()))
                .unwrap_or_else(|| panic!("Field must have a name"))
        })
        .collect();

    // Generate impl block
    let struct_name = &table_args.ident;

    let output = quote! {
        impl #struct_name {
            /// Full CREATE TABLE SQL statement
            pub const CREATE_TABLE_SQL: &'static str = #sql;

            /// Table name
            pub const TABLE_NAME: &'static str = #table_name;

            /// Column names
            pub const COLUMNS: &'static [&'static str] = &[
                #(#column_names),*
            ];
        }
    };

    Ok(output)
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;
    use syn::parse_quote;

    #[test]
    fn test_simple_table() {
        let input: DeriveInput = parse_quote! {
            #[derive(Schema)]
            #[table(name = "users")]
            pub struct User {
                #[column(primary_key)]
                pub id: i64,

                #[column(unique, not_null)]
                pub name: String,

                pub email: Option<String>,
            }
        };

        let result = derive_schema_impl(input).unwrap();
        let code = result.to_string();

        // Check generated constants exist
        assert!(code.contains("CREATE_TABLE_SQL"));
        assert!(code.contains("TABLE_NAME"));
        assert!(code.contains("COLUMNS"));

        // Check SQL structure
        assert!(code.contains("CREATE TABLE"));
        assert!(code.contains("users"));
        assert!(code.contains("PRIMARY KEY"));
        assert!(code.contains("UNIQUE"));
        assert!(code.contains("NOT NULL"));
    }

    #[test]
    fn test_table_with_defaults() {
        let input: DeriveInput = parse_quote! {
            #[derive(Schema)]
            pub struct Channel {
                #[column(primary_key)]
                pub channel_id: u16,

                #[column(default = "modbus_tcp")]
                pub protocol: String,

                #[column(default = "true")]
                pub enabled: bool,
            }
        };

        let result = derive_schema_impl(input).unwrap();
        let code = result.to_string();

        assert!(code.contains("DEFAULT"));
        assert!(code.contains("'modbus_tcp'"));
        assert!(code.contains("TRUE"));
    }

    #[test]
    fn test_foreign_key() {
        let input: DeriveInput = parse_quote! {
            #[derive(Schema)]
            #[table(name = "measurement_routing")]
            pub struct MeasurementRouting {
                #[column(primary_key, autoincrement)]
                pub routing_id: i64,

                #[column(
                    not_null,
                    references = "instances(instance_id)",
                    on_delete = "CASCADE"
                )]
                pub instance_id: u16,
            }
        };

        let result = derive_schema_impl(input).unwrap();
        let code = result.to_string();

        assert!(code.contains("REFERENCES"));
        assert!(code.contains("instances"));
        assert!(code.contains("ON DELETE CASCADE"));
    }

    #[test]
    fn test_skip_field() {
        let input: DeriveInput = parse_quote! {
            #[derive(Schema)]
            pub struct Config {
                #[column(primary_key)]
                pub id: u16,

                pub name: String,

                #[column(skip)]
                pub runtime_data: String,
            }
        };

        let result = derive_schema_impl(input).unwrap();
        let code = result.to_string();

        // runtime_data should not appear in SQL
        assert!(!code.contains("runtime_data"));

        // But id and name should exist
        assert!(code.contains("\"id\""));
        assert!(code.contains("\"name\""));
    }

    #[test]
    fn test_columns_array() {
        let input: DeriveInput = parse_quote! {
            #[derive(Schema)]
            pub struct Point {
                pub point_id: u32,
                pub signal_name: String,
                #[column(skip)]
                pub skip_me: String,
            }
        };

        let result = derive_schema_impl(input).unwrap();
        let code = result.to_string();

        // COLUMNS array should include point_id and signal_name
        assert!(code.contains("\"point_id\""));
        assert!(code.contains("\"signal_name\""));

        // But not skip_me
        assert!(!code.contains("\"skip_me\""));
    }
}
