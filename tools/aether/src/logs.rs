//! Log management commands for Aether CLI
//!
//! Provides commands for dynamically adjusting log levels in running services,
//! and viewing/tailing service log files on disk.

use anyhow::{Context, Result};
use clap::Subcommand;
use colored::*;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Log management commands
#[derive(Subcommand, Debug)]
pub enum LogCommands {
    /// Set log level for a service
    #[command(about = "Set log level for a service (debug, info, warn, error, trace)")]
    Level {
        /// Service name (io, automation, all)
        service: String,

        /// Log level (trace, debug, info, warn, error)
        /// or full filter spec (e.g., "info,io=debug")
        level: String,
    },

    /// Get current log level for a service
    #[command(about = "Get current log level for a service")]
    Get {
        /// Service name (io, automation, all)
        service: String,
    },

    /// List available log files
    #[command(about = "List log files on disk (default: today)")]
    List {
        /// Service name filter (optional; omit for all services)
        service: Option<String>,

        /// Date in YYYYMMDD format (default: today)
        #[arg(short, long)]
        date: Option<String>,
    },

    /// View last N lines of a service log file
    #[command(about = "View recent lines from a service log file")]
    View {
        /// Service name (io, automation, history, uplink, alarm, api)
        service: String,

        /// Number of lines from end (default: 50)
        #[arg(short = 'n', long, default_value = "50")]
        lines: usize,

        /// Show API access log instead of main log
        #[arg(long)]
        api: bool,

        /// Filter lines containing this pattern (case-insensitive)
        #[arg(short, long)]
        grep: Option<String>,
    },

    /// Follow a log file in real-time (Ctrl+C to stop)
    #[command(about = "Tail a service log file in real-time")]
    Tail {
        /// Service name
        service: String,

        /// Show API access log instead of main log
        #[arg(long)]
        api: bool,

        /// Filter lines containing this pattern (case-insensitive)
        #[arg(short, long)]
        grep: Option<String>,
    },

    /// Interactive TUI log viewer (scrollable, searchable)
    #[command(about = "Open interactive log viewer with scroll, search, and follow")]
    Ui {
        /// Service name
        service: String,

        /// Show API access log instead of main log
        #[arg(long)]
        api: bool,
    },
}

/// Response from log level API
#[derive(Debug, Serialize, Deserialize)]
struct LogLevelResponse {
    level: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// Request to set log level
#[derive(Debug, Serialize)]
struct SetLogLevelRequest {
    level: String,
}

/// Get service port by name
fn get_service_port(service: &str) -> Result<u16> {
    aether_model::service_ports::default_port_for(&service.to_lowercase()).ok_or_else(|| {
        anyhow::anyhow!(
            "Unknown service: {}. Use 'aether-io', 'aether-automation', or 'all'",
            service
        )
    })
}

/// Set log level for a service
async fn set_log_level(service: &str, level: &str, host: Option<&str>) -> Result<()> {
    let port = get_service_port(service)?;
    let addr = host.unwrap_or("127.0.0.1");
    let url = format!("http://{addr}:{port}/api/admin/logs/level");

    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .json(&SetLogLevelRequest {
            level: level.to_string(),
        })
        .send()
        .await
        .with_context(|| format!("Failed to connect to {} at port {}", service, port))?;

    if resp.status().is_success() {
        let body: LogLevelResponse = resp.json().await?;
        println!(
            "  {} {} → {}",
            "✓".green(),
            service.bright_cyan(),
            body.level.bright_yellow()
        );
        Ok(())
    } else {
        let body: LogLevelResponse = resp.json().await?;
        let error_msg = body.error.unwrap_or_else(|| "Unknown error".to_string());
        anyhow::bail!("{}: {}", service, error_msg)
    }
}

/// Get log level for a service
async fn get_log_level(service: &str, host: Option<&str>) -> Result<String> {
    let port = get_service_port(service)?;
    let addr = host.unwrap_or("127.0.0.1");
    let url = format!("http://{addr}:{port}/api/admin/logs/level");

    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("Failed to connect to {} at port {}", service, port))?;

    if resp.status().is_success() {
        let body: LogLevelResponse = resp.json().await?;
        Ok(body.level)
    } else {
        anyhow::bail!("Failed to get log level from {}", service)
    }
}

// ── Remote log helpers (HTTP API) ────────────────────────────────────

/// Minimal percent-encoding for query parameter values.
fn encode_query(s: &str) -> String {
    s.replace('%', "%25")
        .replace(' ', "%20")
        .replace('&', "%26")
        .replace('=', "%3D")
        .replace('#', "%23")
        .replace('+', "%2B")
}

/// Build admin API base URL for a given host.
/// Uses aether-io (port 6001) as the default endpoint since all services
/// share the same /app/logs/ directory.
fn admin_logs_url(host: &str) -> String {
    let port = aether_model::service_ports::default_port_for("aether-io").unwrap_or(6001);
    format!("http://{host}:{port}/api/admin/logs")
}

/// Remote list: GET /api/admin/logs/files
async fn remote_list(
    host: &str,
    service: &Option<String>,
    date: &Option<String>,
    json: bool,
) -> Result<()> {
    let base = admin_logs_url(host);
    let mut url = format!("{base}/files?");
    if let Some(d) = date {
        url.push_str(&format!("date={d}&"));
    }
    if let Some(s) = service {
        url.push_str(&format!("service={s}&"));
    }

    let resp: serde_json::Value = reqwest::get(&url)
        .await
        .with_context(|| format!("Failed to connect to {host}"))?
        .json()
        .await?;

    if let Some(err) = resp.get("error") {
        anyhow::bail!("{}", err.as_str().unwrap_or("Unknown error"));
    }

    if json {
        crate::output::print_success(&resp["files"]);
        return Ok(());
    }

    let empty = vec![];
    let files = resp["files"].as_array().unwrap_or(&empty);
    if files.is_empty() {
        println!(
            "  {} No log files found on {}",
            "•".dimmed(),
            host.bright_cyan()
        );
        return Ok(());
    }

    println!("{} ({}):", "Log files".bright_cyan(), host.dimmed());
    for f in files {
        let name = f["name"].as_str().unwrap_or("?");
        let size = f["size"].as_u64().unwrap_or(0);
        println!(
            "  {} {:>10}",
            name.bright_white(),
            format_size(size).dimmed()
        );
    }
    Ok(())
}

/// Remote view: GET /api/admin/logs/view
async fn remote_view(
    host: &str,
    service: &str,
    lines: usize,
    api: bool,
    grep: &Option<String>,
    json: bool,
) -> Result<()> {
    let today = chrono::Local::now().format("%Y%m%d").to_string();
    let file_name = if api {
        format!("{today}_{service}_api.log")
    } else {
        format!("{today}_{service}.log")
    };

    let base = admin_logs_url(host);
    let mut url = format!("{base}/view?file={file_name}&lines={lines}");
    if let Some(g) = grep {
        url.push_str(&format!("&grep={}", encode_query(g)));
    }

    let resp: serde_json::Value = reqwest::get(&url)
        .await
        .with_context(|| format!("Failed to connect to {host}"))?
        .json()
        .await?;

    if let Some(err) = resp.get("error") {
        anyhow::bail!("{}", err.as_str().unwrap_or("Unknown error"));
    }

    let empty_lines = vec![];
    let lines_arr = resp["lines"].as_array().unwrap_or(&empty_lines);

    if json {
        crate::output::print_success(&resp);
        return Ok(());
    }

    println!(
        "{} {} on {} (last {} lines):",
        "Viewing".bright_cyan(),
        file_name.bright_white(),
        host.dimmed(),
        lines_arr.len(),
    );
    println!();
    for line in lines_arr {
        let s = line.as_str().unwrap_or("");
        print_colored_line(s);
    }
    Ok(())
}

/// Remote tail: poll /api/admin/logs/view every second for new lines.
async fn remote_tail(host: &str, service: &str, api: bool, grep: &Option<String>) -> Result<()> {
    let today = chrono::Local::now().format("%Y%m%d").to_string();
    let file_name = if api {
        format!("{today}_{service}_api.log")
    } else {
        format!("{today}_{service}.log")
    };

    println!(
        "{} {} on {} (Ctrl+C to stop)",
        "Tailing".bright_cyan(),
        file_name.bright_white(),
        host.dimmed(),
    );

    let base = admin_logs_url(host);
    let client = reqwest::Client::new();
    let mut seen_total: usize = 0;

    // Get initial total line count (don't print, just record offset)
    let init_url = format!("{base}/view?file={file_name}&lines=0");
    if let Ok(resp) = client.get(&init_url).send().await
        && let Ok(body) = resp.json::<serde_json::Value>().await
    {
        seen_total = body["total"].as_u64().unwrap_or(0) as usize;
    }

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!();
                break;
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {
                // Fetch latest lines
                let mut url = format!("{base}/view?file={file_name}&lines=1000");
                if let Some(g) = grep {
                    url.push_str(&format!("&grep={}", encode_query(g)));
                }
                let Ok(resp) = client.get(&url).send().await else { continue };
                let Ok(body) = resp.json::<serde_json::Value>().await else { continue };

                let total = body["total"].as_u64().unwrap_or(0) as usize;
                if total > seen_total {
                    let new_count = total - seen_total;
                    let empty = vec![];
                    let lines_arr = body["lines"].as_array().unwrap_or(&empty);
                    // Show only the new lines (tail of the returned array)
                    let skip = lines_arr.len().saturating_sub(new_count);
                    for line in &lines_arr[skip..] {
                        let s = line.as_str().unwrap_or("");
                        print_colored_line(s);
                    }
                    seen_total = total;
                }
            }
        }
    }
    Ok(())
}

// ── File-based log helpers ──────────────────────────────────────────

/// Resolve the host-side log directory (read-only, does not create).
/// Fallback: AETHER_LOG_PATH env → /opt/AetherEdge/logs → ./logs
fn resolve_log_dir() -> PathBuf {
    std::env::var("AETHER_LOG_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let prod = PathBuf::from("/opt/AetherEdge/logs");
            if prod.exists() {
                prod
            } else {
                PathBuf::from("logs")
            }
        })
}

/// Find today's log file for a service.
/// Pattern: `{YYYYMMDD}_{service}[_api].log`
fn find_log_file(log_dir: &Path, service: &str, api: bool) -> Result<PathBuf> {
    let today = chrono::Local::now().format("%Y%m%d").to_string();
    find_log_file_for_date(log_dir, service, api, &today)
}

/// Find a log file for a specific date (YYYYMMDD).
fn find_log_file_for_date(log_dir: &Path, service: &str, api: bool, date: &str) -> Result<PathBuf> {
    let stem = if api {
        format!("{}_{}_api", date, service)
    } else {
        format!("{}_{}", date, service)
    };
    let candidate = log_dir.join(format!("{stem}.log"));
    if candidate.exists() {
        return Ok(candidate);
    }
    // Check size-rotated variants (.1, .2, ...)
    for i in 1..=9 {
        let rotated = log_dir.join(format!("{stem}.log.{i}"));
        if rotated.exists() {
            return Ok(rotated);
        }
    }
    anyhow::bail!(
        "No log file found: {stem}.log (looked in {})",
        log_dir.display()
    )
}

/// Print a line with optional level-based coloring.
fn print_colored_line(line: &str) {
    if line.contains("[ERROR]") || line.contains(" ERROR ") {
        println!("{}", line.red());
    } else if line.contains("[WARN]") || line.contains(" WARN ") {
        println!("{}", line.yellow());
    } else {
        println!("{}", line);
    }
}

/// Check if a line matches an optional grep pattern (case-insensitive).
fn matches_grep(line: &str, grep: &Option<String>) -> bool {
    match grep {
        Some(pattern) => line.to_lowercase().contains(&pattern.to_lowercase()),
        None => true,
    }
}

/// Format file size for human display.
fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// List log files matching optional service/date filter.
fn handle_list(
    log_dir: &Path,
    service: &Option<String>,
    date: &Option<String>,
    json: bool,
) -> Result<()> {
    let date_prefix = date.as_deref().map_or_else(
        || chrono::Local::now().format("%Y%m%d").to_string(),
        |d| d.to_string(),
    );

    if !log_dir.exists() {
        anyhow::bail!("Log directory not found: {}", log_dir.display());
    }

    let mut entries: Vec<(String, u64)> = Vec::new();

    for entry in std::fs::read_dir(log_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();

        // Must start with date prefix and end with .log (or .log.N)
        if !name.starts_with(&date_prefix) || !name.contains(".log") {
            continue;
        }
        // Skip compressed files
        if name.ends_with(".gz") {
            continue;
        }

        // Optional service filter
        if let Some(svc) = service {
            // Pattern: {date}_{service}... so check after the date prefix + underscore
            let after_date = &name[date_prefix.len()..];
            if !after_date.starts_with(&format!("_{svc}")) {
                continue;
            }
        }

        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        entries.push((name, size));
    }

    entries.sort_by(|a, b| a.0.cmp(&b.0));

    if json {
        let items: Vec<_> = entries
            .iter()
            .map(|(name, size)| serde_json::json!({"file": name, "size": size}))
            .collect();
        crate::output::print_success(&items);
        return Ok(());
    }

    if entries.is_empty() {
        println!(
            "  {} No log files for {} in {}",
            "•".dimmed(),
            date_prefix.bright_cyan(),
            log_dir.display()
        );
        return Ok(());
    }

    println!(
        "{} ({}):",
        "Log files".bright_cyan(),
        log_dir.display().to_string().dimmed()
    );
    for (name, size) in &entries {
        println!(
            "  {} {:>10}",
            name.bright_white(),
            format_size(*size).dimmed()
        );
    }

    Ok(())
}

/// View last N lines of a log file with optional grep filter.
fn handle_view(
    log_dir: &Path,
    service: &str,
    lines: usize,
    api: bool,
    grep: &Option<String>,
    json: bool,
) -> Result<()> {
    let path = find_log_file(log_dir, service, api)?;
    let file =
        std::fs::File::open(&path).with_context(|| format!("Cannot open {}", path.display()))?;
    let reader = BufReader::new(file);

    let all_lines: Vec<String> = reader
        .lines()
        .map_while(Result::ok)
        .filter(|l| matches_grep(l, grep))
        .collect();

    let start = all_lines.len().saturating_sub(lines);
    let tail = &all_lines[start..];

    if json {
        crate::output::print_success(serde_json::json!({
            "file": path.display().to_string(),
            "lines": tail,
        }));
        return Ok(());
    }

    println!(
        "{} {} (last {} lines):",
        "Viewing".bright_cyan(),
        path.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .bright_white(),
        tail.len(),
    );
    println!();
    for line in tail {
        print_colored_line(line);
    }

    Ok(())
}

/// Tail a log file in real-time (poll every 200ms, Ctrl+C to stop).
async fn handle_tail(
    log_dir: &Path,
    service: &str,
    api: bool,
    grep: &Option<String>,
) -> Result<()> {
    let path = find_log_file(log_dir, service, api)?;
    let mut file =
        std::fs::File::open(&path).with_context(|| format!("Cannot open {}", path.display()))?;

    // Seek to end — only show new content
    file.seek(SeekFrom::End(0))?;

    println!(
        "{} {} (Ctrl+C to stop)",
        "Tailing".bright_cyan(),
        path.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .bright_white(),
    );

    let mut buf = String::new();
    let mut reader = BufReader::new(file);

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!();
                break;
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(200)) => {
                buf.clear();
                while reader.read_line(&mut buf)? > 0 {
                    // read_line appends, so we process the last line added
                    let line = buf.trim_end_matches('\n').trim_end_matches('\r');
                    if matches_grep(line, grep) {
                        print_colored_line(line);
                    }
                    buf.clear();
                }
            }
        }
    }

    Ok(())
}

/// Handle log commands
pub async fn handle_command(command: LogCommands, json: bool, host: Option<&str>) -> Result<()> {
    match command {
        LogCommands::Level { service, level } => {
            if !json {
                println!("{}", "Setting log level...".bright_cyan());
            }

            if service.to_lowercase() == "all" {
                let services = ["aether-io", "aether-automation"];
                let mut errors = Vec::new();

                for svc in services {
                    if let Err(e) = set_log_level(svc, &level, host).await {
                        errors.push(format!("{}: {}", svc, e));
                    }
                }

                if !errors.is_empty() {
                    if !json {
                        println!();
                        for err in &errors {
                            println!("  {} {}", "✗".red(), err);
                        }
                    }
                    if errors.len() == services.len() {
                        anyhow::bail!("Failed to set log level for all services");
                    }
                }
            } else {
                set_log_level(&service, &level, host).await?;
            }

            if json {
                crate::output::print_success(serde_json::json!({
                    "service": service,
                    "level": level,
                }));
            } else {
                println!();
                println!("{}", "Log level updated successfully!".green());
            }
        },

        LogCommands::Get { service } => {
            let mut results = Vec::new();

            let services: Vec<&str> = if service.to_lowercase() == "all" {
                vec!["aether-io", "aether-automation"]
            } else {
                vec![service.as_str()]
            };

            if !json {
                println!("{}", "Current log levels:".bright_cyan());
            }

            for svc in &services {
                match get_log_level(svc, host).await {
                    Ok(level) => {
                        results.push(serde_json::json!({"service": svc, "level": level}));
                        if !json {
                            println!("  {} {}", svc.bright_cyan(), level.bright_yellow());
                        }
                    },
                    Err(e) => {
                        results.push(serde_json::json!({
                            "service": svc, "level": null, "error": e.to_string()
                        }));
                        if !json {
                            println!("  {} {} ({})", svc.bright_cyan(), "unavailable".red(), e);
                        }
                    },
                }
            }

            if json {
                crate::output::print_success(&results);
            }
        },

        LogCommands::List { service, date } => {
            if let Some(h) = host {
                remote_list(h, &service, &date, json).await?;
            } else {
                let log_dir = resolve_log_dir();
                handle_list(&log_dir, &service, &date, json)?;
            }
        },

        LogCommands::View {
            service,
            lines,
            api,
            grep,
        } => {
            if let Some(h) = host {
                remote_view(h, &service, lines, api, &grep, json).await?;
            } else {
                let log_dir = resolve_log_dir();
                handle_view(&log_dir, &service, lines, api, &grep, json)?;
            }
        },

        LogCommands::Tail { service, api, grep } => {
            if let Some(h) = host {
                remote_tail(h, &service, api, &grep).await?;
            } else {
                let log_dir = resolve_log_dir();
                handle_tail(&log_dir, &service, api, &grep).await?;
            }
        },

        LogCommands::Ui { service, api } => {
            if host.is_some() {
                anyhow::bail!(
                    "Interactive UI is not supported for remote hosts. Use 'aether logs view {} --host ...' or 'aether logs tail {} --host ...' instead.",
                    service,
                    service
                );
            }
            let log_dir = resolve_log_dir();
            let path = find_log_file(&log_dir, &service, api)?;
            crate::logs_tui::run_log_viewer(&path)?;
        },
    }

    Ok(())
}
