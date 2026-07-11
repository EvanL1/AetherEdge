use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::json;
use tracing::error;

use crate::models::NetworkConfig;
use crate::routes_auth::require_admin;
use crate::state::AppState;

// ── LAN file mapping ──────────────────────────────────────────────────────────

fn lan_file(config_dir: &str, lan: u8) -> Option<PathBuf> {
    let file = match lan {
        1 => "10-eth0.network",
        2 => "11-eth1.network",
        3 => "12-eth2.network",
        4 => "13-eth3.network",
        _ => return None,
    };
    Some(Path::new(config_dir).join(file))
}

// ── Parsing ───────────────────────────────────────────────────────────────────

fn cidr_to_mask(cidr: u8) -> String {
    if cidr == 0 {
        return "0.0.0.0".to_string();
    }
    let mask: u32 = if cidr >= 32 {
        0xFFFF_FFFF
    } else {
        0xFFFF_FFFF << (32 - cidr)
    };
    let [a, b, c, d] = mask.to_be_bytes();
    format!("{}.{}.{}.{}", a, b, c, d)
}

fn parse_config(content: &str) -> NetworkConfig {
    #[derive(Default, Clone)]
    struct Fields {
        dhcp: Option<bool>,
        ip: String,
        subnet_mask: String,
        gateway: String,
        dns: Vec<String>,
    }

    let mut active = Fields::default();
    let mut commented = Fields::default();

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }

        let is_commented = line.starts_with('#');
        let parsed = if is_commented {
            line.trim_start_matches('#').trim()
        } else {
            line
        };

        let Some((key, value)) = parsed.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();

        let target = if is_commented {
            &mut commented
        } else {
            &mut active
        };

        match key {
            "DHCP" => target.dhcp = Some(value.eq_ignore_ascii_case("yes")),
            "Address" => {
                if let Some((ip, cidr)) = value.rsplit_once('/') {
                    target.ip = ip.trim().to_string();
                    target.subnet_mask = cidr
                        .trim()
                        .parse::<u8>()
                        .map(cidr_to_mask)
                        .unwrap_or_default();
                } else {
                    target.ip = value.to_string();
                }
            },
            "Gateway" => target.gateway = value.to_string(),
            "DNS" => target.dns.push(value.to_string()),
            _ => {},
        }
    }

    let dns1_active = active.dns.first().cloned().unwrap_or_default();
    let dns2_active = active.dns.get(1).cloned().unwrap_or_default();
    let dns1_commented = commented.dns.first().cloned().unwrap_or_default();
    let dns2_commented = commented.dns.get(1).cloned().unwrap_or_default();

    NetworkConfig {
        dhcp: active.dhcp.unwrap_or(false),
        ip: if active.ip.is_empty() {
            commented.ip
        } else {
            active.ip
        },
        subnet_mask: if active.subnet_mask.is_empty() {
            commented.subnet_mask
        } else {
            active.subnet_mask
        },
        gateway: if active.gateway.is_empty() {
            commented.gateway
        } else {
            active.gateway
        },
        dns1: if dns1_active.is_empty() {
            dns1_commented
        } else {
            dns1_active
        },
        dns2: if dns2_active.is_empty() {
            dns2_commented
        } else {
            dns2_active
        },
    }
}

// ── Query params ──────────────────────────────────────────────────────────────

#[derive(Deserialize, utoipa::IntoParams)]
pub struct LanQuery {
    /// LAN port number (1–4)
    lan: u8,
}

// ── GET /api/v1/network ───────────────────────────────────────────────────────

/// Retrieve the network interface configuration (LAN/WAN).
///
/// Parses the current systemd-networkd configuration and returns a structured
/// result: interface name, IP address, subnet mask, gateway, DNS servers, and
/// DHCP mode. Filter by `lan` query parameter to inspect a specific port.
/// **Read-only** — remote mutation is intentionally unavailable.
#[utoipa::path(get, path = "/api/v1/network", tag = "Network",
    security(("bearer_auth" = [])),
    params(LanQuery),
    responses((status = 200, description = "Network configuration", body = NetworkConfig), (status = 400, description = "Invalid LAN number")))]
pub async fn get_network_config(
    State(state): State<Arc<AppState>>,
    Query(q): Query<LanQuery>,
) -> impl IntoResponse {
    let Some(path) = lan_file(&state.config.network_config_dir, q.lan) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"success": false, "message": format!("Invalid LAN number: {}. Valid range: 1-4", q.lan)})),
        )
            .into_response();
    };

    match std::fs::read_to_string(&path) {
        Ok(content) => {
            let config = parse_config(&content);
            Json(json!({
                "success": true,
                "message": format!("LAN{} network config retrieved", q.lan),
                "data": config,
            }))
            .into_response()
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (
            StatusCode::NOT_FOUND,
            Json(json!({"success": false, "message": format!("LAN{} config file not found: {:?}", q.lan, path)})),
        )
            .into_response(),
        Err(e) => {
            error!("Read network config error: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// ── PUT /api/v1/network ───────────────────────────────────────────────────────

/// Remote network mutation is deliberately unavailable.
///
/// Network files are mounted read-only in the management API. Commissioning
/// must happen through an on-device, recovery-capable workflow rather than a
/// remote HTTP request that could sever the management connection.
#[utoipa::path(put, path = "/api/v1/network", tag = "Network",
    security(("bearer_auth" = [])),
    responses(
        (status = 403, description = "Admin privileges required"),
        (status = 501, description = "Remote network mutation is disabled")
    ))]
pub async fn update_network_config(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    if let Err(error) = require_admin(&state, &headers) {
        return error.into_response();
    }
    network_mutation_disabled_response()
}

// ── POST /api/v1/network/apply ────────────────────────────────────────────────

/// Remote network apply is deliberately unavailable.
///
/// The management API has neither a writable network mount nor access to the
/// host container/runtime control socket. Returning `501` makes that boundary
/// explicit instead of reporting an incidental filesystem or Docker error.
#[utoipa::path(post, path = "/api/v1/network/apply", tag = "Network",
    security(("bearer_auth" = [])),
    responses(
        (status = 403, description = "Admin privileges required"),
        (status = 501, description = "Remote network mutation is disabled")
    ))]
pub async fn apply_network_config(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    if let Err(error) = require_admin(&state, &headers) {
        return error.into_response();
    }
    network_mutation_disabled_response()
}

fn network_mutation_disabled_response() -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({
            "success": false,
            "message": "Remote network mutation is disabled. Use an on-device commissioning workflow with recovery access."
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    use super::*;
    use crate::test_support::{app_state, authorization_headers};

    #[tokio::test]
    async fn network_mutations_reject_viewers_and_are_disabled_for_admins() {
        let state = app_state().await;

        for response in [
            update_network_config(State(Arc::clone(&state)), authorization_headers("Viewer"))
                .await
                .into_response(),
            apply_network_config(State(Arc::clone(&state)), authorization_headers("Viewer"))
                .await
                .into_response(),
        ] {
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }

        for response in [
            update_network_config(State(Arc::clone(&state)), authorization_headers("Admin"))
                .await
                .into_response(),
            apply_network_config(State(state), authorization_headers("Admin"))
                .await
                .into_response(),
        ] {
            assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        }
    }
}
