//! Attribute parsing for Schema macro
//!
//! This module uses `darling` to parse derive macro attributes in a type-safe way.

use darling::{FromDeriveInput, FromField, FromMeta, ast};
use syn::Ident;

/// Table-level attributes (#[table(...)])
///
/// # Examples
///
/// ```ignore
/// #[derive(Schema)]
/// #[table(name = "channels", if_not_exists = true)]
/// pub struct ChannelRecord { /* ... */ }
/// ```
#[derive(Debug, FromDeriveInput)]
#[darling(attributes(table), supports(struct_named))]
pub struct TableArgs {
    /// Struct identifier
    pub ident: Ident,

    /// Struct fields
    pub data: ast::Data<(), FieldArgs>,

    /// Table name (default: struct name in snake_case)
    pub name: Option<String>,

    /// Add IF NOT EXISTS clause (default: true)
    #[darling(default = "default_true")]
    pub if_not_exists: bool,

    /// Custom SQL suffix (e.g., "WITHOUT ROWID")
    pub suffix: Option<String>,
}

/// Column-level attributes (#[column(...)])
///
/// # Examples
///
/// ```ignore
/// #[column(primary_key, autoincrement)]
/// pub id: i64,
///
/// #[column(unique, not_null)]
/// pub name: String,
///
/// #[column(default = "modbus_tcp")]
/// pub protocol: String,
///
/// #[column(references = "instances(instance_id)", on_delete = "CASCADE")]
/// pub instance_id: u16,
/// ```
#[derive(Debug, Clone, FromField)]
#[darling(attributes(column))]
pub struct FieldArgs {
    /// Field identifier
    pub ident: Option<Ident>,

    /// Field type
    pub ty: syn::Type,

    /// Column name (default: field name)
    pub name: Option<String>,

    /// PRIMARY KEY constraint
    #[darling(default)]
    pub primary_key: bool,

    /// AUTOINCREMENT (only for INTEGER PRIMARY KEY)
    #[darling(default)]
    pub autoincrement: bool,

    /// UNIQUE constraint
    #[darling(default)]
    pub unique: bool,

    /// NOT NULL constraint
    #[darling(default)]
    pub not_null: bool,

    /// DEFAULT value
    ///
    /// Examples:
    /// - `default = "true"` → BOOLEAN DEFAULT TRUE
    /// - `default = "42"` → INTEGER DEFAULT 42
    /// - `default = "hello"` → TEXT DEFAULT 'hello'
    /// - `default = "CURRENT_TIMESTAMP"` → TIMESTAMP DEFAULT CURRENT_TIMESTAMP
    pub default: Option<String>,

    /// Foreign key reference
    ///
    /// Format: "table(column)"
    /// Example: "instances(instance_id)"
    pub references: Option<String>,

    /// Foreign key ON DELETE action
    pub on_delete: Option<CascadeAction>,

    /// Foreign key ON UPDATE action
    pub on_update: Option<CascadeAction>,

    /// Skip this field in table generation
    #[darling(default)]
    pub skip: bool,

    /// Flatten nested struct fields
    ///
    /// When true, this field's struct fields will be expanded into the parent table.
    /// Commonly used with `#[serde(flatten)]`.
    #[darling(default)]
    pub flatten: bool,
}

/// Foreign key cascade actions
///
/// Corresponds to SQL foreign key actions:
/// - CASCADE: Delete/update child rows when parent is deleted/updated
/// - SET NULL: Set child field to NULL
/// - SET DEFAULT: Set child field to DEFAULT value
/// - RESTRICT: Prevent parent deletion/update if children exist
/// - NO ACTION: Same as RESTRICT
#[derive(Debug, Clone)]
pub enum CascadeAction {
    Cascade,
    SetNull,
    SetDefault,
    Restrict,
    NoAction,
}

impl FromMeta for CascadeAction {
    fn from_string(value: &str) -> darling::Result<Self> {
        match value.to_uppercase().as_str() {
            "CASCADE" => Ok(Self::Cascade),
            "SET NULL" | "SET_NULL" => Ok(Self::SetNull),
            "SET DEFAULT" | "SET_DEFAULT" => Ok(Self::SetDefault),
            "RESTRICT" => Ok(Self::Restrict),
            "NO ACTION" | "NO_ACTION" => Ok(Self::NoAction),
            _ => Err(darling::Error::unknown_value(value)),
        }
    }
}

impl std::fmt::Display for CascadeAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cascade => write!(f, "CASCADE"),
            Self::SetNull => write!(f, "SET NULL"),
            Self::SetDefault => write!(f, "SET DEFAULT"),
            Self::Restrict => write!(f, "RESTRICT"),
            Self::NoAction => write!(f, "NO ACTION"),
        }
    }
}

/// Default value helper for `if_not_exists`
fn default_true() -> bool {
    true
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;

    #[test]
    fn test_cascade_action_from_string() {
        assert!(matches!(
            CascadeAction::from_string("CASCADE"),
            Ok(CascadeAction::Cascade)
        ));
        assert!(matches!(
            CascadeAction::from_string("cascade"),
            Ok(CascadeAction::Cascade)
        ));
        assert!(matches!(
            CascadeAction::from_string("SET NULL"),
            Ok(CascadeAction::SetNull)
        ));
        assert!(matches!(
            CascadeAction::from_string("SET_NULL"),
            Ok(CascadeAction::SetNull)
        ));
    }

    #[test]
    fn test_cascade_action_display() {
        assert_eq!(CascadeAction::Cascade.to_string(), "CASCADE");
        assert_eq!(CascadeAction::SetNull.to_string(), "SET NULL");
        assert_eq!(CascadeAction::SetDefault.to_string(), "SET DEFAULT");
        assert_eq!(CascadeAction::Restrict.to_string(), "RESTRICT");
        assert_eq!(CascadeAction::NoAction.to_string(), "NO ACTION");
    }
}
