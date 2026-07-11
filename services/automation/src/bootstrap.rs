//! Service Bootstrap and Initialization
//!
//! Handles all service initialization including logging, configuration,
//! database connections, and component setup.

use crate::config::AutomationConfig;
use common::bootstrap_args::ServiceArgs;
use common::bootstrap_database::setup_sqlite_pool;
use common::bootstrap_system::{SystemRequirements, check_system_requirements_with};
use common::service_bootstrap::{ServiceInfo, get_service_port};
use common::sqlite::ServiceConfigLoader;
use common::{ApiConfig, BaseServiceConfig, DEFAULT_API_HOST};
use sqlx::SqlitePool;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

// Import from error module directly (works in both lib and bin context)
use super::error::{AutomationError, Result};

use crate::app_state::AppState;
use crate::infra::application_control::{AutomationCommandDispatcher, ControlAuthenticator};
use crate::instance_manager::InstanceManager;
use crate::product_loader::ProductLoader;
use aether_store_local::SqliteAuditSink;

/// Initialize service info for unified bootstrap
pub fn create_service_info() -> ServiceInfo {
    ServiceInfo::new(
        "aether-automation",
        "Model Service - Instance & Routing Management",
        6002,
    )
}

/// Initialize logging and environment
pub fn init_environment(service_info: &ServiceInfo) -> Result<()> {
    // Load environment variables from .env file
    common::service_bootstrap::load_development_env();

    // Initialize logging using service_bootstrap (config not loaded yet, use env/default)
    common::service_bootstrap::init_logging(service_info, None).map_err(|e| {
        AutomationError::ConfigError(format!("Failed to initialize logging: {}", e))
    })?;

    // Print startup banner using service_bootstrap
    common::service_bootstrap::print_startup_banner(service_info);

    // Enable SIGHUP-triggered log reopen for long-running processes
    common::logging::enable_sighup_log_reopen();

    info!("Automation starting");

    Ok(())
}

/// Load configuration from SQLite database
pub async fn load_configuration(service_info: &ServiceInfo) -> Result<AutomationConfig> {
    let db_path = ServiceArgs::default().get_db_path("automation");

    if !std::path::Path::new(&db_path).exists() {
        error!("DB not found: {}", db_path);
        return Err(AutomationError::DatabaseError(format!(
            "Database not found: {}",
            db_path
        )));
    }

    info!("Loading config: {}", db_path);
    let service_config = ServiceConfigLoader::new(&db_path, "aether-automation")
        .await
        .map_err(|e| {
            AutomationError::ConfigError(format!("Failed to initialize config loader: {}", e))
        })?
        .load_config()
        .await
        .map_err(|e| {
            AutomationError::ConfigError(format!("Failed to load configuration: {}", e))
        })?;

    // Convert ServiceConfig to AutomationConfig (following Rules pattern)
    let api_host = std::env::var("API_HOST")
        .ok()
        .filter(|host| !host.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_API_HOST.to_string());
    let mut config = AutomationConfig {
        service: BaseServiceConfig {
            name: service_config.service_name,
            description: service_config
                .extra_config
                .get("description")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            version: service_config
                .extra_config
                .get("version")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        },
        api: ApiConfig {
            host: api_host,
            port: service_config.port,
        },
        products_path: service_config
            .extra_config
            .get("products_path")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        instances_path: service_config
            .extra_config
            .get("instances_path")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        auto_load_instances: service_config
            .extra_config
            .get("auto_load_instances")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
    };

    debug!("Config loaded");

    // Apply configuration priority: DB > ENV > Default
    config.api.port = get_service_port(config.api.port, service_info);

    // Perform runtime validation
    validate_configuration(&config)?;

    Ok(config)
}

/// Validate configuration
fn validate_configuration(config: &AutomationConfig) -> Result<()> {
    debug!("Validating config");

    let skip_full_check = std::env::var("SKIP_VALIDATION").is_ok();
    if !skip_full_check {
        // Basic runtime validation
        if config.api.port == 0 {
            error!("Invalid port: 0");
            return Err(AutomationError::InvalidConfig(
                "api.port: Port cannot be 0".to_string(),
            ));
        }
        debug!("Config valid");
    }

    debug!("Validation done");
    Ok(())
}

/// Wrapper for SQLite setup with automation defaults
async fn setup_sqlite() -> Result<SqlitePool> {
    let db_path = ServiceArgs::default().get_db_path("automation");
    info!("SQLite: {}", db_path);
    setup_sqlite_pool(&db_path).await.map_err(Into::into)
}

/// Load product definitions and initialize their SQLite schema.
///
/// If `config.products_path` is set and the directory exists, external product
/// JSON files override built-in products. Otherwise, built-in products are used.
pub async fn load_products(
    config: &AutomationConfig,
    sqlite_pool: &SqlitePool,
) -> Result<Arc<ProductLoader>> {
    use aether_model::product_lib::ProductLibrary;
    use std::sync::Arc as StdArc;

    // Load product library with optional external overrides
    let products_dir = config
        .products_path
        .as_ref()
        .map(std::path::Path::new)
        .filter(|p| p.is_dir());

    let library = ProductLibrary::load(products_dir).map_err(|e| {
        super::error::AutomationError::ConfigError(format!("Failed to load product library: {}", e))
    })?;
    let product_count = library.len();

    let product_loader = ProductLoader::with_library(sqlite_pool.clone(), StdArc::new(library));

    // Initialize instance schema
    product_loader.init_schema().await?;

    // Ensure rules tables exist (normally created by `aether init`,
    // but needed for standalone startup)
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS rules (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            description TEXT,
            enabled BOOLEAN DEFAULT TRUE,
            priority INTEGER DEFAULT 0,
            cooldown_ms INTEGER DEFAULT 0,
            trigger_config TEXT,
            nodes_json TEXT NOT NULL,
            flow_json TEXT,
            format TEXT DEFAULT 'vue-flow',
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
        )",
    )
    .execute(sqlite_pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS rule_history (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            rule_id INTEGER NOT NULL,
            triggered_at TIMESTAMP NOT NULL,
            execution_result TEXT,
            error TEXT,
            FOREIGN KEY (rule_id) REFERENCES rules(id)
        )",
    )
    .execute(sqlite_pool)
    .await?;

    if products_dir.is_some() {
        info!(
            "{} products loaded (with external overrides)",
            product_count
        );
    } else {
        info!("{} built-in products available", product_count);
    }

    Ok(Arc::new(product_loader))
}

/// Setup the SQLite/SHM instance manager.
pub async fn setup_instance_manager(
    sqlite_pool: &SqlitePool,
    routing_cache: Arc<aether_routing::RoutingCache>,
    product_loader: Arc<ProductLoader>,
    dispatch: Arc<dyn crate::infra::shm_dispatch::ActionDispatch>,
) -> Result<Arc<InstanceManager>> {
    let instance_manager = Arc::new(InstanceManager::new(
        sqlite_pool.clone(),
        routing_cache,
        product_loader,
        dispatch,
    ));

    // Instances loaded by aether (may be empty on first startup)
    let instance_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM instances")
        .fetch_one(sqlite_pool)
        .await
        .unwrap_or(0);

    if instance_count == 0 {
        warn!("No instances in DB — run `aether sync` to load instance config");
    } else {
        info!("{} instances loaded", instance_count);
    }

    instance_manager.populate_name_cache().await?;

    Ok(instance_manager)
}

/// Validate routing integrity and check for orphan records
///
/// This function validates that all routing table entries point to existing
/// channel points. It's called during service startup to ensure data integrity.
///
/// # Arguments
/// * `sqlite_pool` - SQLite connection pool
///
/// # Returns
/// * `Ok(())` - Validation passed or orphans found but service can continue
/// * `Err(AutomationError)` - Critical validation failure
///
/// # Behavior
/// - Reports orphan measurement_routing records (T/S points not found)
/// - Reports orphan action_routing records (C/A points not found)
/// - Logs warnings but allows service to start
/// - Suggests running migration script if orphans found
pub async fn validate_routing_integrity(sqlite_pool: &SqlitePool) -> Result<()> {
    debug!("Validating routing");

    // Check measurement_routing for orphan T/S points
    let orphan_telemetry: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)
        FROM measurement_routing
        WHERE channel_type = 'T'
          AND NOT EXISTS (
              SELECT 1 FROM telemetry_points
              WHERE telemetry_points.channel_id = measurement_routing.channel_id
                AND telemetry_points.point_id = measurement_routing.channel_point_id
          )
        "#,
    )
    .fetch_one(sqlite_pool)
    .await
    .unwrap_or(0);

    let orphan_signal: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)
        FROM measurement_routing
        WHERE channel_type = 'S'
          AND NOT EXISTS (
              SELECT 1 FROM signal_points
              WHERE signal_points.channel_id = measurement_routing.channel_id
                AND signal_points.point_id = measurement_routing.channel_point_id
          )
        "#,
    )
    .fetch_one(sqlite_pool)
    .await
    .unwrap_or(0);

    // Check action_routing for orphan C/A points
    let orphan_control: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)
        FROM action_routing
        WHERE channel_type = 'C'
          AND NOT EXISTS (
              SELECT 1 FROM control_points
              WHERE control_points.channel_id = action_routing.channel_id
                AND control_points.point_id = action_routing.channel_point_id
          )
        "#,
    )
    .fetch_one(sqlite_pool)
    .await
    .unwrap_or(0);

    let orphan_adjustment: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)
        FROM action_routing
        WHERE channel_type = 'A'
          AND NOT EXISTS (
              SELECT 1 FROM adjustment_points
              WHERE adjustment_points.channel_id = action_routing.channel_id
                AND adjustment_points.point_id = action_routing.channel_point_id
          )
        "#,
    )
    .fetch_one(sqlite_pool)
    .await
    .unwrap_or(0);

    let total_orphans = orphan_telemetry + orphan_signal + orphan_control + orphan_adjustment;

    if total_orphans > 0 {
        warn!(
            "Orphan routes: T={}, S={}, C={}, A={}",
            orphan_telemetry, orphan_signal, orphan_control, orphan_adjustment
        );
    } else {
        debug!("Routing valid");
    }

    Ok(())
}

/// Refresh routing cache from SQLite database
///
/// This function reloads routing data from SQLite and updates the in-memory
/// routing cache. It's called after routing management operations (create/update/delete)
/// to ensure the cache stays synchronized with the database.
///
/// # Arguments
/// * `sqlite_pool` - SQLite connection pool
/// * `routing_cache` - Shared routing cache to refresh
///
/// # Returns
/// * `Ok(usize)` - Number of routes loaded (c2m + m2c)
/// * `Err(anyhow::Error)` - Database or parsing errors
pub async fn refresh_routing_cache(
    sqlite_pool: &SqlitePool,
    routing_cache: &Arc<aether_routing::RoutingCache>,
) -> anyhow::Result<usize> {
    debug!("Refreshing routes");

    // Load fresh routing data from database via shared library
    let maps = aether_routing::load_routing_maps(sqlite_pool).await?;

    let total_routes = maps.c2m.len() + maps.m2c.len();

    // Update cache atomically (clears old data and loads new)
    routing_cache.update(maps.c2m, maps.m2c, maps.c2c);

    info!("Routes refreshed: {}", total_routes);

    Ok(total_routes)
}

/// Create application state with all initialized components
pub async fn create_app_state(service_info: &ServiceInfo) -> Result<Arc<AppState>> {
    // Initialize environment
    init_environment(service_info)?;

    // Wait for io to be healthy before opening SHM (io must create the SHM file first)
    let io_base = common::io_url();
    let io_health = format!("{io_base}/health");
    if let Err(e) = common::dependency::wait_for_dependency(
        "aether-io",
        &io_health,
        std::time::Duration::from_secs(30),
    )
    .await
    {
        warn!("io health check failed: {e}. Continuing startup (SHM may be unavailable).");
    }

    // Check system requirements
    let requirements = SystemRequirements {
        min_cpu_cores: 2,
        min_memory_mb: 512,
        recommended_cpu_cores: 4,
        recommended_memory_mb: 1024,
    };
    check_system_requirements_with(requirements)?;

    // Load configuration
    let config = Arc::new(load_configuration(service_info).await?);

    // Setup SQLite using common function
    let sqlite_pool = setup_sqlite().await?;

    // ============ Phase 1: Load routing configuration from unified database ============
    debug!("Loading routing config");

    // Validate routing integrity before loading (check for orphan records)
    validate_routing_integrity(&sqlite_pool).await?;

    let routing_cache = {
        // Load routing maps directly from the same SQLite pool via shared library
        let maps = aether_routing::load_routing_maps(&sqlite_pool).await?;

        // Save lengths before moving maps
        let c2m_len = maps.c2m.len();
        let m2c_len = maps.m2c.len();

        let cache = Arc::new(aether_routing::RoutingCache::from_maps(
            maps.c2m, maps.m2c, maps.c2c,
        ));

        info!("Routes: {} C2M, {} M2C", c2m_len, m2c_len);

        cache
    };

    // Load products and their local SQLite schema.
    let product_loader = load_products(&config, &sqlite_pool).await?;

    // Create ShmDispatch (initially unconfigured, configured later in main.rs)
    let shm_dispatch = Arc::new(crate::infra::shm_dispatch::ShmDispatch::new());
    let dispatch: Arc<dyn crate::infra::shm_dispatch::ActionDispatch> =
        Arc::clone(&shm_dispatch) as Arc<dyn crate::infra::shm_dispatch::ActionDispatch>;

    // Setup instance manager (routing handled externally by aether-routing library)
    let instance_manager = setup_instance_manager(
        &sqlite_pool,
        routing_cache.clone(),
        Arc::clone(&product_loader),
        dispatch,
    )
    .await?;

    let audit_sink = SqliteAuditSink::initialize(sqlite_pool.clone())
        .await
        .map_err(|error| AutomationError::DatabaseError(error.to_string()))?;
    let command_dispatcher: Arc<dyn aether_ports::CommandDispatcher> = Arc::new(
        AutomationCommandDispatcher::new(Arc::clone(&instance_manager)),
    );
    let audit_sink: Arc<dyn aether_ports::AuditSink> = Arc::new(audit_sink);
    let control_application = Arc::new(aether_application::ControlApplication::new(
        command_dispatcher,
        audit_sink,
        aether_application::SafetyPolicy,
    ));
    let control_authenticator = Arc::new(
        ControlAuthenticator::from_env()
            .map_err(|error| AutomationError::ConfigError(error.to_string()))?,
    );

    // Create application state
    Ok(Arc::new(AppState::new(
        config,
        instance_manager,
        control_application,
        control_authenticator,
        shm_dispatch,
    )))
}
