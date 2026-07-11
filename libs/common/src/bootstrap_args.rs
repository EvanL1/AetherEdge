//! Common command-line arguments for all AetherEMS services
//!
//! Provides a unified argument structure that can be extended by individual services

#[cfg(feature = "cli")]
use clap::Parser;

/// Common service startup arguments
///
/// These arguments are shared by all AetherEMS services and provide
/// standard configuration options for logging, debugging, and validation.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "cli", derive(Parser))]
#[cfg_attr(feature = "cli", clap(author, version, about))]
pub struct ServiceArgs {
    /// Log level (trace, debug, info, warn, error)
    #[cfg_attr(
        feature = "cli",
        clap(short = 'l', long, default_value = "info", env = "RUST_LOG")
    )]
    pub log_level: String,

    /// Optional bind address override (e.g., 127.0.0.1:6001)
    #[cfg_attr(feature = "cli", clap(short = 'b', long, env = "BIND_ADDRESS"))]
    pub bind_address: Option<String>,

    /// Enable debug mode with verbose output
    #[cfg_attr(feature = "cli", clap(long, env = "DEBUG"))]
    pub debug: bool,

    /// Disable colored output (useful for log files)
    #[cfg_attr(feature = "cli", clap(long))]
    pub no_color: bool,

    /// Only validate configuration without starting service
    #[cfg_attr(feature = "cli", clap(long))]
    pub validate: bool,

    /// Watch configuration for changes and reload automatically
    #[cfg_attr(feature = "cli", clap(long))]
    pub watch: bool,

    /// Custom database path override
    #[cfg_attr(feature = "cli", clap(long, env = "DB_PATH"))]
    pub db_path: Option<String>,

    /// Redis URL override
    #[cfg(feature = "redis")]
    #[cfg_attr(feature = "cli", clap(long, env = "REDIS_URL"))]
    pub redis_url: Option<String>,
}

impl Default for ServiceArgs {
    fn default() -> Self {
        Self {
            log_level: "info".to_string(),
            bind_address: None,
            debug: false,
            no_color: false,
            validate: false,
            watch: false,
            db_path: None,
            #[cfg(feature = "redis")]
            redis_url: None,
        }
    }
}

impl ServiceArgs {
    /// Parse log level string to tracing::Level
    pub fn parse_log_level(&self) -> tracing::Level {
        match self.log_level.to_lowercase().as_str() {
            "trace" => tracing::Level::TRACE,
            "debug" => tracing::Level::DEBUG,
            "warn" | "warning" => tracing::Level::WARN,
            "error" => tracing::Level::ERROR,
            _ => tracing::Level::INFO,
        }
    }

    /// Check if running in development mode
    pub fn is_development(&self) -> bool {
        self.debug || self.log_level == "debug" || self.log_level == "trace"
    }

    /// Get database path with unified database defaults
    pub fn get_db_path(&self, _service_name: &str) -> String {
        if let Some(path) = &self.db_path {
            return path.clone();
        }

        // Check unified database environment variable
        if let Ok(path) = std::env::var("AETHER_DB_PATH") {
            return path;
        }

        // Check DATABASE_DIR for all services (unified database)
        if let Ok(dir) = std::env::var("DATABASE_DIR") {
            return format!("{}/aether.db", dir);
        }

        // Default unified database path
        "data/aether.db".to_string()
    }

    /// Get Redis URL with fallback to defaults
    #[cfg(feature = "redis")]
    pub fn get_redis_url(&self) -> String {
        if let Some(url) = &self.redis_url {
            return url.clone();
        }

        std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string())
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;

    #[test]
    fn test_default_args() {
        let args = ServiceArgs::default();
        assert_eq!(args.log_level, "info");
        assert!(!args.debug);
        assert!(!args.validate);
    }

    #[test]
    fn test_parse_log_level() {
        let args = ServiceArgs {
            log_level: "debug".to_string(),
            ..Default::default()
        };
        assert_eq!(args.parse_log_level(), tracing::Level::DEBUG);

        let args = ServiceArgs {
            log_level: "WARN".to_string(),
            ..Default::default()
        };
        assert_eq!(args.parse_log_level(), tracing::Level::WARN);

        let args = ServiceArgs {
            log_level: "invalid".to_string(),
            ..Default::default()
        };
        assert_eq!(args.parse_log_level(), tracing::Level::INFO);
    }

    #[test]
    fn test_is_development() {
        let args = ServiceArgs::default();
        assert!(!args.is_development());

        let args = ServiceArgs {
            debug: true,
            ..Default::default()
        };
        assert!(args.is_development());

        let args = ServiceArgs {
            debug: false,
            log_level: "debug".to_string(),
            ..Default::default()
        };
        assert!(args.is_development());
    }

    #[test]
    fn test_get_db_path() {
        // Clean up any environment variables that might affect the test
        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::remove_var("AETHER_DB_PATH") };
        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::remove_var("DATABASE_DIR") };

        let args = ServiceArgs::default();
        assert_eq!(args.get_db_path("io"), "data/aether.db");

        let args = ServiceArgs {
            db_path: Some("/custom/path.db".to_string()),
            ..Default::default()
        };
        assert_eq!(args.get_db_path("io"), "/custom/path.db");
    }
}
