//! Aether - Unified Management Tool for AetherEMS
//!
//! A powerful management tool that combines configuration synchronization,
//! service management, and operational control for all AetherEMS services.

mod alarms;
mod channels;
mod core;
mod deploy_mode;
mod doctor;
mod history;
mod install_context;
mod logs;
mod logs_tui;
mod mcp;
mod mcp_docs;
mod models;
mod net;
mod output;
mod pack_artifact;
mod routing;
mod rules;
mod services;
mod setup;
mod shm;
mod shm_dashboard;
mod templates;
mod top;
mod top_draw;
mod transport_security;
mod utils;

// Note: lib-mode (direct service library calls) has been removed in favor of HTTP-only mode.
// This simplifies the architecture, reduces code duplication (~50%), and aligns with MCP patterns.
// Runtime commands use HTTP clients; migration/init/sync are explicit local operations.

use crate::core::{AetherCore, schema};
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use colored::*;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "aether")]
#[command(about = "👑 Aether - AetherEMS Unified Management Tool")]
#[command(long_about = "👑 Aether - AetherEMS Unified Management Tool

Configuration Management:
  setup       Plan a safe first-run setup (apply requires a plan ID)
  sync        Apply configuration offline (services stopped; confirmation required)
  status      Show current configuration status
  init        Initialize database schemas
  export      Export configuration from SQLite to YAML/CSV
  packs       Build or install data-only domain Pack artifacts

Service Operations:
  channels    Manage communication channels and protocols
  models      Manage product templates and device instances
  rules       Manage and execute business rules
  services    Start, stop, and manage AetherEMS services
  logs        Log level control and log file viewer

Examples:
  aether setup                              # Read-only first-run plan
  aether setup apply --plan-id <PLAN_ID>    # Apply an unchanged safe plan
  aether sync --confirmed                   # Apply with runtime owners stopped
  aether sync --dry-run                     # Validate without syncing
  aether channels list                      # List all channels
  aether models products list               # List products
  aether rules enable 1                     # Enable a rule
  aether packs install --artifact ./x.bundle # Verify and atomically activate a Pack
  aether alarms list                        # List active alerts
  aether alarms events --level 3            # High-level alert history
  aether history latest inst:9:M 101        # Latest value of a point
  aether history query inst:9:M 101 --from 2026-05-01T00:00:00Z
  aether services status                    # Check service status
  aether logs level all debug               # Switch all services to debug mode
  aether logs list                          # List today's log files
  aether logs view io -n 100            # View last 100 lines of io log
  aether logs tail aether-automation --grep ERROR

Use 'aether <command> --help' for more information on a specific command.")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Enable verbose logging
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Disable colored output
    #[arg(long, global = true)]
    no_color: bool,

    /// Output as JSON (suppresses banner and color; for scripts and AI agents)
    #[arg(long, global = true)]
    json: bool,

    /// Target host for remote operations (overrides localhost default)
    #[arg(long, global = true)]
    host: Option<String>,

    /// Configuration files path (overrides env, install context, and auto-detection)
    #[arg(short = 'c', long = "config-path", global = true)]
    config_path: Option<String>,

    /// Database files path (overrides env, install context, and auto-detection)
    #[arg(long = "db-path", global = true)]
    db_path: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    // === Configuration Management Commands ===
    /// Plan or apply a conservative first-run setup
    #[command(
        about = "Plan a safe first-run setup; persistent changes require an unchanged plan ID"
    )]
    Setup {
        #[command(subcommand)]
        command: Option<setup::SetupCommand>,
    },

    /// Apply all configuration to SQLite while runtime owners are stopped
    Sync {
        /// Validate only, don't write to database (dry run)
        #[arg(short = 'n', long)]
        dry_run: bool,

        /// Replace sync-managed rows; refused while any governed action route exists
        #[arg(short, long)]
        force: bool,

        /// Show detailed progress for each item
        #[arg(short, long)]
        detailed: bool,

        /// Check database consistency (duplicates, references)
        #[arg(long)]
        check: bool,

        /// Confirm the offline desired-state apply
        #[arg(long)]
        confirmed: bool,
    },

    /// Show current configuration status
    Status {
        /// Show detailed status
        #[arg(short, long)]
        detailed: bool,
    },

    /// Initialize database schema (migration-only, safe upgrade)
    Init {
        /// DEPRECATED: This option is disabled for safety. Database can only be upgraded, not reset.
        #[arg(short, long, hide = true)]
        force: bool,
    },

    /// Export configuration from SQLite to YAML/CSV
    Export {
        /// Output directory (default: config/)
        #[arg(short = 'O', long)]
        output: Option<String>,

        /// Show detailed export progress
        #[arg(short, long)]
        detailed: bool,
    },

    // === Service Management Commands ===
    /// Manage communication channels
    #[command(about = "Manage communication channels and protocols")]
    Channels {
        #[command(subcommand)]
        command: channels::ChannelCommands,
    },

    /// Manage models (products and instances)
    #[command(about = "Manage product templates and device instances")]
    Models {
        #[command(subcommand)]
        command: models::ModelCommands,
    },

    /// Manage business rules
    #[command(about = "Manage and execute business rules")]
    Rules {
        #[command(subcommand)]
        command: rules::RuleCommands,
    },

    /// Manage routing configurations
    #[command(about = "Manage channel-to-instance point routing")]
    Routing {
        #[command(subcommand)]
        command: routing::RoutingCommands,
    },

    /// Manage Docker services
    #[command(about = "Start, stop, and manage AetherEMS services")]
    Services {
        #[command(subcommand)]
        command: services::ServiceCommands,
    },

    /// Manage logs
    #[command(about = "Log level control and log file viewer")]
    Logs {
        #[command(subcommand)]
        command: logs::LogCommands,
    },

    /// Shared memory operations (interactive REPL)
    #[command(about = "Zero-latency shared memory CLI (like mysql-cli)")]
    Shm {
        #[command(subcommand)]
        command: Option<shm::ShmCommands>,
    },

    /// System health check and diagnostics
    #[command(about = "Check system health and diagnose issues")]
    Doctor {
        /// Show detailed information (response times, etc.)
        #[arg(short, long)]
        verbose: bool,
    },

    /// Manage channel templates
    #[command(about = "Manage channel configuration templates")]
    Templates {
        #[command(subcommand)]
        command: templates::TemplateCommands,
    },

    /// Manage alarm rules and inspect active alerts
    #[command(
        about = "Manage alarm rules (create/update/delete/enable/disable); query alerts, events, and statistics"
    )]
    Alarms {
        #[command(subcommand)]
        command: alarms::AlarmCommands,
    },

    /// Manage uplink: MQTT connection/config and TLS certificates
    #[command(about = "Manage MQTT connection, uplink config, and TLS certificates")]
    Net {
        #[command(subcommand)]
        command: net::NetCommands,
    },

    /// Query historical data from history
    #[command(about = "Query historical sensor data (latest values, time-range queries)")]
    History {
        #[command(subcommand)]
        command: history::HistoryCommands,
    },

    /// Interactive TUI dashboard for real-time monitoring
    #[command(about = "Interactive TUI dashboard for real-time monitoring")]
    Top,

    /// Verify and inspect the feature-exact kernel runtime manifest
    #[command(about = "Verify and inspect the installed kernel runtime manifest")]
    RuntimeManifest {
        /// Explicit manifest file; defaults to <config-path>/runtime-manifest.json
        #[arg(long)]
        path: Option<PathBuf>,
    },

    /// Build or install data-only domain Pack artifacts
    #[command(about = "Build or install Pack-only artifacts without Kernel binaries")]
    Packs {
        #[command(subcommand)]
        command: pack_artifact::PackCommands,
    },

    /// Run an MCP (Model Context Protocol) server over stdio
    #[command(about = "Run an MCP server exposing aether's capabilities as tools")]
    Mcp {
        /// Register the 22 governed write tools. Each call still requires
        /// explicit confirmation; the flag is not confirmation.
        #[arg(long)]
        allow_write: bool,
    },
}

/// Resolve service URL from env var or default to scheme://localhost:port
pub(crate) fn service_url(env_var: &str, scheme: &str, port: u16, host: Option<&str>) -> String {
    if let Some(h) = host {
        return format!("{scheme}://{h}:{port}");
    }
    std::env::var(env_var).unwrap_or_else(|_| format!("{scheme}://localhost:{port}"))
}

const BANNER: &str = "\
╔════════════════════════════════════════════════════╗
║                                                    ║
║               AETHER CONFIG MANAGER               ║
║                                                    ║
║    Configuration Management for AetherEdge        ║
║                                                    ║
╚════════════════════════════════════════════════════╝";

fn print_banner() {
    println!("\n{}\n", BANNER.bright_blue());
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let json = cli.json || std::env::var("AETHER_JSON").is_ok();

    if let Err(e) = run(cli).await {
        if json {
            output::print_error(&format!("{e:#}"));
        } else {
            eprintln!("{}: {e:#}", "Error".red());
        }
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    let json = cli.json || std::env::var("AETHER_JSON").is_ok();
    let host = cli.host.as_deref();

    // Configure colored output
    if cli.no_color || json {
        colored::control::set_override(false);
    }

    // Initialize logging
    let log_level = if cli.verbose { "debug" } else { "info" };
    tracing_subscriber::fmt()
        .with_env_filter(log_level)
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    let install_paths = install_context::resolve_install_paths(
        cli.config_path.map(PathBuf::from),
        cli.db_path.map(PathBuf::from),
    )?;
    let config_path = install_paths.config_directory;
    let db_path = install_paths.data_directory;
    let install_mode = install_paths.install_mode;

    if !json && matches!(cli.command, Commands::Init { .. }) && !cli.no_color {
        print_banner();
        println!(
            "{} Config: {}, DB: {}",
            "Paths:".bright_cyan(),
            config_path.display(),
            db_path.display()
        );
    }

    match cli.command {
        // Configuration management commands
        Commands::Setup { command } => {
            if host.is_some() {
                eprintln!("warning: --host is ignored for 'setup' (local filesystem operation)");
            }
            setup::handle(command, &config_path, &db_path, json).await?;
        },
        Commands::Sync {
            dry_run,
            force,
            detailed,
            check,
            confirmed,
        } => {
            if host.is_some() {
                eprintln!("warning: --host is ignored for 'sync' (local filesystem operation)");
            }
            if dry_run {
                if !json {
                    println!(
                        "{}",
                        "Validating all configuration (dry run)...".bright_cyan()
                    );
                }
                validate_command(detailed, &config_path, &db_path, check, json).await?;
            } else {
                if !confirmed {
                    anyhow::bail!(
                        "configuration apply requires --confirmed; use --dry-run for validation only"
                    );
                }
                if !json {
                    println!("{}", "Syncing all configuration...".bright_cyan());
                }
                sync_command(force, detailed, &config_path, &db_path, check, json).await?;
            }
        },
        Commands::Status { detailed } => {
            if host.is_some() {
                eprintln!("warning: --host is ignored for 'status' (local filesystem operation)");
            }
            if !json {
                println!("{}", "Configuration Status".bright_cyan());
            }
            status_command(detailed, &db_path, json).await?;
        },
        Commands::Init { force } => {
            if host.is_some() {
                eprintln!("warning: --host is ignored for 'init' (local filesystem operation)");
            }
            if !json {
                println!("{}", "Initializing database schema...".bright_cyan());
            }
            init_command(&db_path, force, json).await?;
        },
        Commands::Export { output, detailed } => {
            if host.is_some() {
                eprintln!("warning: --host is ignored for 'export' (local filesystem operation)");
            }
            if !json {
                println!(
                    "{}",
                    "Exporting configuration from database...".bright_cyan()
                );
            }
            export_command(output, detailed, &config_path, &db_path, json).await?;
        },

        // Service management commands (all use HTTP API)
        Commands::Channels { command } => {
            let url = service_url(
                "AETHER_IO_URL",
                "http",
                aether_model::service_ports::IO_PORT,
                host,
            );
            channels::handle_command(command, &url, json).await?;
        },
        Commands::Models { command } => {
            let url = service_url(
                "AETHER_AUTOMATION_URL",
                "http",
                aether_model::service_ports::AUTOMATION_PORT,
                host,
            );
            models::handle_command(command, &url, json).await?;
        },
        Commands::Rules { command } => {
            let url = service_url(
                "AETHER_AUTOMATION_URL",
                "http",
                aether_model::service_ports::AUTOMATION_PORT,
                host,
            );
            rules::handle_command(command, &url, json).await?;
        },
        Commands::Routing { command } => {
            let url = service_url(
                "AETHER_AUTOMATION_URL",
                "http",
                aether_model::service_ports::AUTOMATION_PORT,
                host,
            );
            routing::handle_command(command, &url, json).await?;
        },
        Commands::Services { command } => {
            let mode = deploy_mode::DeployMode::detect(install_mode.as_deref())?;
            if host.is_some() {
                eprintln!(
                    "warning: --host is ignored for 'services' (local operation, {})",
                    if mode == deploy_mode::DeployMode::Systemd {
                        "systemd"
                    } else {
                        "Docker"
                    }
                );
            }
            if json {
                eprintln!("warning: --json is not supported for 'services' command");
            }
            services::handle_command(command, mode).await?;
        },
        Commands::Logs { command } => {
            logs::handle_command(command, json, host).await?;
        },
        Commands::Shm { command } => {
            if json {
                eprintln!("warning: --json is not supported for 'shm' command");
            }
            shm::handle_command(command, &db_path).await?;
        },
        Commands::Doctor { verbose } => {
            let mode = deploy_mode::DeployMode::detect(install_mode.as_deref())?;
            doctor::run_doctor(config_path, db_path, mode, verbose, json).await?;
        },
        Commands::Templates { command } => {
            let url = service_url(
                "AETHER_IO_URL",
                "http",
                aether_model::service_ports::IO_PORT,
                host,
            );
            templates::handle_command(command, &url, json).await?;
        },
        Commands::Alarms { command } => {
            let url = service_url(
                "AETHER_ALARM_URL",
                "http",
                aether_model::service_ports::ALARM_PORT,
                host,
            );
            alarms::handle_command(command, &url, json).await?;
        },
        Commands::Net { command } => {
            let url = service_url(
                "AETHER_UPLINK_URL",
                "http",
                aether_model::service_ports::UPLINK_PORT,
                host,
            );
            net::handle_command(command, &url, json).await?;
        },
        Commands::History { command } => {
            let url = service_url(
                "AETHER_HISTORY_URL",
                "http",
                aether_model::service_ports::HISTORY_PORT,
                host,
            );
            history::handle_command(command, &url, json).await?;
        },
        Commands::Top => {
            let automation_url = service_url(
                "AETHER_AUTOMATION_URL",
                "http",
                aether_model::service_ports::AUTOMATION_PORT,
                host,
            );
            let io_url = service_url(
                "AETHER_IO_URL",
                "http",
                aether_model::service_ports::IO_PORT,
                host,
            );
            top::run_top(&io_url, &automation_url).await?;
        },
        Commands::RuntimeManifest { path } => {
            if host.is_some() {
                eprintln!(
                    "warning: --host is ignored for 'runtime-manifest' (local artifact verification)"
                );
            }
            let manifest = match path {
                Some(path) => aether_runtime_catalog::load_runtime_manifest_file(
                    path,
                    env!("CARGO_PKG_VERSION"),
                )?,
                None => aether_runtime_catalog::load_runtime_manifest_for_current_process(
                    &config_path,
                    env!("CARGO_PKG_VERSION"),
                )?,
            };
            if json {
                println!("{}", manifest.to_pretty_json()?);
            } else {
                println!("Runtime manifest verified: {}", manifest.composition());
                println!("Aether version: {}", manifest.aether_version());
                println!("Target: {}", manifest.target_triple());
                println!("Checksum: {}", manifest.digest());
                println!(
                    "Capabilities: {} | Protocols: {}",
                    manifest.capabilities().len(),
                    manifest.protocols().len()
                );
            }
        },
        Commands::Packs { command } => {
            if host.is_some() {
                eprintln!("warning: --host is ignored for 'packs' (local artifact operation)");
            }
            pack_artifact::handle(command, &config_path, &db_path, json)?;
        },
        Commands::Mcp { allow_write } => {
            if json {
                eprintln!(
                    "warning: --json is ignored for 'mcp' (it always speaks MCP's own JSON-RPC protocol)"
                );
            }
            let urls = mcp::BaseUrls {
                io: service_url(
                    "AETHER_IO_URL",
                    "http",
                    aether_model::service_ports::IO_PORT,
                    host,
                ),
                automation: service_url(
                    "AETHER_AUTOMATION_URL",
                    "http",
                    aether_model::service_ports::AUTOMATION_PORT,
                    host,
                ),
                alarm: service_url(
                    "AETHER_ALARM_URL",
                    "http",
                    aether_model::service_ports::ALARM_PORT,
                    host,
                ),
                uplink: service_url(
                    "AETHER_UPLINK_URL",
                    "http",
                    aether_model::service_ports::UPLINK_PORT,
                    host,
                ),
                history: service_url(
                    "AETHER_HISTORY_URL",
                    "http",
                    aether_model::service_ports::HISTORY_PORT,
                    host,
                ),
            };
            let server = mcp::AetherMcp::from_active_pack_config(&urls, allow_write, &config_path)?;
            use rmcp::ServiceExt;
            let running = server.serve(rmcp::transport::stdio()).await?;
            running.waiting().await?;
        },
    }

    Ok(())
}

async fn sync_command(
    force: bool,
    detailed: bool,
    config_path: &Path,
    db_path: &Path,
    check: bool,
    json: bool,
) -> Result<()> {
    ensure_configuration_owners_stopped().await?;
    let configs = ["global", "aether-io", "aether-automation"];
    let core = AetherCore::readwrite(db_path, config_path, "all").await?;

    // Full replacement is destructive, but it never bypasses validation.
    // Validate every domain before opening the site-level write transaction.
    for config in configs {
        let validation = core
            .validate(config)
            .await
            .with_context(|| format!("Validation error for {config}"))?;
        if !validation.is_valid {
            if !json {
                for error in &validation.errors {
                    eprintln!("   {} {}", "ERROR".red(), error);
                }
            }
            anyhow::bail!(
                "Validation failed for {}: {}",
                config,
                validation.errors.join("; ")
            );
        }
    }

    if !json {
        println!(
            "{} Applying one atomic configuration transaction...",
            "-".bright_cyan()
        );
    }
    let results = core
        .sync_all(force)
        .await
        .context("Atomic configuration apply failed; all changes were rolled back")?;

    let json_results = results
        .iter()
        .map(|(config, result)| {
            serde_json::json!({
                "config": config,
                "items_synced": result.items_synced,
                "items_deleted": result.items_deleted,
                "errors": [],
            })
        })
        .collect::<Vec<_>>();

    if !json {
        for (config, result) in &results {
            println!("{} {}", "OK".green(), config.bright_yellow());
            if detailed {
                println!("     {} items synced", result.items_synced);
                if result.items_deleted > 0 {
                    println!("     {} items deleted", result.items_deleted);
                }
            }
        }
    }

    if check {
        if !json {
            println!();
        }
        if run_db_checks(db_path, json).await? {
            anyhow::bail!("Database consistency check failed after configuration apply");
        }
    }

    if json {
        output::print_success(serde_json::json!({
            "desired_state_atomic": true,
            "configs": json_results,
            "runtime_activation": "on_next_service_start",
        }));
    } else {
        println!(
            "\n{} Desired configuration synced; start services to activate it.",
            "DONE".green()
        );
    }

    Ok(())
}

async fn ensure_configuration_owners_stopped() -> Result<()> {
    let mut running = Vec::new();
    for (service, port) in [
        ("aether-io", aether_model::service_ports::IO_PORT),
        (
            "aether-automation",
            aether_model::service_ports::AUTOMATION_PORT,
        ),
    ] {
        let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        if tokio::time::timeout(
            std::time::Duration::from_millis(300),
            tokio::net::TcpStream::connect(address),
        )
        .await
        .is_ok_and(|result| result.is_ok())
        {
            running.push(service);
        }
    }
    if running.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(
            "offline configuration apply refused while runtime owners are listening: {}; stop them first, then run sync and start them again",
            running.join(", ")
        )
    }
}

async fn validate_command(
    detailed: bool,
    config_path: &Path,
    db_path: &Path,
    check: bool,
    json: bool,
) -> Result<()> {
    let configs = ["global", "aether-io", "aether-automation"];
    let mut all_valid = true;
    let mut nested_validation_error = None;
    let mut json_results = Vec::new();

    if !json {
        println!();
    }

    let core = AetherCore::new(config_path);

    for cfg in configs {
        if !json {
            print!(
                "{} Validating {}... ",
                "-".bright_cyan(),
                cfg.bright_yellow()
            );
        }

        match core.validate(cfg).await {
            Ok(result) => {
                json_results.push(serde_json::json!({
                    "config": cfg,
                    "valid": result.is_valid,
                    "errors": &result.errors,
                    "warnings": &result.warnings,
                }));

                if !json {
                    if result.is_valid {
                        println!("{}", "OK".green());
                        if detailed && !result.warnings.is_empty() {
                            for warning in &result.warnings {
                                println!("   {} {}", "WARN".yellow(), warning);
                            }
                        }
                    } else {
                        println!("{}", "FAIL".red());
                        for error in &result.errors {
                            eprintln!("   {} {}", "ERROR".red(), error);
                        }
                    }
                }
                if !result.is_valid {
                    all_valid = false;
                }
            },
            Err(e) => {
                json_results.push(serde_json::json!({
                    "config": cfg,
                    "valid": false,
                    "errors": [e.to_string()],
                    "warnings": [],
                }));
                if !json {
                    println!("{}", "FAIL".red());
                    eprintln!("   {} {}", "ERROR".red(), e);
                }
                all_valid = false;
            },
        }
    }

    // The top-level validators intentionally stay lightweight. A real apply also
    // reads nested rule, instance, product, and property files, so validate those
    // through the exact same atomic sync path against an isolated database. This
    // keeps `sync --dry-run` truthful without creating or changing the installed
    // runtime database.
    if all_valid {
        let validation_data =
            tempfile::tempdir().context("Failed to create isolated dry-run database directory")?;
        let staged_result = async {
            let validation_core =
                AetherCore::readwrite(validation_data.path(), config_path, "all").await?;
            validation_core.sync_all(false).await
        }
        .await;

        match staged_result {
            Ok(_) => {
                json_results.push(serde_json::json!({
                    "config": "site",
                    "valid": true,
                    "errors": [],
                    "warnings": [],
                }));
                if !json && detailed {
                    println!("{} Nested configuration files", "OK".green());
                }
            },
            Err(error) => {
                let error = format!("{error:#}");
                json_results.push(serde_json::json!({
                    "config": "site",
                    "valid": false,
                    "errors": [&error],
                    "warnings": [],
                }));
                if !json {
                    eprintln!("   {} Nested configuration: {}", "ERROR".red(), error);
                }
                nested_validation_error = Some(error);
                all_valid = false;
            },
        }
    }

    if check {
        if !json {
            println!();
        }
        let check_failed = run_db_checks(db_path, json).await?;
        if check_failed {
            all_valid = false;
        }
    }

    if !all_valid {
        if !json {
            println!("\n{} Validation failed", "ERROR".red());
        }
        if let Some(error) = nested_validation_error {
            anyhow::bail!("Validation failed: {error}");
        }
        anyhow::bail!("Validation failed");
    }

    if json {
        output::print_success(serde_json::json!({
            "configs": json_results,
            "all_valid": true,
        }));
    } else {
        println!("\n{} All configurations valid!", "SUCCESS".green());
    }

    Ok(())
}

async fn status_command(detailed: bool, db_path: &Path, json: bool) -> Result<()> {
    let db_file = db_path.join("aether.db");

    if json {
        if !db_file.exists() {
            output::print_success(serde_json::json!({
                "db_path": db_file.display().to_string(),
                "exists": false,
            }));
            return Ok(());
        }
        match utils::check_database_status(&db_file).await {
            Ok(status) => output::print_success(serde_json::json!({
                "db_path": db_file.display().to_string(),
                "exists": true,
                "initialized": status.initialized,
                "last_sync": status.last_sync,
                "item_count": status.item_count,
            })),
            Err(e) => output::print_success(serde_json::json!({
                "db_path": db_file.display().to_string(),
                "exists": true,
                "initialized": false,
                "error": e.to_string(),
            })),
        }
        return Ok(());
    }

    println!();
    println!("{}", "=".repeat(50).bright_blue());
    println!("{:^50}", "AetherEMS Configuration Status".bright_yellow());
    println!("{}", "=".repeat(50).bright_blue());
    println!();

    print!("{} Database: ", "-".bright_cyan());

    if db_file.exists() {
        match utils::check_database_status(&db_file).await {
            Ok(status) => {
                println!("{} {}", "OK".green(), db_file.display());

                if detailed {
                    let sync_time = status.last_sync.unwrap_or_else(|| "never".to_string());
                    println!(
                        "   {} Last sync: {}",
                        "-".bright_blue(),
                        sync_time.bright_white()
                    );
                    if let Some(count) = status.item_count {
                        println!("   {} Items: {}", "-".bright_blue(), count);
                    }
                }
            },
            Err(_) => {
                println!("{} Not initialized", "WARN".yellow());
                println!("   {} Run 'aether init' first", "HINT".bright_blue());
            },
        }
    } else {
        println!("{} Not found", "ERROR".red());
        println!(
            "   {} Run 'aether init' to create database",
            "HINT".bright_blue()
        );
    }

    println!();
    println!("{}", "=".repeat(50).bright_blue());
    Ok(())
}

async fn init_command(db_path: &Path, force: bool, json: bool) -> Result<()> {
    let db_file = db_path.join("aether.db");

    if !json {
        println!();
    }

    // --force is disabled for safety (migration-only policy)
    if force {
        if !json {
            eprintln!(
                "{} --force is disabled for safety.",
                "WARNING".bright_yellow()
            );
            eprintln!("   Database can only be upgraded, not reset.");
            eprintln!(
                "   If you really need to reset, manually delete: {}",
                db_file.display()
            );
        }
        return Ok(());
    }

    if !json {
        if db_file.exists() {
            println!(
                "{} Database already exists: {}",
                "INFO".bright_cyan(),
                db_file.display()
            );
            println!(
                "{} Running safe schema upgrade (CREATE TABLE IF NOT EXISTS)...",
                "INFO".bright_blue()
            );
        }
        print!(
            "{} Creating database schema in {}... ",
            "-".bright_cyan(),
            db_file.display().to_string().bright_white()
        );
    }

    match schema::init_database(&db_file).await {
        Ok(_) => {
            if json {
                output::print_success(serde_json::json!({
                    "db_path": db_file.display().to_string(),
                }));
            } else {
                println!("{}", "OK".green());
                println!(
                    "\n{} Database initialized: {}",
                    "DONE".green(),
                    db_file.display()
                );
            }
        },
        Err(e) => {
            if !json {
                println!("{}", "FAIL".red());
            }
            anyhow::bail!("Failed to initialize database: {:#}", e);
        },
    }

    Ok(())
}

async fn export_command(
    output: Option<String>,
    detailed: bool,
    config_path: &Path,
    db_path: &Path,
    json: bool,
) -> Result<()> {
    let configs = ["global", "aether-io", "aether-automation"];
    let output_base = output
        .map(PathBuf::from)
        .unwrap_or_else(|| config_path.to_path_buf());

    if !json {
        println!();
    }

    for cfg in configs {
        if !json {
            print!(
                "{} Exporting {}... ",
                "-".bright_cyan(),
                cfg.bright_yellow()
            );
        }

        let config_dir = match cfg {
            "aether-io" => "io",
            "aether-automation" => "automation",
            other => other,
        };
        let output_dir = output_base.join(config_dir);

        if !json && detailed {
            println!();
            println!("   {} Output: {}", "-".bright_blue(), output_dir.display());
        }

        let core = AetherCore::readwrite(db_path, config_path, cfg).await?;

        let output_path = output_dir
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Invalid output path"))?;

        match core.export(cfg, output_path).await {
            Ok(_) => {
                if !json {
                    println!("{}", "OK".green());
                }
            },
            Err(e) => {
                if !json {
                    println!("{}", "FAIL".red());
                }
                anyhow::bail!("Export failed for {}: {}", cfg, e);
            },
        }
    }

    if json {
        output::print_success(serde_json::json!({
            "output_dir": output_base.display().to_string(),
            "configs": configs,
        }));
    } else {
        println!(
            "\n{} Export completed: {}",
            "DONE".green(),
            output_base.display()
        );
    }
    Ok(())
}

/// Run database consistency checks (duplicates, references)
/// Returns true if any errors were found
async fn run_db_checks(db_path: &Path, json: bool) -> Result<bool> {
    if !json {
        println!("{}", "Checking database consistency...".bright_cyan());
    }

    let db_file = db_path.join("aether.db");
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .connect_with(common::bootstrap_database::sqlite_connect_options(
            db_file.to_str().unwrap_or_default(),
        ))
        .await
        .context("Failed to connect to database")?;

    let mut has_errors = false;

    for &(table, id_col) in ALLOWED_DUPLICATE_CHECKS {
        if !json {
            print!("  Checking {} {}s... ", table, id_col.replace("_id", ""));
        }
        has_errors |= check_duplicates(&pool, table, id_col, json).await?;
    }

    for table in ALLOWED_POINT_TABLES {
        if !json {
            print!("  Checking {} table... ", table.replace('_', " "));
        }
        has_errors |= check_point_duplicates(&pool, table, json).await?;
    }

    if !json {
        if has_errors {
            println!("\n{} Database consistency issues found", "ERROR".red());
        } else {
            println!("\n{} Database consistency OK", "OK".green());
        }
    }

    Ok(has_errors)
}

/// Allowed table/column combinations for duplicate checks (SQL injection prevention)
const ALLOWED_DUPLICATE_CHECKS: &[(&str, &str)] = &[
    ("channels", "channel_id"),
    ("instances", "instance_id"),
    ("rules", "id"),
];

async fn check_duplicates(
    pool: &sqlx::SqlitePool,
    table: &str,
    id_column: &str,
    json: bool,
) -> Result<bool> {
    // Validate table/column against allowlist to prevent SQL injection
    if !ALLOWED_DUPLICATE_CHECKS
        .iter()
        .any(|(t, c)| *t == table && *c == id_column)
    {
        anyhow::bail!(
            "Invalid table/column for duplicate check: {}/{}",
            table,
            id_column
        );
    }

    let query = format!(
        "SELECT {}, COUNT(*) as count FROM {} GROUP BY {} HAVING count > 1",
        id_column, table, id_column
    );

    let rows: Vec<(String, i64)> = sqlx::query_as(&query).fetch_all(pool).await?;

    if rows.is_empty() {
        if !json {
            println!("{}", "OK".green());
        }
        Ok(false)
    } else {
        if !json {
            println!("{}", "FAIL".red());
            for (id, count) in rows {
                eprintln!(
                    "    {} {} '{}' appears {} times",
                    "ERROR".red(),
                    id_column,
                    id,
                    count
                );
            }
        }
        Ok(true)
    }
}

/// Allowed tables for point duplicate checks
const ALLOWED_POINT_TABLES: &[&str] = &[
    "telemetry_points",
    "signal_points",
    "control_points",
    "adjustment_points",
];

async fn check_point_duplicates(pool: &sqlx::SqlitePool, table: &str, json: bool) -> Result<bool> {
    // Validate table against allowlist to prevent SQL injection
    if !ALLOWED_POINT_TABLES.contains(&table) {
        anyhow::bail!("Invalid table for point duplicate check: {}", table);
    }

    let query = format!(
        "SELECT channel_id, point_id, COUNT(*) as count FROM {} GROUP BY channel_id, point_id HAVING count > 1",
        table
    );

    let rows: Vec<(i32, i64, i64)> = sqlx::query_as(&query).fetch_all(pool).await?;

    if rows.is_empty() {
        if !json {
            println!("{}", "OK".green());
        }
        Ok(false)
    } else {
        if !json {
            println!("{}", "FAIL".red());
            for (channel_id, point_id, count) in rows {
                eprintln!(
                    "    {} (channel_id={}, point_id={}) appears {} times",
                    "ERROR".red(),
                    channel_id,
                    point_id,
                    count
                );
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod cli_tests {
    use super::Cli;
    use clap::CommandFactory;

    /// clap's own validator. It catches structural mistakes the type checker
    /// cannot — e.g. a positional `bool` field, which derive silently gives
    /// `ArgAction::SetTrue`, panicking in debug and degrading to a zero-value
    /// flag in release. Every command in this CLI is checked here.
    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
