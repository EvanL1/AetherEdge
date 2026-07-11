//! Network Configuration Handlers
//!
//! Provides endpoints for managing host network interface configurations.
//! Targets systemd-networkd based systems (like EdgeLinux/Ubuntu).
//!
//! # Supported Operations
//! - List all network interfaces
//! - Get interface configuration
//! - Update interface configuration (IP, gateway, DNS, VLAN)
//! - Apply configuration changes
//!
//! # Configuration File Location
//! `/etc/systemd/network/*.network`

// Allow unwrap in utoipa ToSchema macro expansion (not user code)
#![allow(clippy::disallowed_methods)]

use axum::{
    extract::{Path, State},
    response::Json,
};
use common::{AppError, SuccessResponse};
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::path::PathBuf;
use utoipa::ToSchema;

use crate::api::routes::AppState;

// ============================================================================
// Configuration Constants
// ============================================================================

/// Default directory for systemd-networkd configuration files
const NETWORKD_CONFIG_DIR: &str = "/etc/systemd/network";

// ============================================================================
// Data Transfer Objects
// ============================================================================

/// Network interface configuration
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NetworkInterfaceConfig {
    /// Interface name (e.g., "eth0", "enp0s3")
    #[schema(example = "eth0")]
    pub name: String,

    /// Whether DHCP is enabled
    #[schema(example = false)]
    pub dhcp: bool,

    /// List of IP addresses with CIDR notation
    #[schema(example = json!(["192.168.1.100/24", "10.0.0.1/24"]))]
    pub addresses: Vec<String>,

    /// Default gateway (optional)
    #[schema(example = "192.168.1.1")]
    pub gateway: Option<String>,

    /// DNS servers
    #[schema(example = json!(["8.8.8.8", "8.8.4.4"]))]
    pub dns: Vec<String>,

    /// VLAN ID (optional, 1-4094)
    #[schema(example = json!(null))]
    pub vlan_id: Option<u16>,

    /// MAC address (read-only, from system)
    #[schema(example = "00:11:22:33:44:55")]
    pub mac_address: Option<String>,

    /// Whether interface is currently up
    #[schema(example = true)]
    pub is_up: Option<bool>,

    /// Configuration file path
    #[schema(example = "/etc/systemd/network/10-eth0.network")]
    pub config_file: Option<String>,
}

/// Request to update network interface configuration
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NetworkConfigUpdateRequest {
    /// Whether DHCP is enabled (if true, addresses/gateway are ignored)
    #[schema(example = false)]
    pub dhcp: Option<bool>,

    /// List of IP addresses with CIDR notation
    #[schema(example = json!(["192.168.1.100/24"]))]
    pub addresses: Option<Vec<String>>,

    /// Default gateway
    #[schema(example = "192.168.1.1")]
    pub gateway: Option<String>,

    /// DNS servers
    #[schema(example = json!(["8.8.8.8"]))]
    pub dns: Option<Vec<String>>,

    /// VLAN ID (1-4094, or null to remove)
    #[schema(example = json!(null))]
    pub vlan_id: Option<u16>,
}

/// Response after updating network configuration
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NetworkConfigUpdateResult {
    /// Whether the update was successful
    pub success: bool,

    /// Interface name
    pub interface: String,

    /// Message describing the result
    pub message: String,

    /// Whether a restart is required to apply changes
    pub restart_required: bool,
}

/// Response for applying network changes
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NetworkApplyResult {
    /// Whether the apply was successful
    pub success: bool,

    /// Message describing the result
    pub message: String,

    /// Interfaces that were affected
    pub affected_interfaces: Vec<String>,
}

/// List of all network interfaces
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NetworkInterfaceList {
    /// Total number of interfaces
    pub total: usize,

    /// List of interface configurations
    pub interfaces: Vec<NetworkInterfaceConfig>,
}

// ============================================================================
// Internal Implementation
// ============================================================================

/// Parse a systemd-networkd .network file
fn parse_network_file(content: &str) -> NetworkInterfaceConfig {
    let mut config = NetworkInterfaceConfig {
        name: String::new(),
        dhcp: false,
        addresses: Vec::new(),
        gateway: None,
        dns: Vec::new(),
        vlan_id: None,
        mac_address: None,
        is_up: None,
        config_file: None,
    };

    let mut current_section = String::new();

    for line in content.lines() {
        let line = line.trim();

        // Skip empty lines and comments
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        // Section header
        if line.starts_with('[') && line.ends_with(']') {
            current_section = line[1..line.len() - 1].to_string();
            continue;
        }

        // Key=Value pair
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            let value = value.trim();

            match current_section.as_str() {
                "Match" => {
                    if key == "Name" {
                        config.name = value.to_string();
                    }
                },
                "Network" => match key {
                    "DHCP" => {
                        config.dhcp = value.eq_ignore_ascii_case("yes")
                            || value.eq_ignore_ascii_case("true")
                            || value.eq_ignore_ascii_case("ipv4");
                    },
                    "Address" => {
                        config.addresses.push(value.to_string());
                    },
                    "Gateway" => {
                        config.gateway = Some(value.to_string());
                    },
                    "DNS" => {
                        config.dns.push(value.to_string());
                    },
                    "VLAN" => {
                        // VLAN reference (e.g., "vlan100")
                        if let Some(id_str) = value.strip_prefix("vlan")
                            && let Ok(id) = id_str.parse::<u16>()
                        {
                            config.vlan_id = Some(id);
                        }
                    },
                    _ => {},
                },
                "Link" => {
                    if key == "MACAddress" {
                        config.mac_address = Some(value.to_string());
                    }
                },
                _ => {},
            }
        }
    }

    config
}

/// Generate systemd-networkd .network file content
fn generate_network_file(config: &NetworkInterfaceConfig) -> String {
    let mut content = String::new();

    // [Match] section
    content.push_str("[Match]\n");
    content.push_str(&format!("Name={}\n", config.name));
    content.push('\n');

    // [Network] section
    content.push_str("[Network]\n");

    if config.dhcp {
        content.push_str("DHCP=yes\n");
    } else {
        // Static configuration
        for addr in &config.addresses {
            content.push_str(&format!("Address={}\n", addr));
        }

        if let Some(gw) = &config.gateway {
            content.push_str(&format!("Gateway={}\n", gw));
        }

        for dns in &config.dns {
            content.push_str(&format!("DNS={}\n", dns));
        }
    }

    // VLAN configuration (if any)
    if let Some(vlan_id) = config.vlan_id {
        content.push_str(&format!("VLAN=vlan{}\n", vlan_id));
    }

    content
}

/// Validate IP address with CIDR notation
fn validate_address(addr: &str) -> Result<(), String> {
    // Split into IP and prefix
    let parts: Vec<&str> = addr.split('/').collect();
    if parts.len() != 2 {
        return Err(format!(
            "Invalid address format '{}': expected IP/prefix (e.g., 192.168.1.1/24)",
            addr
        ));
    }

    // Validate IP
    let ip_str = parts[0];
    if ip_str.parse::<IpAddr>().is_err() {
        return Err(format!("Invalid IP address: {}", ip_str));
    }

    // Validate prefix
    let prefix: u8 = parts[1]
        .parse()
        .map_err(|_| format!("Invalid prefix: {}", parts[1]))?;

    // For IPv4, prefix should be 0-32; for IPv6, 0-128
    // We'll just check a reasonable range
    if prefix > 128 {
        return Err(format!("Invalid prefix length: {}", prefix));
    }

    Ok(())
}

/// Validate gateway address
fn validate_gateway(gw: &str) -> Result<(), String> {
    if gw.parse::<IpAddr>().is_err() {
        return Err(format!("Invalid gateway address: {}", gw));
    }
    Ok(())
}

/// Validate network interface name to prevent path traversal attacks.
///
/// Valid interface names:
/// - Only alphanumeric characters, underscores, and hyphens
/// - Maximum 15 characters (Linux IFNAMSIZ - 1)
/// - Cannot be empty
fn validate_interface_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Interface name cannot be empty".to_string());
    }
    if name.len() > 15 {
        return Err(format!(
            "Interface name '{}' exceeds maximum length of 15 characters",
            name
        ));
    }
    // Only allow alphanumeric, underscore, and hyphen
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!(
            "Interface name '{}' contains invalid characters (only a-z, A-Z, 0-9, _, - allowed)",
            name
        ));
    }
    Ok(())
}

/// Find network configuration files (async version using spawn_blocking for read_dir iterator)
async fn find_network_files() -> std::io::Result<Vec<PathBuf>> {
    tokio::task::spawn_blocking(|| {
        let dir = std::path::Path::new(NETWORKD_CONFIG_DIR);
        if !dir.exists() {
            return Ok(Vec::new());
        }

        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|ext| ext == "network")
            })
            .collect();

        files.sort();
        Ok(files)
    })
    .await
    .map_err(|e| std::io::Error::other(format!("spawn_blocking: {e}")))?
}

/// Get interface status from /sys/class/net
async fn get_interface_status(name: &str) -> Option<bool> {
    let operstate_path = format!("/sys/class/net/{}/operstate", name);
    tokio::fs::read_to_string(operstate_path)
        .await
        .ok()
        .map(|state| state.trim() == "up")
}

/// Get MAC address from /sys/class/net
async fn get_mac_address(name: &str) -> Option<String> {
    let mac_path = format!("/sys/class/net/{}/address", name);
    tokio::fs::read_to_string(mac_path)
        .await
        .ok()
        .map(|mac| mac.trim().to_string())
}

// ============================================================================
// Shared Helpers
// ============================================================================

/// Enrich interface config with runtime status (is_up, mac_address) from sysfs
async fn enrich_runtime_status(config: &mut NetworkInterfaceConfig) {
    if config.name.is_empty() {
        return;
    }
    config.is_up = get_interface_status(&config.name).await;
    if config.mac_address.is_none() {
        config.mac_address = get_mac_address(&config.name).await;
    }
}

/// Load and parse all network config files, enriching each with runtime status
async fn load_all_configs() -> Result<Vec<NetworkInterfaceConfig>, AppError> {
    let files = find_network_files().await.map_err(|e| {
        AppError::internal_error(format!("Failed to read network config directory: {}", e))
    })?;

    let mut interfaces = Vec::new();
    for file_path in files {
        let content = match tokio::fs::read_to_string(&file_path).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("Failed to read {}: {}", file_path.display(), e);
                continue;
            },
        };
        let mut config = parse_network_file(&content);
        config.config_file = Some(file_path.to_string_lossy().to_string());
        enrich_runtime_status(&mut config).await;
        interfaces.push(config);
    }
    Ok(interfaces)
}

/// Find a specific interface config by name, returning file path and config
async fn find_interface_config(name: &str) -> Result<(PathBuf, NetworkInterfaceConfig), AppError> {
    let files = find_network_files().await.map_err(|e| {
        AppError::internal_error(format!("Failed to read network config directory: {}", e))
    })?;

    for file_path in files {
        let content = match tokio::fs::read_to_string(&file_path).await {
            Ok(c) => c,
            Err(_) => continue,
        };
        let mut config = parse_network_file(&content);
        if config.name == name {
            config.config_file = Some(file_path.to_string_lossy().to_string());
            enrich_runtime_status(&mut config).await;
            return Ok((file_path, config));
        }
    }

    Err(AppError::not_found(format!(
        "Network interface '{}' not found",
        name
    )))
}

// ============================================================================
// HTTP Handlers
// ============================================================================

/// List all network interfaces
///
/// Returns all network interfaces with their current configuration.
#[utoipa::path(
    get,
    path = "/api/network/interfaces",
    responses(
        (status = 200, description = "Network interfaces retrieved", body = NetworkInterfaceList)
    ),
    tag = "network"
)]
pub async fn list_network_interfaces(
    State(_state): State<AppState>,
) -> Result<Json<SuccessResponse<NetworkInterfaceList>>, AppError> {
    let interfaces = load_all_configs().await?;

    let result = NetworkInterfaceList {
        total: interfaces.len(),
        interfaces,
    };

    Ok(Json(SuccessResponse::new(result)))
}

/// Get network interface configuration
///
/// Returns the configuration for a specific network interface.
#[utoipa::path(
    get,
    path = "/api/network/interfaces/{name}",
    params(
        ("name" = String, Path, description = "Interface name (e.g., eth0)")
    ),
    responses(
        (status = 200, description = "Interface configuration retrieved", body = NetworkInterfaceConfig),
        (status = 404, description = "Interface not found")
    ),
    tag = "network"
)]
pub async fn get_network_interface(
    State(_state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<SuccessResponse<NetworkInterfaceConfig>>, AppError> {
    validate_interface_name(&name).map_err(AppError::bad_request)?;
    let (_path, config) = find_interface_config(&name).await?;
    Ok(Json(SuccessResponse::new(config)))
}

/// Update network interface configuration
///
/// Updates the configuration for a specific network interface.
/// Changes are written to the configuration file but not immediately applied.
/// Use POST /api/network/apply to apply changes.
#[utoipa::path(
    put,
    path = "/api/network/interfaces/{name}",
    params(
        ("name" = String, Path, description = "Interface name (e.g., eth0)")
    ),
    request_body = NetworkConfigUpdateRequest,
    responses(
        (status = 200, description = "Configuration updated", body = NetworkConfigUpdateResult),
        (status = 400, description = "Invalid configuration"),
        (status = 404, description = "Interface not found")
    ),
    tag = "network"
)]
pub async fn update_network_interface(
    State(_state): State<AppState>,
    Path(name): Path<String>,
    Json(request): Json<NetworkConfigUpdateRequest>,
) -> Result<Json<SuccessResponse<NetworkConfigUpdateResult>>, AppError> {
    validate_interface_name(&name).map_err(AppError::bad_request)?;
    let (file_path, mut config) = find_interface_config(&name).await?;

    // Apply updates
    if let Some(dhcp) = request.dhcp {
        config.dhcp = dhcp;
    }

    if let Some(addresses) = request.addresses {
        // Validate all addresses
        for addr in &addresses {
            validate_address(addr).map_err(AppError::bad_request)?;
        }
        config.addresses = addresses;
    }

    if let Some(gateway) = request.gateway {
        if !gateway.is_empty() {
            validate_gateway(&gateway).map_err(AppError::bad_request)?;
            config.gateway = Some(gateway);
        } else {
            config.gateway = None;
        }
    }

    if let Some(dns) = request.dns {
        // Validate DNS servers
        for server in &dns {
            if server.parse::<IpAddr>().is_err() {
                return Err(AppError::bad_request(format!(
                    "Invalid DNS server: {}",
                    server
                )));
            }
        }
        config.dns = dns;
    }

    if let Some(vlan_id) = request.vlan_id {
        if vlan_id == 0 || vlan_id > 4094 {
            return Err(AppError::bad_request(format!(
                "Invalid VLAN ID: {} (must be 1-4094)",
                vlan_id
            )));
        }
        config.vlan_id = Some(vlan_id);
    }

    // Generate new configuration
    let new_content = generate_network_file(&config);

    // Backup existing file
    let backup_path = file_path.with_extension("network.bak");
    if let Err(e) = tokio::fs::copy(&file_path, &backup_path).await {
        tracing::warn!("Failed to create backup: {}", e);
    }

    // Write new configuration
    tokio::fs::write(&file_path, &new_content)
        .await
        .map_err(|e| AppError::internal_error(format!("Failed to write configuration: {}", e)))?;

    tracing::info!(
        "Updated network configuration for {} at {}",
        name,
        file_path.display()
    );

    let result = NetworkConfigUpdateResult {
        success: true,
        interface: name,
        message: "Configuration updated. Use POST /api/network/apply to apply changes.".to_string(),
        restart_required: true,
    };

    Ok(Json(SuccessResponse::new(result)))
}

/// Apply network configuration changes
///
/// Applies all pending network configuration changes by reloading systemd-networkd.
/// This will briefly interrupt network connectivity.
#[utoipa::path(
    post,
    path = "/api/network/apply",
    responses(
        (status = 200, description = "Changes applied", body = NetworkApplyResult),
        (status = 500, description = "Failed to apply changes")
    ),
    tag = "network"
)]
pub async fn apply_network_changes(
    State(_state): State<AppState>,
) -> Result<Json<SuccessResponse<NetworkApplyResult>>, AppError> {
    // Get list of interfaces before reload
    let files = find_network_files().await.unwrap_or_default();
    let mut affected = Vec::new();
    for f in &files {
        if let Ok(c) = tokio::fs::read_to_string(f).await {
            let name = parse_network_file(&c).name;
            if !name.is_empty() {
                affected.push(name);
            }
        }
    }

    // Reload networkd
    let output = tokio::process::Command::new("networkctl")
        .arg("reload")
        .output()
        .await
        .map_err(|e| AppError::internal_error(format!("Failed to execute networkctl: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AppError::internal_error(format!(
            "networkctl reload failed: {}",
            stderr
        )));
    }

    tracing::info!(
        "Applied network configuration changes, affected interfaces: {:?}",
        affected
    );

    let result = NetworkApplyResult {
        success: true,
        message: "Network configuration reloaded successfully".to_string(),
        affected_interfaces: affected,
    };

    Ok(Json(SuccessResponse::new(result)))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_network_file() {
        let content = r#"
[Match]
Name=eth0

[Network]
Address=192.168.137.2/24
Gateway=192.168.137.1
DNS=8.8.8.8
Address=10.0.0.1/24
"#;

        let config = parse_network_file(content);
        assert_eq!(config.name, "eth0");
        assert!(!config.dhcp);
        assert_eq!(config.addresses.len(), 2);
        assert_eq!(config.addresses[0], "192.168.137.2/24");
        assert_eq!(config.addresses[1], "10.0.0.1/24");
        assert_eq!(config.gateway, Some("192.168.137.1".to_string()));
        assert_eq!(config.dns, vec!["8.8.8.8"]);
    }

    #[test]
    fn test_parse_dhcp_config() {
        let content = r#"
[Match]
Name=eth1

[Network]
DHCP=yes
"#;

        let config = parse_network_file(content);
        assert_eq!(config.name, "eth1");
        assert!(config.dhcp);
        assert!(config.addresses.is_empty());
    }

    #[test]
    fn test_generate_network_file() {
        let config = NetworkInterfaceConfig {
            name: "eth0".to_string(),
            dhcp: false,
            addresses: vec!["192.168.1.100/24".to_string()],
            gateway: Some("192.168.1.1".to_string()),
            dns: vec!["8.8.8.8".to_string()],
            vlan_id: None,
            mac_address: None,
            is_up: None,
            config_file: None,
        };

        let content = generate_network_file(&config);
        assert!(content.contains("[Match]"));
        assert!(content.contains("Name=eth0"));
        assert!(content.contains("[Network]"));
        assert!(content.contains("Address=192.168.1.100/24"));
        assert!(content.contains("Gateway=192.168.1.1"));
        assert!(content.contains("DNS=8.8.8.8"));
    }

    #[test]
    fn test_validate_address() {
        assert!(validate_address("192.168.1.1/24").is_ok());
        assert!(validate_address("10.0.0.1/8").is_ok());
        assert!(validate_address("fe80::1/64").is_ok());

        assert!(validate_address("192.168.1.1").is_err()); // Missing prefix
        assert!(validate_address("invalid/24").is_err()); // Invalid IP
        assert!(validate_address("192.168.1.1/abc").is_err()); // Invalid prefix
    }

    #[test]
    fn test_validate_gateway() {
        assert!(validate_gateway("192.168.1.1").is_ok());
        assert!(validate_gateway("10.0.0.1").is_ok());
        assert!(validate_gateway("fe80::1").is_ok());

        assert!(validate_gateway("invalid").is_err());
        assert!(validate_gateway("192.168.1.1/24").is_err()); // Should not have prefix
    }
}
