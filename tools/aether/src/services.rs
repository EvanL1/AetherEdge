//! Service management module for Docker operations
//!
//! Provides functionality to manage AetherEMS services

use anyhow::Result;
use clap::Subcommand;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Subcommand)]
pub enum ServiceCommands {
    /// Start services
    #[command(about = "Start one or more AetherEMS services")]
    Start {
        /// Service names (optional, starts all if not specified)
        services: Vec<String>,
    },

    /// Stop services
    #[command(about = "Stop one or more AetherEMS services")]
    Stop {
        /// Service names (optional, stops all if not specified)
        services: Vec<String>,
    },

    /// Restart services
    #[command(about = "Restart one or more AetherEMS services")]
    Restart {
        /// Service names (optional, restarts all if not specified)
        services: Vec<String>,
    },

    /// Show service status
    #[command(about = "Display status of AetherEMS services")]
    Status {
        /// Service names (optional, shows all if not specified)
        services: Vec<String>,
    },

    /// View service logs
    #[command(about = "View logs for AetherEMS services")]
    Logs {
        /// Service name
        service: String,
        /// Follow log output
        #[arg(short, long)]
        follow: bool,
        /// Number of lines to show from the end
        #[arg(short = 'n', long, default_value = "100")]
        tail: String,
    },

    /// Reload service configurations
    #[command(about = "Reload configurations for services")]
    Reload {
        /// Service names (optional, reloads all if not specified)
        services: Vec<String>,
    },

    /// Build Docker images
    #[command(about = "Build Docker images for services")]
    Build {
        /// Service names (optional, builds all if not specified)
        services: Vec<String>,
    },

    /// Pull Docker images
    #[command(about = "Pull latest Docker images")]
    Pull,

    /// Clean up Docker resources
    #[command(about = "Clean up Docker volumes and networks")]
    Clean {
        /// Also remove volumes (long form only; -v is the global verbose flag)
        #[arg(long)]
        volumes: bool,
    },

    /// Refresh Docker images by recreating containers
    #[command(about = "Force recreate containers with latest images")]
    Refresh {
        /// Service names (optional, refreshes all if not specified)
        services: Vec<String>,
        /// Also pull latest images before recreating
        #[arg(short, long)]
        pull: bool,
        /// Use smart mode (only recreate changed images; extensions stay explicit)
        #[arg(short, long)]
        smart: bool,
    },
}

pub async fn handle_command(
    cmd: ServiceCommands,
    mode: crate::deploy_mode::DeployMode,
) -> Result<()> {
    match cmd {
        ServiceCommands::Start { services } => {
            match mode {
                crate::deploy_mode::DeployMode::Docker => {
                    ensure_logs_dir_exists()?;

                    let mut args = vec!["up".to_string(), "-d".to_string()];

                    // Filter out "all" keyword and add specific service names
                    let filtered_services: Vec<String> = services
                        .into_iter()
                        .filter(|s| s.to_lowercase() != "all")
                        .map(|service| compose_service_target(&service))
                        .collect();

                    args.extend(filtered_services);
                    execute_docker_compose_str(&args)?;
                },
                crate::deploy_mode::DeployMode::Systemd => {
                    execute_systemctl(&build_systemctl_args("start", &services))?;
                },
            }
            println!("Services started");
        },
        ServiceCommands::Stop { services } => {
            match mode {
                crate::deploy_mode::DeployMode::Docker => {
                    let args = build_docker_compose_args("stop", "", services);
                    execute_docker_compose_str(&args)?;
                },
                crate::deploy_mode::DeployMode::Systemd => {
                    execute_systemctl(&build_systemctl_args("stop", &services))?;
                },
            }
            println!("Services stopped");
        },
        ServiceCommands::Restart { services } => {
            match mode {
                crate::deploy_mode::DeployMode::Docker => {
                    ensure_logs_dir_exists()?;

                    let filtered: Vec<String> = services
                        .into_iter()
                        .filter(|s| !s.eq_ignore_ascii_case("all"))
                        .collect();

                    if filtered.is_empty() {
                        // Restart all: dependency-aware order
                        // io writes SHM, automation reads it — io must start first
                        println!("Restarting services in dependency order...");
                        println!("  [1/3] io (SHM writer)");
                        execute_docker_compose(&["restart", "aether-io"])?;
                        println!("  [2/3] automation (SHM reader)");
                        execute_docker_compose(&["restart", "aether-automation"])?;
                        println!("  [3/3] remaining services");
                        execute_docker_compose(&[
                            "restart",
                            "aether-api",
                            "aether-history",
                            "aether-alarm",
                            "aether-uplink",
                        ])?;
                    } else {
                        let args = build_docker_compose_args("restart", "", filtered);
                        execute_docker_compose_str(&args)?;
                    }
                },
                crate::deploy_mode::DeployMode::Systemd => {
                    execute_systemctl(&build_systemctl_args("restart", &services))?;
                },
            }
            println!("Services restarted");
        },
        ServiceCommands::Status { services } => match mode {
            crate::deploy_mode::DeployMode::Docker => {
                let filtered_services: Vec<String> = services
                    .into_iter()
                    .filter(|s| !s.eq_ignore_ascii_case("all"))
                    .map(|service| compose_service_target(&service))
                    .collect();

                let args = if filtered_services.is_empty() {
                    vec!["ps"]
                } else {
                    let mut args = vec!["ps"];
                    for service in &filtered_services {
                        args.push(service.as_str());
                    }
                    args
                };
                execute_docker_compose(&args)?;
            },
            crate::deploy_mode::DeployMode::Systemd => {
                execute_systemctl(&build_systemctl_args("status", &services))?;
            },
        },
        ServiceCommands::Logs {
            service,
            follow,
            tail,
        } => match mode {
            crate::deploy_mode::DeployMode::Docker => {
                let compose_service = compose_service_target(&service);
                let mut args = vec!["logs"];
                if follow {
                    args.push("-f");
                }
                args.push("--tail");
                args.push(&tail);
                args.push(&compose_service);
                execute_docker_compose(&args)?;
            },
            crate::deploy_mode::DeployMode::Systemd => {
                let unit = service.clone();
                let mut args = vec!["-u".to_string(), unit];
                if follow {
                    args.push("-f".to_string());
                }
                if tail != "all" {
                    args.push("-n".to_string());
                    args.push(tail);
                }
                let status = Command::new("journalctl").args(&args).status()?;
                if !status.success() {
                    anyhow::bail!("journalctl failed for {service}");
                }
            },
        },
        ServiceCommands::Reload { services } => {
            let hot_reload_services = vec!["aether-io", "aether-automation"];

            let services_to_reload =
                if services.is_empty() || services.iter().any(|s| s.to_lowercase() == "all") {
                    hot_reload_services.clone()
                } else {
                    services
                        .iter()
                        .filter(|s| s.to_lowercase() != "all")
                        .map(|s| s.as_str())
                        .collect()
                };

            for service in services_to_reload {
                match reload_service(service).await {
                    Ok(()) => println!("Reloaded {} configuration", service),
                    Err(e) => eprintln!("Failed to reload {}: {}", service, e),
                }
            }
        },
        ServiceCommands::Build { services } => match mode {
            crate::deploy_mode::DeployMode::Docker => {
                let args = build_docker_compose_args("build", "", services);
                execute_docker_compose_str(&args)?;
                println!("Images built");
            },
            crate::deploy_mode::DeployMode::Systemd => {
                anyhow::bail!(
                    "'build' has no meaning in systemd mode (no container images in a bare-metal install) — re-run the .run installer to upgrade binaries instead"
                );
            },
        },
        ServiceCommands::Pull => match mode {
            crate::deploy_mode::DeployMode::Docker => {
                execute_docker_compose(&["pull"])?;
                println!("Images pulled");
            },
            crate::deploy_mode::DeployMode::Systemd => {
                anyhow::bail!(
                    "'pull' has no meaning in systemd mode (no container images in a bare-metal install) — re-run the .run installer to upgrade binaries instead"
                );
            },
        },
        ServiceCommands::Clean { volumes } => match mode {
            crate::deploy_mode::DeployMode::Docker => {
                if volumes {
                    execute_docker_compose(&["down", "-v"])?;
                    println!("Services stopped and volumes removed");
                } else {
                    execute_docker_compose(&["down"])?;
                    println!("Services stopped");
                }
            },
            crate::deploy_mode::DeployMode::Systemd => {
                anyhow::bail!(
                    "'clean' has no meaning in systemd mode (no container images in a bare-metal install) — re-run the .run installer to upgrade binaries instead"
                );
            },
        },
        ServiceCommands::Refresh {
            services,
            pull,
            smart,
        } => match mode {
            crate::deploy_mode::DeployMode::Systemd => {
                let _ = pull; // Docker-only concept, unused in this branch
                if smart {
                    eprintln!(
                        "note: --smart has no effect in systemd mode (no container images to diff); performing a plain restart"
                    );
                }
                execute_systemctl(&build_systemctl_args("restart", &services))?;
            },
            crate::deploy_mode::DeployMode::Docker => {
                ensure_logs_dir_exists()?;

                if smart {
                    println!("Refreshing services with smart mode...");
                    let target_services = refresh_targets(&services);
                    let changes = prepare_smart_refresh(
                        &target_services,
                        pull,
                        execute_docker_compose_str,
                        check_container_image_changed,
                    )?;

                    // Stateful extensions are refreshed only when named explicitly.
                    if changes.redis_targeted {
                        if changes.redis_changed {
                            println!("\n⚠️  Redis extension image has changed");
                            println!("   Recreating it will briefly interrupt mirror consumers");
                            println!("\nRecreate Redis extension? (yes/NO): ");

                            use std::io::{Write, stdin, stdout};
                            let mut input = String::new();
                            stdout().flush()?;
                            stdin().read_line(&mut input)?;

                            if input.trim() == "yes" {
                                execute_docker_compose(&[
                                    "up",
                                    "-d",
                                    "--force-recreate",
                                    "aether-redis",
                                ])?;
                                println!("✓ Redis extension recreated");
                            } else {
                                println!("Skipped Redis extension recreation");
                            }
                        } else {
                            println!("✓ Redis extension image unchanged");
                        }
                    }

                    // Handle all Rust services (unified aetherems:latest image)
                    if changes.services_changed {
                        println!("\nRecreating services...");
                        execute_docker_compose(&[
                            "up",
                            "-d",
                            "--force-recreate",
                            "aether-io",
                            "aether-automation",
                            "aether-history",
                            "aether-api",
                            "aether-uplink",
                            "aether-alarm",
                        ])?;
                        println!("✓ Services recreated");
                    } else {
                        println!("✓ Services unchanged (no recreation needed)");
                    }

                    // Handle frontend
                    if changes.frontend_changed {
                        println!("\nRecreating frontend...");
                        execute_docker_compose(&["up", "-d", "--force-recreate", "apps"])?;
                        println!("✓ Frontend recreated");
                    } else if changes.frontend_targeted {
                        println!("✓ Frontend unchanged (no recreation needed)");
                    }

                    if changes.timescaledb_targeted {
                        if changes.timescaledb_changed {
                            println!("\n⚠️  PostgreSQL history extension image has changed");
                            println!("\nRecreate PostgreSQL history extension? (yes/NO): ");

                            use std::io::{Write, stdin, stdout};
                            let mut input = String::new();
                            stdout().flush()?;
                            stdin().read_line(&mut input)?;

                            if input.trim() == "yes" {
                                execute_docker_compose(&[
                                    "up",
                                    "-d",
                                    "--force-recreate",
                                    "timescaledb",
                                ])?;
                                println!("✓ PostgreSQL history extension recreated");
                            } else {
                                println!("Skipped PostgreSQL history extension recreation");
                            }
                        } else {
                            println!("✓ PostgreSQL history extension image unchanged");
                        }
                    }

                    println!("\nSmart refresh completed successfully");
                } else {
                    // Force recreation uses Compose's replacement path. Do not tear down
                    // healthy containers before a requested pull has succeeded.
                    println!("Refreshing services with latest images (force mode)...");

                    let target_services = refresh_targets(&services);
                    let full_refresh = target_services.is_empty();

                    if pull && full_refresh {
                        println!("Pulling latest images for all core services...");
                    } else if pull {
                        println!(
                            "Pulling latest images for selected services: {}",
                            target_services.join(", ")
                        );
                    }

                    force_refresh_services(&target_services, pull, execute_docker_compose_str)?;

                    if full_refresh {
                        println!("Recreated all core containers with latest images");
                    } else {
                        println!(
                            "Recreated selected services with latest images: {}",
                            target_services.join(", ")
                        );
                    }

                    println!("Services refreshed successfully");
                }
            },
        },
    }
    Ok(())
}

/// Reload a single service via its HTTP API.
async fn reload_service(service: &str) -> Result<()> {
    match reload_service_outcome(service).await? {
        ReloadOutcome::Reloaded => Ok(()),
        ReloadOutcome::Unavailable => anyhow::bail!("service is not reachable"),
        ReloadOutcome::RestartRequired(reason) => {
            anyhow::bail!("live reload incomplete; restart required: {reason}")
        },
    }
}

const IO_RELOAD_PATHS: &[&str] = &["/api/channels/reload"];
const AUTOMATION_RELOAD_PATHS: &[&str] = &["/api/instances/reload", "/api/scheduler/reload"];

fn reload_targets(service: &str) -> Result<(u16, &'static [&'static str])> {
    match service {
        "aether-io" => Ok((aether_model::service_ports::IO_PORT, IO_RELOAD_PATHS)),
        "aether-automation" => Ok((
            aether_model::service_ports::AUTOMATION_PORT,
            AUTOMATION_RELOAD_PATHS,
        )),
        _ => anyhow::bail!("{service} does not support hot reload"),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ReloadOutcome {
    Reloaded,
    /// The service was not listening and will load configuration on next start.
    Unavailable,
    /// A running or partially reloaded service may still be using old config.
    RestartRequired(String),
}

async fn reload_service_outcome(service: &str) -> Result<ReloadOutcome> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .no_proxy()
        .build()?;
    let (port, paths) = reload_targets(service)?;
    let mut completed = 0;

    for path in paths {
        let url = format!("http://localhost:{port}{path}");
        match client.post(&url).send().await {
            Ok(response) if response.status().is_success() => completed += 1,
            Ok(response) => {
                return Ok(ReloadOutcome::RestartRequired(format!(
                    "{path} returned HTTP {}",
                    response.status()
                )));
            },
            Err(error) if completed == 0 && error.is_connect() => {
                return Ok(ReloadOutcome::Unavailable);
            },
            Err(error) => {
                return Ok(ReloadOutcome::RestartRequired(format!(
                    "{path} failed after {completed} reload step(s): {error}"
                )));
            },
        }
    }

    Ok(ReloadOutcome::Reloaded)
}

/// Ensure the Compose log target is a real directory without rewriting an
/// operator-owned symlink when external media is unavailable.
fn ensure_logs_dir_exists() -> Result<()> {
    let logs_path = std::env::var("AETHER_LOG_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("logs"));
    ensure_logs_path_exists(&logs_path)
}

fn ensure_logs_path_exists(logs_path: &Path) -> Result<()> {
    match fs::symlink_metadata(logs_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => anyhow::bail!(
            "refusing symlinked Docker log path {}; restore its target or choose AETHER_LOG_PATH explicitly",
            logs_path.display()
        ),
        Ok(metadata) if !metadata.is_dir() => anyhow::bail!(
            "Docker log path is not a directory: {}",
            logs_path.display()
        ),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!("Creating logs directory: {}", logs_path.display());
            fs::create_dir_all(logs_path).map_err(Into::into)
        },
        Err(error) => Err(error.into()),
    }
}

fn build_docker_compose_args(command: &str, flag: &str, services: Vec<String>) -> Vec<String> {
    let mut args = vec![command.to_string()];
    if !flag.is_empty() {
        args.push(flag.to_string());
    }

    let filtered_services: Vec<String> = services
        .into_iter()
        .filter(|s| !s.eq_ignore_ascii_case("all"))
        .map(|service| compose_service_target(&service))
        .collect();

    args.extend(filtered_services);
    args
}

fn compose_service_target(service: &str) -> String {
    if service.eq_ignore_ascii_case("aether-apps") {
        "apps".to_owned()
    } else if service.eq_ignore_ascii_case("aether-timescaledb") {
        "timescaledb".to_owned()
    } else {
        service.to_owned()
    }
}

fn extension_is_targeted(target_services: &[String], extension: &str) -> bool {
    target_services
        .iter()
        .any(|service| service.eq_ignore_ascii_case(extension))
}

const CORE_DOCKER_SERVICES: &[&str] = &[
    "aether-io",
    "aether-automation",
    "aether-history",
    "aether-api",
    "aether-uplink",
    "aether-alarm",
];

#[derive(Debug, PartialEq, Eq)]
struct SmartRefreshChanges {
    redis_targeted: bool,
    redis_changed: bool,
    services_changed: bool,
    frontend_targeted: bool,
    frontend_changed: bool,
    timescaledb_targeted: bool,
    timescaledb_changed: bool,
}

fn refresh_targets(services: &[String]) -> Vec<String> {
    services
        .iter()
        .filter(|service| !service.eq_ignore_ascii_case("all"))
        .map(|service| compose_service_target(service))
        .collect()
}

fn docker_pull_args(target_services: &[String]) -> Vec<String> {
    let mut args = vec!["pull".to_string()];
    args.extend(target_services.iter().cloned());
    args
}

/// Pulls requested images before comparing them with running containers.
///
/// Keeping both actions in one helper makes the safety-critical ordering
/// explicit: a failed pull returns before any change detection or recreation.
fn prepare_smart_refresh<Execute, ImageChanged>(
    target_services: &[String],
    pull: bool,
    mut execute: Execute,
    mut image_changed: ImageChanged,
) -> Result<SmartRefreshChanges>
where
    Execute: FnMut(&[String]) -> Result<()>,
    ImageChanged: FnMut(&str) -> Result<bool>,
{
    if pull {
        println!("Pulling latest images...");
        execute(&docker_pull_args(target_services))?;
    }

    println!("Detecting image changes...");

    // All six core services share aetherems:latest, so one targeted core
    // container is sufficient to compare the running and local image IDs.
    let core_probe = if target_services.is_empty() {
        Some("aether-io")
    } else {
        target_services
            .iter()
            .map(String::as_str)
            .find(|service| CORE_DOCKER_SERVICES.contains(service))
    };
    let services_changed = match core_probe {
        Some(container) => image_changed(container)?,
        None => false,
    };

    let redis_targeted = extension_is_targeted(target_services, "aether-redis");
    let redis_changed = redis_targeted && image_changed("aether-redis")?;

    let frontend_targeted = extension_is_targeted(target_services, "apps");
    let frontend_changed = frontend_targeted && image_changed("aether-apps")?;

    let timescaledb_targeted = extension_is_targeted(target_services, "timescaledb");
    let timescaledb_changed = timescaledb_targeted && image_changed("aether-timescaledb")?;

    Ok(SmartRefreshChanges {
        redis_targeted,
        redis_changed,
        services_changed,
        frontend_targeted,
        frontend_changed,
        timescaledb_targeted,
        timescaledb_changed,
    })
}

/// Pulls before asking Compose to replace containers in place.
///
/// `docker compose up --force-recreate` avoids the destructive `down` and
/// `stop`/`rm` prelude. A registry failure therefore leaves the old runtime
/// untouched, while Compose handles the shortest available replacement path.
fn force_refresh_services<Execute>(
    target_services: &[String],
    pull: bool,
    mut execute: Execute,
) -> Result<()>
where
    Execute: FnMut(&[String]) -> Result<()>,
{
    if pull {
        execute(&docker_pull_args(target_services))?;
    }

    let mut up_args = vec![
        "up".to_string(),
        "-d".to_string(),
        "--force-recreate".to_string(),
    ];
    up_args.extend(target_services.iter().cloned());
    execute(&up_args)
}

/// Maps a systemctl verb + service-name list to unit-file arguments.
/// Empty `services` means "the whole stack" (`aether.target`), matching
/// `build_docker_compose_args`'s existing "empty = all services" convention.
fn build_systemctl_args(verb: &str, services: &[String]) -> Vec<String> {
    let mut args = vec![verb.to_string()];
    if services.is_empty() {
        args.push("aether.target".to_string());
    } else {
        args.extend(services.iter().cloned());
    }
    args
}

fn execute_systemctl(args: &[String]) -> Result<()> {
    let output = Command::new("systemctl").args(args).output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("systemctl {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(())
}

fn execute_docker_compose(args: &[&str]) -> Result<()> {
    let compose_file = if std::path::Path::new("/opt/AetherEdge/docker-compose.yml").exists() {
        "/opt/AetherEdge/docker-compose.yml"
    } else if std::path::Path::new("docker-compose.yml").exists() {
        "docker-compose.yml"
    } else {
        return Err(anyhow::anyhow!(
            "docker-compose.yml not found in /opt/AetherEdge or current directory"
        ));
    };

    // Detect which Docker Compose version is available
    let use_v2 = Command::new("docker")
        .args(["compose", "version"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    let output = if use_v2 {
        let mut full_args = vec!["compose", "-f", compose_file];
        full_args.extend(args);
        Command::new("docker").args(&full_args).output()?
    } else {
        let mut v1_args = vec!["-f", compose_file];
        v1_args.extend(args);
        Command::new("docker-compose").args(&v1_args).output()?
    };

    if !output.status.success() {
        return Err(anyhow::anyhow!(
            "Docker compose command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    print!("{}", String::from_utf8_lossy(&output.stdout));
    Ok(())
}

fn execute_docker_compose_str(args: &[String]) -> Result<()> {
    let str_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    execute_docker_compose(&str_args)
}

/// Check if a container's image has changed compared to the local image
fn check_container_image_changed(container_name: &str) -> Result<bool> {
    let running_image_output = Command::new("docker")
        .args(["inspect", container_name, "--format={{.Image}}"])
        .output();

    let running_image_id = match running_image_output {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).into_owned()
        },
        _ => {
            return Ok(true); // Container doesn't exist, assume needs update
        },
    };

    // Determine the image name from container name
    let image_name = if container_name == "aether-redis" {
        "redis:8-alpine".to_string()
    } else if container_name == "aether-timescaledb" {
        "timescale/timescaledb:2.25.2-pg17".to_string()
    } else if [
        "aether-io",
        "aether-automation",
        "aether-history",
        "aether-api",
        "aether-uplink",
        "aether-alarm",
    ]
    .contains(&container_name)
    {
        "aetherems:latest".to_string()
    } else if container_name == "aether-apps" {
        "aether-apps:latest".to_string()
    } else {
        return Ok(false); // Unknown container, assume no change
    };

    let local_image_output = Command::new("docker")
        .args(["image", "inspect", "--format={{.Id}}", &image_name])
        .output()?;

    if !local_image_output.status.success() {
        return Ok(true); // Image not found locally, assume needs update
    }

    let local_image_id = String::from_utf8_lossy(&local_image_output.stdout).into_owned();

    Ok(!image_ids_match(&running_image_id, &local_image_id))
}

fn normalize_image_id(raw_image_id: &str) -> Option<String> {
    let image_id = raw_image_id
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?
        .to_ascii_lowercase();
    let digest = image_id.strip_prefix("sha256:").unwrap_or(&image_id);

    (!digest.is_empty()).then(|| digest.to_string())
}

fn image_ids_match(running_image_id: &str, local_image_id: &str) -> bool {
    match (
        normalize_image_id(running_image_id),
        normalize_image_id(local_image_id),
    ) {
        (Some(running), Some(local)) => running == local,
        _ => false,
    }
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn docker_image_ids_are_compared_in_one_canonical_form() {
        assert!(image_ids_match(
            " sha256:ABCDEF0123456789\n",
            "abcdef0123456789"
        ));
        assert!(!image_ids_match(
            "sha256:abcdef0123456789",
            "sha256:0123456789abcdef"
        ));
        assert!(!image_ids_match("\n", "sha256:abcdef0123456789"));
    }

    #[test]
    fn smart_refresh_pulls_before_detecting_image_changes() {
        let events = RefCell::new(Vec::new());
        let changes = prepare_smart_refresh(
            &["aether-io".to_string()],
            true,
            |args| {
                events
                    .borrow_mut()
                    .push(format!("compose:{}", args.join(" ")));
                Ok(())
            },
            |container| {
                events.borrow_mut().push(format!("inspect:{container}"));
                Ok(true)
            },
        )
        .unwrap();

        assert!(changes.services_changed);
        assert_eq!(
            events.into_inner(),
            vec!["compose:pull aether-io", "inspect:aether-io"]
        );
    }

    #[test]
    fn smart_refresh_pull_failure_skips_image_detection() {
        let inspected_containers = RefCell::new(Vec::new());
        let result = prepare_smart_refresh(
            &["aether-io".to_string()],
            true,
            |_args| anyhow::bail!("registry unavailable"),
            |container| {
                inspected_containers
                    .borrow_mut()
                    .push(container.to_string());
                Ok(true)
            },
        );

        assert!(result.is_err());
        assert!(inspected_containers.into_inner().is_empty());
    }

    #[test]
    fn force_refresh_pulls_before_non_destructive_recreation() {
        let mut commands = Vec::new();
        force_refresh_services(&["aether-io".to_string()], true, |args| {
            commands.push(args.to_vec());
            Ok(())
        })
        .unwrap();

        assert_eq!(
            commands,
            vec![
                vec!["pull", "aether-io"],
                vec!["up", "-d", "--force-recreate", "aether-io"],
            ]
        );
        assert!(
            commands
                .iter()
                .flatten()
                .all(|arg| !matches!(arg.as_str(), "down" | "stop" | "rm"))
        );
    }

    #[test]
    fn force_refresh_pull_failure_leaves_running_containers_untouched() {
        let mut commands = Vec::new();
        let result = force_refresh_services(&["aether-io".to_string()], true, |args| {
            commands.push(args.to_vec());
            anyhow::bail!("registry unavailable")
        });

        assert!(result.is_err());
        assert_eq!(commands, vec![vec!["pull", "aether-io"]]);
    }

    #[test]
    fn log_path_creation_is_nondestructive() {
        let directory = tempfile::tempdir().expect("create log-path fixture");
        let missing = directory.path().join("new-logs");
        ensure_logs_path_exists(&missing).expect("create missing log directory");
        assert!(missing.is_dir());

        let regular_file = directory.path().join("not-a-directory");
        fs::write(&regular_file, "operator data").expect("write log-path fixture");
        assert!(ensure_logs_path_exists(&regular_file).is_err());
        assert_eq!(
            fs::read_to_string(&regular_file).expect("read preserved fixture"),
            "operator data"
        );
    }

    #[cfg(unix)]
    #[test]
    fn log_path_symlinks_are_rejected_without_replacement() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("create log-symlink fixture");
        let missing_target = directory.path().join("offline-volume");
        let link = directory.path().join("logs");
        symlink(&missing_target, &link).expect("create broken log symlink");

        assert!(ensure_logs_path_exists(&link).is_err());
        assert!(
            fs::symlink_metadata(&link)
                .expect("inspect preserved log symlink")
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn automation_reload_covers_instances_and_the_rule_scheduler() {
        let (_, paths) = reload_targets("aether-automation").unwrap();
        assert_eq!(paths, ["/api/instances/reload", "/api/scheduler/reload"]);
    }

    #[test]
    fn test_build_docker_compose_args_basic() {
        let args = build_docker_compose_args("start", "", vec![]);
        assert_eq!(args, vec!["start"]);
    }

    #[test]
    fn test_build_docker_compose_args_with_flag() {
        let args = build_docker_compose_args("rm", "-f", vec![]);
        assert_eq!(args, vec!["rm", "-f"]);
    }

    #[test]
    fn test_build_docker_compose_args_with_services() {
        let args = build_docker_compose_args(
            "stop",
            "",
            vec!["aether-io".to_string(), "aether-automation".to_string()],
        );
        assert_eq!(args, vec!["stop", "aether-io", "aether-automation"]);
    }

    #[test]
    fn test_build_docker_compose_args_filters_all() {
        let args = build_docker_compose_args("stop", "", vec!["all".to_string()]);
        assert_eq!(args, vec!["stop"]);
    }

    #[test]
    fn test_build_docker_compose_args_filters_all_case_insensitive() {
        let args = build_docker_compose_args(
            "stop",
            "",
            vec![
                "ALL".to_string(),
                "All".to_string(),
                "aether-io".to_string(),
            ],
        );
        assert_eq!(args, vec!["stop", "aether-io"]);
    }

    #[test]
    fn test_service_commands_start_default() {
        let cmd = ServiceCommands::Start { services: vec![] };
        match cmd {
            ServiceCommands::Start { services } => {
                assert!(services.is_empty());
            },
            _ => panic!("Expected Start command"),
        }
    }

    #[test]
    fn optional_extensions_are_never_implicit_refresh_targets() {
        assert!(!extension_is_targeted(&[], "aether-redis"));
        assert!(!extension_is_targeted(&[], "timescaledb"));
        assert!(!extension_is_targeted(&[], "apps"));
        assert!(extension_is_targeted(
            &["aether-redis".to_string()],
            "aether-redis"
        ));
        assert!(extension_is_targeted(
            &["timescaledb".to_string()],
            "timescaledb"
        ));
        assert!(extension_is_targeted(&["apps".to_string()], "apps"));
    }

    #[test]
    fn optional_container_aliases_map_to_compose_service_names() {
        assert_eq!(compose_service_target("aether-apps"), "apps");
        assert_eq!(compose_service_target("apps"), "apps");
        assert_eq!(compose_service_target("aether-timescaledb"), "timescaledb");
        assert_eq!(compose_service_target("aether-io"), "aether-io");
    }

    #[test]
    fn test_service_commands_logs_defaults() {
        let cmd = ServiceCommands::Logs {
            service: "aether-io".to_string(),
            follow: false,
            tail: "100".to_string(),
        };
        match cmd {
            ServiceCommands::Logs {
                service,
                follow,
                tail,
            } => {
                assert_eq!(service, "aether-io");
                assert!(!follow);
                assert_eq!(tail, "100");
            },
            _ => panic!("Expected Logs command"),
        }
    }

    #[test]
    fn test_service_commands_refresh_smart_mode() {
        let cmd = ServiceCommands::Refresh {
            services: vec![],
            pull: true,
            smart: true,
        };
        match cmd {
            ServiceCommands::Refresh {
                services,
                pull,
                smart,
            } => {
                assert!(services.is_empty());
                assert!(pull);
                assert!(smart);
            },
            _ => panic!("Expected Refresh command"),
        }
    }

    #[test]
    fn test_service_commands_clean_with_volumes() {
        let cmd = ServiceCommands::Clean { volumes: true };
        match cmd {
            ServiceCommands::Clean { volumes } => {
                assert!(volumes);
            },
            _ => panic!("Expected Clean command"),
        }
    }

    #[test]
    fn build_systemctl_args_maps_start_to_all_units_via_target() {
        let args = build_systemctl_args("start", &[]);
        assert_eq!(args, vec!["start", "aether.target"]);
    }

    #[test]
    fn build_systemctl_args_maps_named_services_to_unit_names() {
        let args = build_systemctl_args(
            "restart",
            &["aether-io".to_string(), "aether-automation".to_string()],
        );
        assert_eq!(args, vec!["restart", "aether-io", "aether-automation"]);
    }

    #[test]
    fn build_systemctl_args_status_defaults_to_target() {
        let args = build_systemctl_args("status", &[]);
        assert_eq!(args, vec!["status", "aether.target"]);
    }
}
