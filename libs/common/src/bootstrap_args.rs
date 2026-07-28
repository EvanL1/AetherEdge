//! Shared resolution for the unified local SQLite database path.

/// Resolve the site database from deployment environment or the local default.
pub fn database_path() -> String {
    if let Ok(path) = std::env::var("AETHER_DB_PATH") {
        return path;
    }
    if let Ok(dir) = std::env::var("DATABASE_DIR") {
        return format!("{dir}/aether.db");
    }
    "data/aether.db".to_string()
}
