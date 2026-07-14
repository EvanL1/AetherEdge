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
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

// Import from error module directly (works in both lib and bin context)
use super::error::{AutomationError, Result};

use crate::app_state::AppState;
use crate::infra::application_control::{AutomationCommandDispatcher, ControlAuthenticator};
use crate::instance_manager::InstanceManager;
use crate::product_loader::ProductLoader;
use aether_pack::{ActivePackSet, load_active_packs};
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

/// Assembles models from validated active Packs and an optional site directory.
///
/// Pack directories are ordered as declared in `global.yaml`; the explicitly
/// configured site directory is last and may intentionally override a Pack
/// model. No active Pack and no site directory produces an empty library.
pub fn load_product_library(
    active_packs: &ActivePackSet,
    site_products: Option<&std::path::Path>,
) -> Result<aether_model::product_lib::ProductLibrary> {
    use aether_model::product_lib::ProductLibrary;
    use std::collections::{BTreeMap, BTreeSet};

    let mut directories = Vec::new();
    let mut owners = BTreeMap::<String, String>::new();
    for pack in active_packs.iter() {
        let model_directory = pack.asset_directory("models");
        let pack_library = match model_directory.as_deref() {
            Some(directory) => ProductLibrary::load(Some(directory)).map_err(|error| {
                AutomationError::ConfigError(format!(
                    "Failed to load models for active Pack {}: {error}",
                    pack.id()
                ))
            })?,
            None => ProductLibrary::load(None).map_err(|error| {
                AutomationError::ConfigError(format!(
                    "Failed to create empty model set for active Pack {}: {error}",
                    pack.id()
                ))
            })?,
        };
        let actual = pack_library
            .names()
            .into_iter()
            .map(str::to_string)
            .collect::<BTreeSet<_>>();
        let declared = pack
            .manifest()
            .capability_ids("models")
            .unwrap_or(&[])
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        if actual != declared {
            let missing = declared.difference(&actual).cloned().collect::<Vec<_>>();
            let unexpected = actual.difference(&declared).cloned().collect::<Vec<_>>();
            return Err(AutomationError::InvalidConfig(format!(
                "active Pack {} model capabilities do not match assets; missing={missing:?}, unexpected={unexpected:?}",
                pack.id()
            )));
        }
        for name in actual {
            if let Some(previous) = owners.insert(name.clone(), pack.id().to_string()) {
                return Err(AutomationError::InvalidConfig(format!(
                    "product {name:?} is declared by both active Packs {previous:?} and {:?}",
                    pack.id()
                )));
            }
        }
        if let Some(directory) = model_directory {
            directories.push(directory);
        }
    }
    if let Some(site_products) = site_products {
        let metadata = std::fs::symlink_metadata(site_products).map_err(|error| {
            AutomationError::ConfigError(format!(
                "explicit products_path {} is unavailable: {error}",
                site_products.display()
            ))
        })?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
            return Err(AutomationError::ConfigError(format!(
                "explicit products_path must be a real directory: {}",
                site_products.display()
            )));
        }
        directories.push(site_products.to_path_buf());
    }
    let selected = directories.iter().map(PathBuf::as_path).collect::<Vec<_>>();
    ProductLibrary::load_directories(&selected).map_err(|error| {
        AutomationError::ConfigError(format!("Failed to load product library: {error}"))
    })
}

/// Rejects startup when persisted instances reference products that are no
/// longer supplied by the active Pack/site library.
pub async fn validate_instance_product_references(
    sqlite_pool: &SqlitePool,
    library: &aether_model::product_lib::ProductLibrary,
) -> Result<()> {
    let referenced = sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT product_name FROM instances ORDER BY product_name",
    )
    .fetch_all(sqlite_pool)
    .await?;
    let missing = referenced
        .into_iter()
        .filter(|name| !library.exists(name))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(AutomationError::InvalidConfig(format!(
            "persisted instances reference products absent from active Packs/site products: {missing:?}"
        )))
    }
}

fn active_pack_config_directory() -> PathBuf {
    if let Some(path) = std::env::var_os("AETHER_CONFIG_PATH").filter(|path| !path.is_empty()) {
        return PathBuf::from(path);
    }
    let container = PathBuf::from("/app/config");
    if container.join("global.yaml").is_file() {
        return container;
    }
    PathBuf::from("data/config")
}

/// Loads the mandatory, feature-exact runtime compatibility view used by the
/// active-Pack loader. Missing, tampered, cross-target, or cross-version
/// metadata fails startup; production never substitutes a static catalog.
pub fn load_pack_runtime_from_manifest(
    config_directory: impl AsRef<std::path::Path>,
) -> Result<aether_pack::PackRuntime> {
    let runtime_manifest = aether_runtime_catalog::load_runtime_manifest_for_current_process(
        config_directory,
        env!("CARGO_PKG_VERSION"),
    )
    .map_err(|error| {
        AutomationError::ConfigError(format!(
            "Failed to load mandatory runtime manifest: {error}"
        ))
    })?;
    runtime_manifest
        .pack_runtime()
        .map_err(|error| AutomationError::ConfigError(format!("Invalid runtime manifest: {error}")))
}

/// Loads product definitions and initializes their SQLite schema.
pub async fn load_products(
    config: &AutomationConfig,
    sqlite_pool: &SqlitePool,
) -> Result<Arc<ProductLoader>> {
    use std::sync::Arc as StdArc;

    let config_directory = active_pack_config_directory();
    let pack_runtime = load_pack_runtime_from_manifest(&config_directory)?;
    let active_packs = load_active_packs(&config_directory, &pack_runtime).map_err(|error| {
        AutomationError::ConfigError(format!("Failed to load active Packs: {error}"))
    })?;
    let products_dir = config.products_path.as_ref().map(std::path::Path::new);
    let library = load_product_library(&active_packs, products_dir)?;
    let product_count = library.len();
    let library = StdArc::new(library);

    let product_loader = ProductLoader::with_library(sqlite_pool.clone(), StdArc::clone(&library));

    // Initialize instance schema
    product_loader.init_schema().await?;
    validate_instance_product_references(sqlite_pool, &library).await?;

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
    common::test_utils::schema::initialize_configuration_revisions(sqlite_pool).await?;
    crate::instance_configuration::initialize_instance_configuration_revision(sqlite_pool).await?;

    info!(
        active_pack_count = active_packs.len(),
        site_products = products_dir.is_some(),
        "{} explicitly selected products available",
        product_count
    );

    Ok(Arc::new(product_loader))
}

/// Setup the SQLite/SHM instance manager.
pub async fn setup_instance_manager(
    sqlite_pool: &SqlitePool,
    product_loader: Arc<ProductLoader>,
) -> Result<Arc<InstanceManager>> {
    let instance_manager = Arc::new(InstanceManager::new(sqlite_pool.clone(), product_loader));

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

    // Load products and their local SQLite schema.
    let product_loader = load_products(&config, &sqlite_pool).await?;

    // The physical command sink is configured with SHM and UDS resources later
    // in main, after IO's canonical generation becomes available.
    let shm_dispatch = Arc::new(aether_shm_bridge::ShmDeviceCommandSink::new());

    let instance_manager =
        setup_instance_manager(&sqlite_pool, Arc::clone(&product_loader)).await?;

    let audit_sink = SqliteAuditSink::initialize(sqlite_pool.clone())
        .await
        .map_err(|error| AutomationError::DatabaseError(error.to_string()))?;
    let command_dispatcher: Arc<dyn aether_ports::CommandDispatcher> =
        Arc::new(AutomationCommandDispatcher::new(
            Arc::clone(&instance_manager),
            Arc::clone(&shm_dispatch) as Arc<dyn aether_ports::DeviceCommandSink>,
        ));
    let audit_sink: Arc<dyn aether_ports::AuditSink> = Arc::new(audit_sink);
    let control_application = Arc::new(aether_application::ControlApplication::new(
        command_dispatcher,
        Arc::clone(&audit_sink),
        aether_application::SafetyPolicy,
    ));
    let action_routing_mutator: Arc<dyn aether_ports::AutomationActionRoutingMutator> = Arc::new(
        crate::infra::action_routing::SqliteActionRoutingMutator::new(Arc::clone(
            &instance_manager,
        )),
    );
    let action_routing_application = Arc::new(aether_application::ActionRoutingApplication::new(
        action_routing_mutator,
        Arc::clone(&audit_sink),
        aether_application::SafetyPolicy,
    ));
    let measurement_routing_mutator: Arc<dyn aether_ports::AutomationMeasurementRoutingMutator> =
        Arc::new(
            crate::infra::measurement_routing::SqliteMeasurementRoutingMutator::new(Arc::clone(
                &instance_manager,
            )),
        );
    let measurement_routing_application =
        Arc::new(aether_application::MeasurementRoutingApplication::new(
            measurement_routing_mutator,
            Arc::clone(&audit_sink),
            aether_application::SafetyPolicy,
        ));
    let instance_configuration_application = Arc::new(
        crate::instance_configuration::InstanceConfigurationApplication::new(
            Arc::clone(&instance_manager),
            audit_sink,
        ),
    );
    let control_authenticator = Arc::new(
        ControlAuthenticator::from_env()
            .map_err(|error| AutomationError::ConfigError(error.to_string()))?,
    );

    // Create application state
    Ok(Arc::new(AppState::new(
        config,
        instance_manager,
        control_application,
        action_routing_application,
        measurement_routing_application,
        instance_configuration_application,
        control_authenticator,
        shm_dispatch,
    )))
}
