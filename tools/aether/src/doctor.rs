//! Health check and diagnostics for AetherEMS system

use anyhow::Result;
use colored::*;
use serde::Serialize;
use std::borrow::Cow;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::utils::check_database_status;

/// Check result status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Ok,
    Warning,
    Error,
}

/// Single check result
#[derive(Debug, Serialize)]
pub struct CheckResult {
    pub name: String,
    pub status: CheckStatus,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

impl CheckResult {
    fn ok(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Ok,
            message: message.into(),
            suggestion: None,
            duration_ms: None,
        }
    }

    fn warning(
        name: impl Into<String>,
        message: impl Into<String>,
        suggestion: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Warning,
            message: message.into(),
            suggestion: Some(suggestion.into()),
            duration_ms: None,
        }
    }

    fn error(
        name: impl Into<String>,
        message: impl Into<String>,
        suggestion: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Error,
            message: message.into(),
            suggestion: Some(suggestion.into()),
            duration_ms: None,
        }
    }

    fn with_duration(mut self, duration: Duration) -> Self {
        self.duration_ms = Some(duration.as_millis() as u64);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServiceHealthState {
    Healthy,
    Degraded,
    Invalid,
}

#[derive(Debug)]
struct ParsedServiceHealth {
    state: ServiceHealthState,
    checks: Option<serde_json::Value>,
}

fn check_status_for_service_health(state: ServiceHealthState) -> CheckStatus {
    match state {
        ServiceHealthState::Healthy => CheckStatus::Ok,
        ServiceHealthState::Degraded => CheckStatus::Warning,
        ServiceHealthState::Invalid => CheckStatus::Error,
    }
}

fn check_status_for_service_response(
    service_name: &str,
    body: &str,
    state: ServiceHealthState,
) -> CheckStatus {
    let status = check_status_for_service_health(state);
    if service_name != "aether-history" || status != CheckStatus::Warning {
        return status;
    }

    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return CheckStatus::Error;
    };
    let data = value.get("data").unwrap_or(&value);
    let enabled = data
        .get("storage_enabled")
        .and_then(serde_json::Value::as_bool);
    let backend = data
        .get("configured_backend")
        .or_else(|| data.get("backend"))
        .and_then(serde_json::Value::as_str)
        .map(|backend| backend.to_ascii_lowercase());

    match (enabled, backend.as_deref()) {
        (Some(false), _) | (_, Some("disabled")) => CheckStatus::Warning,
        (Some(true), Some("postgres" | "timescaledb" | "influxdb")) => CheckStatus::Warning,
        // Embedded SQLite is the default history authority. A failure here is
        // a core runtime failure, and unknown degraded responses fail closed.
        _ => CheckStatus::Error,
    }
}

#[derive(Debug, Clone, Copy)]
struct ServiceCheck {
    name: &'static str,
    port: u16,
    health_path: &'static str,
}

const CORE_SERVICE_CHECKS: [ServiceCheck; 6] = [
    ServiceCheck {
        name: "aether-io",
        port: aether_model::service_ports::IO_PORT,
        health_path: "/health",
    },
    ServiceCheck {
        name: "aether-automation",
        port: aether_model::service_ports::AUTOMATION_PORT,
        health_path: "/health",
    },
    ServiceCheck {
        name: "aether-history",
        port: aether_model::service_ports::HISTORY_PORT,
        health_path: "/hisApi/health",
    },
    ServiceCheck {
        name: "aether-api",
        port: aether_model::service_ports::API_PORT,
        health_path: "/health",
    },
    ServiceCheck {
        name: "aether-uplink",
        port: aether_model::service_ports::UPLINK_PORT,
        health_path: "/netApi/health",
    },
    ServiceCheck {
        name: "aether-alarm",
        port: aether_model::service_ports::ALARM_PORT,
        health_path: "/health",
    },
];

const REQUIRED_CONFIG_FILES: [&str; 4] = [
    "global.yaml",
    "io/io.yaml",
    "automation/automation.yaml",
    "automation/instances.yaml",
];

/// Run all health checks
pub async fn run_doctor(
    config_path: impl AsRef<Path>,
    db_path: impl AsRef<Path>,
    mode: crate::deploy_mode::DeployMode,
    verbose: bool,
    json_output: bool,
) -> Result<()> {
    let config_path = config_path.as_ref();
    let db_path = db_path.as_ref();
    let mut results = Vec::new();

    // Run all checks
    if mode == crate::deploy_mode::DeployMode::Docker {
        results.push(check_docker().await);
    }
    for service in CORE_SERVICE_CHECKS {
        results.push(check_service(service).await);
    }
    results.push(check_database(db_path).await);
    results.push(check_config_files(config_path).await);
    results.push(check_shared_memory().await);

    let healthy = doctor_is_healthy(&results);

    if !healthy {
        if !json_output {
            print_results(&results, verbose);
        }
        let failures = results
            .iter()
            .filter(|result| result.status == CheckStatus::Error)
            .map(|result| format!("{}: {}", result.name, result.message))
            .collect::<Vec<_>>()
            .join("; ");
        anyhow::bail!("Health check failed: {failures}");
    }

    if json_output {
        crate::output::print_success(serde_json::json!({
            "checks": &results,
            "healthy": true,
        }));
    } else {
        print_results(&results, verbose);
    }

    Ok(())
}

fn doctor_is_healthy(results: &[CheckResult]) -> bool {
    !results
        .iter()
        .any(|result| result.status == CheckStatus::Error)
}

#[cfg(test)]
fn doctor_exit_result(healthy: bool) -> Result<()> {
    if healthy {
        Ok(())
    } else {
        anyhow::bail!("Health check failed")
    }
}

/// Check Docker engine status
async fn check_docker() -> CheckResult {
    let start = Instant::now();

    let output = Command::new("docker")
        .args(["info", "--format", "{{.ServerVersion}}"])
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
            CheckResult::ok("Docker Engine", format!("Running (v{})", version))
                .with_duration(start.elapsed())
        },
        Ok(_) => CheckResult::error(
            "Docker Engine",
            "Not running",
            "Start Docker Desktop or run: sudo systemctl start docker",
        )
        .with_duration(start.elapsed()),
        Err(_) => CheckResult::error(
            "Docker Engine",
            "Not installed",
            "Install Docker: https://docs.docker.com/get-docker/",
        )
        .with_duration(start.elapsed()),
    }
}

/// Check service health via HTTP endpoint
async fn check_service(service: ServiceCheck) -> CheckResult {
    let start = Instant::now();
    let url = format!("http://localhost:{}{}", service.port, service.health_path);

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => {
            return CheckResult::error(
                service.name,
                "Failed to create HTTP client",
                "Check system configuration",
            )
            .with_duration(start.elapsed());
        },
    };

    match client.get(&url).send().await {
        Ok(response) => {
            if response.status().is_success() {
                match response.text().await {
                    Ok(body) => {
                        let health = parse_health_body(&body);
                        let extra = service_health_details(service.name, health.checks.as_ref());

                        match check_status_for_service_response(service.name, &body, health.state) {
                            CheckStatus::Ok => CheckResult::ok(
                                service.name,
                                format!("Healthy ({}){}", service.port, extra),
                            )
                            .with_duration(start.elapsed()),
                            CheckStatus::Warning => CheckResult::warning(
                                service.name,
                                format!("Degraded ({}){}", service.port, extra),
                                format!("aether services logs {}", service.name),
                            )
                            .with_duration(start.elapsed()),
                            CheckStatus::Error => CheckResult::error(
                                service.name,
                                format!("Running ({}) but invalid health response", service.port),
                                format!("aether services logs {}", service.name),
                            )
                            .with_duration(start.elapsed()),
                        }
                    },
                    Err(_) => CheckResult::error(
                        service.name,
                        format!(
                            "Running ({}) but health response was unreadable",
                            service.port
                        ),
                        format!("aether services logs {}", service.name),
                    )
                    .with_duration(start.elapsed()),
                }
            } else {
                CheckResult::error(
                    service.name,
                    format!(
                        "Unhealthy ({}) - status {}",
                        service.port,
                        response.status()
                    ),
                    format!("aether services logs {}", service.name),
                )
                .with_duration(start.elapsed())
            }
        },
        Err(e) => {
            let msg = if e.is_connect() {
                "Not running or not reachable"
            } else if e.is_timeout() {
                "Connection timeout"
            } else {
                "Connection failed"
            };
            CheckResult::error(
                service.name,
                msg,
                format!("aether services start {}", service.name),
            )
            .with_duration(start.elapsed())
        },
    }
}

fn parse_health_body(body: &str) -> ParsedServiceHealth {
    let trimmed = body.trim();
    if trimmed.eq_ignore_ascii_case("ok") || trimmed.eq_ignore_ascii_case("pong") {
        return ParsedServiceHealth {
            state: ServiceHealthState::Healthy,
            checks: None,
        };
    }

    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return ParsedServiceHealth {
            state: ServiceHealthState::Invalid,
            checks: None,
        };
    };
    let data = value.get("data").unwrap_or(&value);
    let status = data
        .get("status")
        .or_else(|| value.get("status"))
        .and_then(serde_json::Value::as_str);
    let success = value.get("success").and_then(serde_json::Value::as_bool);
    let checks = data.get("checks").or_else(|| value.get("checks")).cloned();

    let state = match status.map(|status| status.to_ascii_lowercase()) {
        Some(status) if matches!(status.as_str(), "healthy" | "running" | "ok") => {
            ServiceHealthState::Healthy
        },
        Some(_) => ServiceHealthState::Degraded,
        None => match success {
            Some(true) => ServiceHealthState::Healthy,
            Some(false) => ServiceHealthState::Degraded,
            None => ServiceHealthState::Invalid,
        },
    };

    ParsedServiceHealth { state, checks }
}

fn service_health_details(name: &str, checks: Option<&serde_json::Value>) -> String {
    let Some(checks) = checks.and_then(serde_json::Value::as_object) else {
        return String::new();
    };

    match name {
        "aether-io" => checks
            .get("channels")
            .and_then(|channels| channels.get("message"))
            .and_then(serde_json::Value::as_str)
            .map(|message| format!(" - {message}"))
            .unwrap_or_default(),
        "aether-automation" => checks
            .get("instances")
            .and_then(|instances| instances.get("count"))
            .and_then(serde_json::Value::as_i64)
            .map(|count| format!(" - {count} instances"))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// Check SQLite database status
async fn check_database(db_path: &Path) -> CheckResult {
    let start = Instant::now();
    let db_file = db_path.join("aether.db");

    match check_database_status(&db_file).await {
        Ok(status) => {
            if !status.exists {
                CheckResult::error("SQLite Database", "Not found", "aether init && aether sync")
                    .with_duration(start.elapsed())
            } else if !status.initialized {
                CheckResult::error("SQLite Database", "Not initialized", "aether init")
                    .with_duration(start.elapsed())
            } else {
                let sync_info = status
                    .last_sync
                    .map(|t| format!(", synced {}", t))
                    .unwrap_or_default();
                CheckResult::ok("SQLite Database", format!("Initialized{}", sync_info))
                    .with_duration(start.elapsed())
            }
        },
        Err(e) => CheckResult::error(
            "SQLite Database",
            format!("Error: {}", e),
            "Check database file permissions",
        )
        .with_duration(start.elapsed()),
    }
}

/// Check configuration files
async fn check_config_files(config_path: &Path) -> CheckResult {
    let start = Instant::now();

    let mut missing = Vec::new();
    for file in &REQUIRED_CONFIG_FILES {
        if !config_path.join(file).exists() {
            missing.push(*file);
        }
    }

    if missing.is_empty() {
        CheckResult::ok("Config Files", "All present").with_duration(start.elapsed())
    } else if missing.len() == REQUIRED_CONFIG_FILES.len() {
        CheckResult::error(
            "Config Files",
            "No config files found",
            format!("Create config files in: {}", config_path.display()),
        )
        .with_duration(start.elapsed())
    } else {
        CheckResult::error(
            "Config Files",
            format!("Missing: {}", missing.join(", ")),
            "Restore every required config file before applying or starting the site",
        )
        .with_duration(start.elapsed())
    }
}

/// Check shared memory availability
async fn check_shared_memory() -> CheckResult {
    let start = Instant::now();
    let shm_path = aether_rtdb_shm::core::config::default_shm_path();

    check_shared_memory_path(&shm_path).with_duration(start.elapsed())
}

fn check_shared_memory_path(shm_path: &Path) -> CheckResult {
    let metadata = match std::fs::symlink_metadata(shm_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return CheckResult::error(
                "Shared Memory",
                "Authoritative live-state segment is missing",
                format!(
                    "Start aether-io and verify {} is initialized",
                    shm_path.display()
                ),
            );
        },
        Err(error) => {
            return CheckResult::error(
                "Shared Memory",
                format!("Cannot inspect authoritative segment: {error}"),
                "Check /dev/shm and aether-rtdb.shm permissions",
            );
        },
    };

    if !metadata.file_type().is_file() {
        return CheckResult::error(
            "Shared Memory",
            "Authoritative segment is not a regular file",
            "Remove the invalid path and restart aether-io",
        );
    }

    match aether_rtdb_shm::SlotReader::open(shm_path) {
        Ok(reader) => {
            let header = reader.header();
            if !reader.is_writer_alive(3_000) {
                return CheckResult::error(
                    "Shared Memory",
                    format!(
                        "Authoritative segment has a missing or stale writer heartbeat ({})",
                        header.writer_heartbeat
                    ),
                    "Restart aether-io and inspect its SHM writer task",
                );
            }
            let size_mb = metadata.len() as f64 / 1024.0 / 1024.0;
            CheckResult::ok(
                "Shared Memory",
                format!(
                    "Authoritative ({size_mb:.1} MB, {} live slots, generation {})",
                    header.slot_count, header.writer_generation
                ),
            )
        },
        Err(error) => CheckResult::error(
            "Shared Memory",
            format!("Authoritative segment is unreadable or invalid: {error}"),
            "Restart aether-io and inspect its SHM initialization logs",
        ),
    }
}

/// Print results in a nice table format
fn print_results(results: &[CheckResult], verbose: bool) {
    println!();
    println!(
        "{}",
        "┌─────────────────────────────────────────────────────────┐".bright_blue()
    );
    println!(
        "{}",
        "│          AetherEMS System Health Check                 │".bright_blue()
    );
    println!(
        "{}",
        "├─────────────────────────────────────────────────────────┤".bright_blue()
    );

    for result in results {
        let icon = match result.status {
            CheckStatus::Ok => "✓".green(),
            CheckStatus::Warning => "⚠".yellow(),
            CheckStatus::Error => "✗".red(),
        };

        let name = format!("{:<18}", result.name);
        let message: Cow<'_, str> = if verbose {
            if let Some(ms) = result.duration_ms {
                Cow::Owned(format!("{} ({}ms)", result.message, ms))
            } else {
                Cow::Borrowed(result.message.as_str())
            }
        } else {
            Cow::Borrowed(result.message.as_str())
        };

        println!("│ {} {} {:<35} │", icon, name, message);

        if let Some(ref suggestion) = result.suggestion {
            println!("│   {} {:<52} │", "→".cyan(), suggestion.dimmed());
        }
    }

    println!(
        "{}",
        "├─────────────────────────────────────────────────────────┤".bright_blue()
    );

    let ok_count = results
        .iter()
        .filter(|r| r.status == CheckStatus::Ok)
        .count();
    let total = results.len();
    let summary = format!("{}/{} checks passed", ok_count, total);

    let summary_colored = if ok_count == total {
        summary.green()
    } else if ok_count >= total - 1 {
        summary.yellow()
    } else {
        summary.red()
    };

    println!("│ {:<55} │", summary_colored);
    println!(
        "{}",
        "└─────────────────────────────────────────────────────────┘".bright_blue()
    );
    println!();
}

/// The check names run for a given deploy mode, in the same order
/// `run_doctor` executes them. Exists as a pure, testable seam separate
/// from the actual (async, subprocess-invoking) check functions.
#[cfg(test)]
fn check_names_for_mode(mode: crate::deploy_mode::DeployMode) -> Vec<&'static str> {
    let mut names = Vec::new();
    if mode == crate::deploy_mode::DeployMode::Docker {
        names.push("Docker Engine");
    }
    names.extend(CORE_SERVICE_CHECKS.iter().map(|service| service.name));
    names.push("SQLite Database");
    names.push("Config Files");
    names.push("Shared Memory");
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systemd_mode_checks_all_core_services_without_external_stores() {
        let names = check_names_for_mode(crate::deploy_mode::DeployMode::Systemd);
        assert_eq!(
            names,
            [
                "aether-io",
                "aether-automation",
                "aether-history",
                "aether-api",
                "aether-uplink",
                "aether-alarm",
                "SQLite Database",
                "Config Files",
                "Shared Memory",
            ]
        );
        assert!(!names.contains(&"Docker Engine"));
        assert!(!names.contains(&"aether-redis"));
        assert!(!names.contains(&"PostgreSQL"));
    }

    #[test]
    fn docker_mode_adds_engine_check_without_external_stores() {
        let names = check_names_for_mode(crate::deploy_mode::DeployMode::Docker);
        assert_eq!(names.first(), Some(&"Docker Engine"));
        assert_eq!(names.len(), 10);
        assert!(!names.contains(&"aether-redis"));
        assert!(!names.contains(&"PostgreSQL"));
    }

    #[test]
    fn core_services_use_their_real_health_routes() {
        let routes = CORE_SERVICE_CHECKS
            .iter()
            .map(|service| (service.name, service.health_path))
            .collect::<Vec<_>>();

        assert_eq!(
            routes,
            [
                ("aether-io", "/health"),
                ("aether-automation", "/health"),
                ("aether-history", "/hisApi/health"),
                ("aether-api", "/health"),
                ("aether-uplink", "/netApi/health"),
                ("aether-alarm", "/health"),
            ]
        );
    }

    #[tokio::test]
    async fn config_check_accepts_current_nested_layout() {
        let config = tempfile::tempdir().expect("temporary config directory");
        for relative_path in [
            "global.yaml",
            "io/io.yaml",
            "automation/automation.yaml",
            "automation/instances.yaml",
        ] {
            let path = config.path().join(relative_path);
            std::fs::create_dir_all(path.parent().expect("config file parent"))
                .expect("create nested config directory");
            std::fs::write(path, "# doctor test\n").expect("write config fixture");
        }

        let result = check_config_files(config.path()).await;

        assert_eq!(result.status, CheckStatus::Ok);
        assert_eq!(result.message, "All present");
    }

    #[tokio::test]
    async fn config_check_names_missing_current_layout_paths() {
        let config = tempfile::tempdir().expect("temporary config directory");
        let global = config.path().join("global.yaml");
        std::fs::write(global, "# doctor test\n").expect("write config fixture");

        let result = check_config_files(config.path()).await;

        assert_eq!(result.status, CheckStatus::Error);
        assert_eq!(
            result.message,
            "Missing: io/io.yaml, automation/automation.yaml, automation/instances.yaml"
        );
    }

    #[tokio::test]
    async fn an_existing_but_uninitialized_database_fails_the_acceptance_gate() {
        let data = tempfile::tempdir().expect("temporary data directory");
        std::fs::write(data.path().join("aether.db"), []).expect("create empty database file");

        let result = check_database(data.path()).await;

        assert_eq!(result.status, CheckStatus::Error);
        assert_eq!(result.message, "Not initialized");
    }

    #[test]
    fn shared_memory_is_a_required_valid_authority() {
        let directory = tempfile::tempdir().expect("temporary SHM directory");
        let missing = directory.path().join("missing.shm");
        assert_eq!(
            check_shared_memory_path(&missing).status,
            CheckStatus::Error
        );

        let empty = directory.path().join("empty.shm");
        std::fs::write(&empty, []).expect("create empty SHM fixture");
        assert_eq!(check_shared_memory_path(&empty).status, CheckStatus::Error);

        let stale = directory.path().join("stale.shm");
        let stale_writer =
            aether_rtdb_shm::SlotWriter::create(&stale, 8, 2, 7).expect("create stale SHM fixture");
        stale_writer
            .update_heartbeat(aether_rtdb_shm::core::config::timestamp_ms().saturating_sub(10_000));
        drop(stale_writer);
        assert_eq!(check_shared_memory_path(&stale).status, CheckStatus::Error);

        let valid = directory.path().join("valid.shm");
        let writer =
            aether_rtdb_shm::SlotWriter::create(&valid, 8, 2, 7).expect("create valid SHM fixture");
        writer.update_heartbeat(aether_rtdb_shm::core::config::timestamp_ms());
        drop(writer);
        assert_eq!(check_shared_memory_path(&valid).status, CheckStatus::Ok);
    }

    #[cfg(unix)]
    #[test]
    fn shared_memory_symlinks_are_rejected() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("temporary SHM directory");
        let target = directory.path().join("target.shm");
        let link = directory.path().join("link.shm");
        let writer =
            aether_rtdb_shm::SlotWriter::create(&target, 8, 0, 7).expect("create valid SHM target");
        writer.update_heartbeat(aether_rtdb_shm::core::config::timestamp_ms());
        drop(writer);
        symlink(&target, &link).expect("create SHM symlink fixture");

        assert_eq!(check_shared_memory_path(&link).status, CheckStatus::Error);
    }

    #[test]
    fn health_parser_accepts_service_response_variants() {
        let standard = parse_health_body(
            r#"{"success":true,"data":{"status":"healthy","checks":{"sqlite":{}}}}"#,
        );
        assert_eq!(standard.state, ServiceHealthState::Healthy);
        assert!(standard.checks.is_some());

        let alarm = parse_health_body(r#"{"success":true,"message":"Service is running"}"#);
        assert_eq!(alarm.state, ServiceHealthState::Healthy);

        let api = parse_health_body("ok");
        assert_eq!(api.state, ServiceHealthState::Healthy);
    }

    #[test]
    fn health_parser_treats_optional_dependency_outages_as_degraded() {
        let uplink = parse_health_body(
            r#"{"success":false,"message":"MQTT disconnected","data":{"mqtt_connected":false}}"#,
        );
        assert_eq!(uplink.state, ServiceHealthState::Degraded);

        let invalid = parse_health_body("not-json");
        assert_eq!(invalid.state, ServiceHealthState::Invalid);
        assert_eq!(
            check_status_for_service_health(invalid.state),
            CheckStatus::Error
        );
    }

    #[test]
    fn history_health_distinguishes_embedded_failure_from_optional_backends() {
        let sqlite = r#"{"success":false,"data":{"backend":"sqlite","active_backend":"disabled","storage_enabled":true}}"#;
        let postgres = r#"{"success":false,"data":{"backend":"postgres","active_backend":"disabled","storage_enabled":true}}"#;
        let disabled = r#"{"success":false,"data":{"backend":"sqlite","active_backend":"disabled","storage_enabled":false}}"#;

        assert_eq!(
            check_status_for_service_response(
                "aether-history",
                sqlite,
                parse_health_body(sqlite).state,
            ),
            CheckStatus::Error
        );
        assert_eq!(
            check_status_for_service_response(
                "aether-history",
                postgres,
                parse_health_body(postgres).state,
            ),
            CheckStatus::Warning
        );
        assert_eq!(
            check_status_for_service_response(
                "aether-history",
                disabled,
                parse_health_body(disabled).state,
            ),
            CheckStatus::Warning
        );

        let legacy_with_explicit_config = r#"{"success":false,"data":{"backend":"disabled","configured_backend":"sqlite","storage_enabled":true}}"#;
        assert_eq!(
            check_status_for_service_response(
                "aether-history",
                legacy_with_explicit_config,
                parse_health_body(legacy_with_explicit_config).state,
            ),
            CheckStatus::Error
        );
    }

    #[test]
    fn warnings_remain_healthy_but_errors_drive_json_and_exit_failure() {
        let warning_only = vec![CheckResult::warning(
            "optional dependency",
            "offline",
            "configure it when needed",
        )];
        assert!(doctor_is_healthy(&warning_only));
        assert!(doctor_exit_result(doctor_is_healthy(&warning_only)).is_ok());

        let with_error = vec![CheckResult::error(
            "core service",
            "not reachable",
            "start it",
        )];
        assert!(!doctor_is_healthy(&with_error));
        assert!(doctor_exit_result(doctor_is_healthy(&with_error)).is_err());
    }
}
