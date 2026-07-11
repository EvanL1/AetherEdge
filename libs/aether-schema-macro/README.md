# aether-schema-macro

Automatic SQL DDL generation from Rust struct definitions.

## Overview

This proc-macro crate eliminates the need to manually maintain SQL schema constants by automatically generating CREATE TABLE statements from Rust struct definitions.

## Features

- ✅ **Automatic type mapping**: Rust types → SQLite types
- ✅ **Full constraint support**: PRIMARY KEY, UNIQUE, NOT NULL, DEFAULT
- ✅ **Foreign keys**: REFERENCES with CASCADE actions
- ✅ **Optional fields**: `Option<T>` → nullable columns
- ✅ **Skip fields**: Exclude runtime-only fields
- ✅ **Flatten support**: Skip nested struct fields

## Quick Start

```rust
use aether_schema_macro::Schema;

#[derive(Schema)]
#[table(name = "channels")]
pub struct ChannelRecord {
    #[column(primary_key)]
    pub channel_id: u16,

    #[column(unique, not_null)]
    pub name: String,

    #[column(default = "modbus_tcp")]
    pub protocol: String,

    #[column(default = "true")]
    pub enabled: bool,
}

// Generated constants:
println!("{}", ChannelRecord::CREATE_TABLE_SQL);
println!("Table: {}", ChannelRecord::TABLE_NAME);
println!("Columns: {:?}", ChannelRecord::COLUMNS);
```

Generated SQL:
```sql
CREATE TABLE IF NOT EXISTS channels (
    channel_id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    protocol TEXT NOT NULL DEFAULT 'modbus_tcp',
    enabled BOOLEAN NOT NULL DEFAULT TRUE
)
```

## Attributes

### Table-Level

| Attribute | Default | Description |
|-----------|---------|-------------|
| `#[table(name = "...")]` | struct name | Override table name |
| `#[table(if_not_exists = false)]` | `true` | Disable IF NOT EXISTS |
| `#[table(suffix = "...")]` | - | Custom SQL suffix |

### Column-Level

| Attribute | Description |
|-----------|-------------|
| `#[column(primary_key)]` | PRIMARY KEY constraint |
| `#[column(autoincrement)]` | AUTOINCREMENT (INTEGER PRIMARY KEY only) |
| `#[column(unique)]` | UNIQUE constraint |
| `#[column(not_null)]` | NOT NULL constraint |
| `#[column(default = "...")]` | DEFAULT value |
| `#[column(references = "table(column)")]` | Foreign key |
| `#[column(on_delete = "CASCADE")]` | Foreign key DELETE action |
| `#[column(on_update = "CASCADE")]` | Foreign key UPDATE action |
| `#[column(skip)]` | Skip this field |
| `#[column(flatten)]` | Skip flattened field |

## Type Mapping

| Rust Type | SQLite Type |
|-----------|-------------|
| `u8, u16, u32, i8, i16, i32, i64, usize, isize` | INTEGER |
| `f32, f64` | REAL |
| `String, &str` | TEXT |
| `bool` | BOOLEAN |
| `DateTime, NaiveDateTime` | TIMESTAMP |
| `Date, NaiveDate` | DATE |
| `serde_json::Value, HashMap, BTreeMap` | TEXT (JSON) |
| `Vec<u8>` | BLOB |
| `Option<T>` | Nullable column |

## Examples

### Foreign Keys

```rust
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

    #[column(
        not_null,
        references = "channels(channel_id)",
        on_delete = "CASCADE"
    )]
    pub channel_id: u16,
}
```

### Optional Fields

```rust
#[derive(Schema)]
pub struct Device {
    #[column(primary_key)]
    pub device_id: u16,

    pub name: String,           // NOT NULL (required)
    pub description: Option<String>,  // Nullable (optional)
}
```

### Skip Fields

```rust
#[derive(Schema)]
pub struct Config {
    #[column(primary_key)]
    pub id: u16,

    pub name: String,

    #[column(skip)]
    pub runtime_cache: HashMap<String, String>,  // Not in database
}
```

### Working with Flatten

When using `#[serde(flatten)]` for runtime structs, manually declare database fields:

```rust
// Shared core structure
struct ChannelCore {
    id: u16,
    name: String,
    enabled: bool,
}

// Database schema (manually expanded)
#[derive(Schema)]
#[table(name = "channels")]
struct ChannelRecord {
    #[column(primary_key)]
    pub id: u16,

    #[column(not_null)]
    pub name: String,

    #[column(default = "true")]
    pub enabled: bool,

    pub protocol: String,
}

// Runtime config (with flatten)
struct ChannelConfig {
    #[serde(flatten)]
    core: ChannelCore,
    protocol: String,
}
```

## Default Value Rules

- **SQL functions**: No quotes (`CURRENT_TIMESTAMP`, `CURRENT_DATE`)
- **Booleans**: `"true"` → `TRUE`, `"false"` → `FALSE`
- **Numbers**: No quotes (`"42"` → `42`, `"3.14"` → `3.14`)
- **Strings**: Single quotes with escaping (`"hello"` → `'hello'`, `"it's"` → `'it''s'`)

## Generated Constants

```rust
#[derive(Schema)]
pub struct MyTable { /* ... */ }

// Available constants:
MyTable::CREATE_TABLE_SQL  // &'static str - Full DDL
MyTable::TABLE_NAME        // &'static str - Table name
MyTable::COLUMNS           // &'static [&'static str] - Column names
```

## Limitations

1. **Flatten fields**: `#[column(flatten)]` marks fields to skip. To include nested fields, manually declare them in the parent struct.
2. **Proc-macro constraints**: Cannot access type definitions from other crates at compile time.
3. **SQLite focus**: Type mapping optimized for SQLite (can be extended for other databases).

## Testing

```bash
# Run unit tests
cargo test -p aether-schema-macro --lib

# Run integration tests
cargo test -p aether-schema-macro --test integration

# Run all tests
cargo test -p aether-schema-macro
```

## License

Part of the AetherEMS project.
