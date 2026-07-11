//! Aether Schema Macro - Automatic SQL DDL Generation
//!
//! This proc-macro crate provides automatic CREATE TABLE statement generation
//! from Rust struct definitions, eliminating the need to manually maintain
//! SQL schema constants.
//!
//! # Features
//!
//! - **Automatic type mapping**: Rust types → SQLite types
//! - **Constraint support**: PRIMARY KEY, UNIQUE, NOT NULL, DEFAULT
//! - **Foreign keys**: REFERENCES with CASCADE actions
//! - **Index support**: CREATE INDEX (planned)
//! - **#[serde(flatten)] compatibility**: Flatten nested structs
//!
//! # Example
//!
//! ```rust,ignore
//! use aether_schema_macro::Schema;
//!
//! #[derive(Schema)]
//! #[table(name = "channels")]
//! pub struct ChannelRecord {
//!     #[column(primary_key)]
//!     pub channel_id: u16,
//!
//!     #[column(unique, not_null)]
//!     pub name: String,
//!
//!     #[column(default = true)]
//!     pub enabled: bool,
//! }
//!
//! // Use generated constants:
//! let sql = ChannelRecord::CREATE_TABLE_SQL;
//! let table_name = ChannelRecord::TABLE_NAME;
//! let columns = ChannelRecord::COLUMNS;
//! ```

use proc_macro::TokenStream;
use syn::{DeriveInput, parse_macro_input};

mod attributes;
mod codegen;
pub(crate) mod schema;
mod types;
mod utils;

/// Derive macro for automatic CREATE TABLE DDL generation
///
/// # Attributes
///
/// ## Table-level
/// - `#[table(name = "...")]` - Override table name (default: struct name in snake_case)
/// - `#[table(if_not_exists = false)]` - Disable IF NOT EXISTS clause
/// - `#[table(suffix = "...")]` - Add custom SQL suffix (e.g., "WITHOUT ROWID")
///
/// ## Column-level
/// - `#[column(primary_key)]` - Mark as primary key
/// - `#[column(autoincrement)]` - Add AUTOINCREMENT (INTEGER PRIMARY KEY only)
/// - `#[column(unique)]` - Add UNIQUE constraint
/// - `#[column(not_null)]` - Add NOT NULL constraint
/// - `#[column(default = "...")]` - Set default value
/// - `#[column(references = "table(column)")]` - Add foreign key reference
/// - `#[column(on_delete = "CASCADE")]` - Foreign key DELETE action
/// - `#[column(on_update = "CASCADE")]` - Foreign key UPDATE action
/// - `#[column(skip)]` - Skip this field in table generation
/// - `#[column(flatten)]` - Skip flattened fields (users should manually declare needed fields)
///
/// # Type Mapping
///
/// | Rust Type | SQLite Type |
/// |-----------|-------------|
/// | u8, u16, u32, i8, i16, i32, i64 | INTEGER |
/// | f32, f64 | REAL |
/// | String, &str | TEXT |
/// | bool | BOOLEAN |
/// | DateTime, NaiveDateTime | TIMESTAMP |
/// | Date, NaiveDate | DATE |
/// | serde_json::Value, HashMap | TEXT (JSON) |
/// | `Vec<u8>` | BLOB |
/// | `Option<T>` | (nullable) |
///
/// # Example with Foreign Keys
///
/// ```rust,ignore
/// #[derive(Schema)]
/// #[table(name = "measurement_routing")]
/// pub struct MeasurementRouting {
///     #[column(primary_key, autoincrement)]
///     pub routing_id: i64,
///
///     #[column(
///         not_null,
///         references = "instances(instance_id)",
///         on_delete = "CASCADE"
///     )]
///     pub instance_id: u16,
///
///     #[column(default = "true")]
///     pub enabled: bool,
/// }
/// ```
///
/// # Working with Flatten Fields
///
/// When using `#[serde(flatten)]` with nested structs, fields marked with
/// `#[column(flatten)]` will be automatically skipped. To include nested struct
/// fields in your table, manually declare them in the parent struct:
///
/// ```rust,ignore
/// // Core structure (reusable)
/// struct ChannelCore {
///     id: u16,
///     name: String,
///     enabled: bool,
/// }
///
/// // Database schema (manually expanded)
/// #[derive(Schema)]
/// #[table(name = "channels")]
/// struct ChannelRecord {
///     // Manually declare core fields for database
///     #[column(primary_key)]
///     pub id: u16,
///
///     #[column(not_null)]
///     pub name: String,
///
///     #[column(default = "true")]
///     pub enabled: bool,
///
///     // Additional fields
///     pub protocol: String,
/// }
///
/// // Runtime configuration (with flatten)
/// struct ChannelConfig {
///     #[serde(flatten)]
///     core: ChannelCore,
///     protocol: String,
/// }
/// ```
#[proc_macro_derive(Schema, attributes(table, column, index))]
pub fn derive_schema(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    schema::derive_schema_impl(input)
        .unwrap_or_else(|err| err.write_errors())
        .into()
}
