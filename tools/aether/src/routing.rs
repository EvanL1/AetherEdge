//! Routing management module
//!
//! Provides functionality to manage channel-to-instance point routing via HTTP API

use anyhow::Result;
use clap::{Subcommand, ValueEnum};
use reqwest::Client;
use serde_json::Value;

/// Point type: M (measurement) or A (action)
#[derive(Clone, ValueEnum, serde::Serialize)]
pub(crate) enum PointType {
    /// Measurement point
    M,
    /// Action point
    A,
}

impl std::fmt::Display for PointType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PointType::M => write!(f, "M"),
            PointType::A => write!(f, "A"),
        }
    }
}

/// Four-remote type: T (telemetry), S (signal), C (control), A (adjustment)
#[derive(Clone, ValueEnum, serde::Serialize)]
pub(crate) enum FourRemote {
    /// Telemetry
    T,
    /// Signal
    S,
    /// Control
    C,
    /// Adjustment
    A,
}

/// Physical point types valid as action-command destinations.
#[derive(Clone, ValueEnum, serde::Serialize)]
pub(crate) enum ActionFourRemote {
    /// Binary control point
    C,
    /// Analog adjustment point
    A,
}

impl std::fmt::Display for ActionFourRemote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::C => write!(f, "C"),
            Self::A => write!(f, "A"),
        }
    }
}

#[derive(Subcommand)]
pub enum ActionRoutingCommands {
    /// Create or replace one physical action route
    Upsert {
        /// Instance ID
        instance_id: u32,
        /// Logical action-point ID
        action_point_id: u32,
        /// Physical destination channel
        #[arg(long)]
        channel_id: u32,
        /// Physical destination type: C (control) or A (adjustment)
        #[arg(long, value_enum)]
        channel_type: ActionFourRemote,
        /// Physical destination point ID
        #[arg(long)]
        channel_point_id: u32,
        /// Create the route disabled
        #[arg(long)]
        disabled: bool,
        /// Explicitly confirm this physical command-topology change
        #[arg(long)]
        confirmed: bool,
    },
    /// Delete one physical action route
    Delete {
        /// Instance ID
        instance_id: u32,
        /// Logical action-point ID
        action_point_id: u32,
        /// Explicitly confirm this physical command-topology change
        #[arg(long)]
        confirmed: bool,
    },
    /// Enable one physical action route
    Enable {
        /// Instance ID
        instance_id: u32,
        /// Logical action-point ID
        action_point_id: u32,
        /// Explicitly confirm this physical command-topology change
        #[arg(long)]
        confirmed: bool,
    },
    /// Disable one physical action route
    Disable {
        /// Instance ID
        instance_id: u32,
        /// Logical action-point ID
        action_point_id: u32,
        /// Explicitly confirm this physical command-topology change
        #[arg(long)]
        confirmed: bool,
    },
}

impl std::fmt::Display for FourRemote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FourRemote::T => write!(f, "T"),
            FourRemote::S => write!(f, "S"),
            FourRemote::C => write!(f, "C"),
            FourRemote::A => write!(f, "A"),
        }
    }
}

#[derive(Subcommand)]
pub enum RoutingCommands {
    /// List routing configurations
    List {
        /// Filter by instance ID
        #[arg(short = 'i', long)]
        instance: Option<u32>,
        /// Filter by channel ID
        #[arg(long)]
        channel: Option<u32>,
    },

    /// Manage governed physical action routes
    Action {
        #[command(subcommand)]
        command: ActionRoutingCommands,
    },

    /// Create a single routing entry for an instance
    Create {
        /// Instance ID
        instance_id: u32,
        /// Point type: M (measurement) or A (action)
        #[arg(short = 't', long = "point-type", value_enum)]
        point_type: PointType,
        /// Instance point ID
        #[arg(short = 'p', long = "point-id")]
        point_id: u32,
        /// Channel ID
        #[arg(long = "channel-id")]
        channel_id: u32,
        /// Four-remote type: T (telemetry), S (signal), C (control), A (adjustment)
        #[arg(short = 'r', long = "four-remote", value_enum)]
        four_remote: FourRemote,
        /// Channel point ID
        #[arg(short = 'P', long = "channel-point-id")]
        channel_point_id: u32,
        /// Explicitly confirm a physical command-topology change (required for A routes)
        #[arg(long)]
        confirmed: bool,
    },

    /// Batch upsert routing from JSON file or stdin
    Batch {
        /// Instance ID
        instance_id: u32,
        /// Path to JSON file with routing entries (use '-' for stdin)
        #[arg(long)]
        file: String,
    },

    /// Delete all routing for an instance
    DeleteInstance {
        /// Instance name
        instance_name: String,
        /// Skip confirmation
        #[arg(short, long)]
        force: bool,
        /// Explicitly confirm deletion of physical action routes
        #[arg(long)]
        confirmed: bool,
    },

    /// Delete all routing for a channel
    DeleteChannel {
        /// Channel ID
        channel_id: u32,
        /// Skip confirmation
        #[arg(short, long)]
        force: bool,
        /// Explicitly confirm deletion of physical action routes
        #[arg(long)]
        confirmed: bool,
    },
}

pub async fn handle_command(cmd: RoutingCommands, base_url: &str, json: bool) -> Result<()> {
    let client = RoutingClient::new(base_url)?;

    match cmd {
        RoutingCommands::List { instance, channel } => match (instance, channel) {
            (Some(_), Some(_)) => {
                anyhow::bail!("Use either --instance or --channel, not both");
            },
            (Some(id), None) => {
                let result = client.list_by_instance(id).await?;
                if json {
                    crate::output::print_success(&result);
                } else {
                    println!(
                        "Routing for instance {}: {}",
                        id,
                        serde_json::to_string_pretty(&result)?
                    );
                }
            },
            (None, Some(id)) => {
                let result = client.list_by_channel(id).await?;
                if json {
                    crate::output::print_success(&result);
                } else {
                    println!(
                        "Routing for channel {}: {}",
                        id,
                        serde_json::to_string_pretty(&result)?
                    );
                }
            },
            (None, None) => {
                let result = client.list_all().await?;
                if json {
                    crate::output::print_success(&result);
                } else {
                    println!("Routing: {}", serde_json::to_string_pretty(&result)?);
                }
            },
        },
        RoutingCommands::Action { command } => {
            let result = match command {
                ActionRoutingCommands::Upsert {
                    instance_id,
                    action_point_id,
                    channel_id,
                    channel_type,
                    channel_point_id,
                    disabled,
                    confirmed,
                } => {
                    client
                        .upsert_action_route(
                            instance_id,
                            action_point_id,
                            channel_id,
                            &channel_type.to_string(),
                            channel_point_id,
                            !disabled,
                            confirmed,
                        )
                        .await?
                },
                ActionRoutingCommands::Delete {
                    instance_id,
                    action_point_id,
                    confirmed,
                } => {
                    client
                        .delete_action_route(instance_id, action_point_id, confirmed)
                        .await?
                },
                ActionRoutingCommands::Enable {
                    instance_id,
                    action_point_id,
                    confirmed,
                } => {
                    client
                        .set_action_route_enabled(instance_id, action_point_id, true, confirmed)
                        .await?
                },
                ActionRoutingCommands::Disable {
                    instance_id,
                    action_point_id,
                    confirmed,
                } => {
                    client
                        .set_action_route_enabled(instance_id, action_point_id, false, confirmed)
                        .await?
                },
            };
            if json {
                crate::output::print_success(&result);
            } else {
                println!("Action routing: {}", serde_json::to_string_pretty(&result)?);
            }
        },
        RoutingCommands::Create {
            instance_id,
            point_type,
            point_id,
            channel_id,
            four_remote,
            channel_point_id,
            confirmed,
        } => {
            let result = match point_type {
                PointType::A => {
                    client
                        .upsert_action_route(
                            instance_id,
                            point_id,
                            channel_id,
                            &four_remote.to_string(),
                            channel_point_id,
                            true,
                            confirmed,
                        )
                        .await?
                },
                PointType::M => {
                    let entry = serde_json::json!({
                        "point_type": "M",
                        "point_id": point_id,
                        "channel_id": channel_id,
                        "four_remote": four_remote,
                        "channel_point_id": channel_point_id,
                    });
                    client.create_routing(instance_id, entry).await?
                },
            };
            if json {
                crate::output::print_success(&result);
            } else {
                println!(
                    "Routing created for instance {}: {}",
                    instance_id,
                    serde_json::to_string_pretty(&result)?
                );
            }
        },
        RoutingCommands::Batch { instance_id, file } => {
            let content = if file == "-" {
                let mut buf = String::new();
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
                buf
            } else {
                std::fs::read_to_string(&file)
                    .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", file, e))?
            };
            let entries: Value = serde_json::from_str(&content)
                .map_err(|e| anyhow::anyhow!("Invalid JSON in routing file: {}", e))?;
            if entries
                .as_array()
                .is_some_and(|items| items.iter().any(is_action_routing_entry))
            {
                anyhow::bail!(
                    "action-routing batch writes are disabled until the governed batch command is available; use `aether routing create ... --point-type a --confirmed`"
                );
            }
            let result = client.batch_routing(instance_id, entries).await?;
            if json {
                crate::output::print_success(&result);
            } else {
                println!("Batch routing upserted for instance {}", instance_id);
                println!("{}", serde_json::to_string_pretty(&result)?);
            }
        },
        RoutingCommands::DeleteInstance {
            instance_name,
            force,
            confirmed,
        } => {
            let mut confirmed = confirmed;
            if !force && !json {
                println!("Delete all routing for instance '{}'? [y/N]", instance_name);
                let mut input = String::new();
                std::io::stdin().read_line(&mut input)?;
                if !input.trim().eq_ignore_ascii_case("y") {
                    println!("Cancelled");
                    return Ok(());
                }
                confirmed = true;
            }
            client
                .delete_instance_routing(&instance_name, confirmed)
                .await?;
            if json {
                crate::output::print_ok();
            } else {
                println!("Routing deleted for instance '{}'", instance_name);
            }
        },
        RoutingCommands::DeleteChannel {
            channel_id,
            force,
            confirmed,
        } => {
            let mut confirmed = confirmed;
            if !force && !json {
                println!("Delete all routing for channel {}? [y/N]", channel_id);
                let mut input = String::new();
                std::io::stdin().read_line(&mut input)?;
                if !input.trim().eq_ignore_ascii_case("y") {
                    println!("Cancelled");
                    return Ok(());
                }
                confirmed = true;
            }
            client.delete_channel_routing(channel_id, confirmed).await?;
            if json {
                crate::output::print_ok();
            } else {
                println!("Routing deleted for channel {}", channel_id);
            }
        },
    }

    Ok(())
}

fn is_action_routing_entry(value: &Value) -> bool {
    value
        .get("point_type")
        .and_then(Value::as_str)
        .is_some_and(|point_type| point_type.eq_ignore_ascii_case("A"))
}

// HTTP client for routing management
pub(crate) struct RoutingClient {
    client: Client,
    base_url: String,
    access_token: Option<String>,
}

impl RoutingClient {
    pub(crate) fn new(base_url: &str) -> Result<Self> {
        Ok(Self {
            client: Client::new(),
            base_url: base_url.to_string(),
            access_token: std::env::var("AETHER_ACCESS_TOKEN")
                .ok()
                .filter(|value| !value.trim().is_empty() && value.trim() == value),
        })
    }

    #[cfg(test)]
    pub(crate) fn with_access_token(base_url: &str, access_token: &str) -> Result<Self> {
        Ok(Self {
            client: Client::new(),
            base_url: base_url.to_string(),
            access_token: Some(access_token.to_string()),
        })
    }

    pub(crate) async fn list_all(&self) -> Result<Value> {
        let response = self
            .client
            .get(format!("{}/api/routing", self.base_url))
            .send()
            .await?;

        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            Err(anyhow::anyhow!(
                "Failed to list routing: {} - {} (ensure automation is running)",
                status,
                text
            ))
        }
    }

    async fn list_by_instance(&self, id: u32) -> Result<Value> {
        let response = self
            .client
            .get(format!("{}/api/instances/{}/routing", self.base_url, id))
            .send()
            .await?;

        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            Err(anyhow::anyhow!(
                "Failed to list routing for instance {}: {} - {}",
                id,
                status,
                text
            ))
        }
    }

    async fn list_by_channel(&self, id: u32) -> Result<Value> {
        let response = self
            .client
            .get(format!("{}/api/routing/by-channel/{}", self.base_url, id))
            .send()
            .await?;

        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            Err(anyhow::anyhow!(
                "Failed to list routing for channel {}: {} - {}",
                id,
                status,
                text
            ))
        }
    }

    async fn create_routing(&self, instance_id: u32, entries: Value) -> Result<Value> {
        let response = self
            .client
            .post(format!(
                "{}/api/instances/{}/routing",
                self.base_url, instance_id
            ))
            .json(&entries)
            .send()
            .await?;

        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            Err(anyhow::anyhow!(
                "Failed to create routing for instance {}: {} - {}",
                instance_id,
                status,
                text
            ))
        }
    }

    async fn batch_routing(&self, instance_id: u32, entries: Value) -> Result<Value> {
        let response = self
            .client
            .put(format!(
                "{}/api/instances/{}/routing",
                self.base_url, instance_id
            ))
            .json(&entries)
            .send()
            .await?;

        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            Err(anyhow::anyhow!(
                "Failed to batch upsert routing for instance {}: {} - {}",
                instance_id,
                status,
                text
            ))
        }
    }

    pub(crate) async fn upsert_action_route(
        &self,
        instance_id: u32,
        action_id: u32,
        channel_id: u32,
        channel_type: &str,
        channel_point_id: u32,
        enabled: bool,
        confirmed: bool,
    ) -> Result<Value> {
        if !matches!(channel_type, "C" | "A") {
            anyhow::bail!("action routing channel type must be C or A");
        }
        let access_token = self.routing_management_token(confirmed)?;
        let response = self
            .client
            .put(format!(
                "{}/api/instances/{instance_id}/actions/{action_id}/routing",
                self.base_url
            ))
            .bearer_auth(access_token)
            .header("x-request-id", uuid::Uuid::new_v4().to_string())
            .json(&serde_json::json!({
                "channel_id": channel_id,
                "four_remote": channel_type,
                "channel_point_id": channel_point_id,
                "enabled": enabled,
                "confirmed": true
            }))
            .send()
            .await?;
        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            Err(crate::output::parse_error_body("Failed to upsert action routing", response).await)
        }
    }

    pub(crate) async fn delete_action_route(
        &self,
        instance_id: u32,
        action_id: u32,
        confirmed: bool,
    ) -> Result<Value> {
        let access_token = self.routing_management_token(confirmed)?;
        let response = self
            .client
            .delete(format!(
                "{}/api/instances/{instance_id}/actions/{action_id}/routing",
                self.base_url
            ))
            .bearer_auth(access_token)
            .header("x-request-id", uuid::Uuid::new_v4().to_string())
            .json(&serde_json::json!({ "confirmed": true }))
            .send()
            .await?;
        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            Err(crate::output::parse_error_body("Failed to delete action routing", response).await)
        }
    }

    pub(crate) async fn set_action_route_enabled(
        &self,
        instance_id: u32,
        action_id: u32,
        enabled: bool,
        confirmed: bool,
    ) -> Result<Value> {
        let access_token = self.routing_management_token(confirmed)?;
        let response = self
            .client
            .patch(format!(
                "{}/api/instances/{instance_id}/actions/{action_id}/routing",
                self.base_url
            ))
            .bearer_auth(access_token)
            .header("x-request-id", uuid::Uuid::new_v4().to_string())
            .json(&serde_json::json!({
                "enabled": enabled,
                "confirmed": true
            }))
            .send()
            .await?;
        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            Err(
                crate::output::parse_error_body("Failed to change action-routing state", response)
                    .await,
            )
        }
    }

    async fn delete_instance_routing(&self, name: &str, confirmed: bool) -> Result<()> {
        let access_token = self.routing_management_token(confirmed)?;
        let response = self
            .client
            .delete(format!(
                "{}/api/routing/instances/{}?confirm=true",
                self.base_url, name
            ))
            .bearer_auth(access_token)
            .header("x-request-id", uuid::Uuid::new_v4().to_string())
            .send()
            .await?;

        if response.status().is_success() {
            Ok(())
        } else {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            Err(anyhow::anyhow!(
                "Failed to delete routing for instance '{}': {} - {}",
                name,
                status,
                text
            ))
        }
    }

    async fn delete_channel_routing(&self, id: u32, confirmed: bool) -> Result<()> {
        let access_token = self.routing_management_token(confirmed)?;
        let response = self
            .client
            .delete(format!(
                "{}/api/routing/channels/{}?confirm=true",
                self.base_url, id
            ))
            .bearer_auth(access_token)
            .header("x-request-id", uuid::Uuid::new_v4().to_string())
            .send()
            .await?;

        if response.status().is_success() {
            Ok(())
        } else {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            Err(anyhow::anyhow!(
                "Failed to delete routing for channel {}: {} - {}",
                id,
                status,
                text
            ))
        }
    }

    fn routing_management_token(&self, confirmed: bool) -> Result<&str> {
        if !confirmed {
            anyhow::bail!("action routing requires explicit confirmation (--confirmed)");
        }
        crate::transport_security::require_secure_bearer_transport(&self.base_url)?;
        self.access_token.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "action routing requires AETHER_ACCESS_TOKEN from an authenticated Admin or Engineer session"
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{ActionRoutingCommands, RoutingClient, RoutingCommands};
    use clap::Parser;
    use wiremock::matchers::{body_json, header, header_exists, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn bearer_writes_reject_remote_plaintext_before_token_access() {
        let client = RoutingClient {
            client: reqwest::Client::new(),
            base_url: "http://192.0.2.10:6002".to_string(),
            access_token: None,
        };

        let error = client
            .routing_management_token(true)
            .expect_err("remote plaintext must fail closed");
        assert!(error.to_string().contains("refusing to send"), "{error:#}");
    }

    #[derive(Parser)]
    struct RoutingCli {
        #[command(subcommand)]
        command: RoutingCommands,
    }

    #[test]
    fn action_subcommands_expose_explicit_confirmation() {
        let cli =
            RoutingCli::try_parse_from(["routing", "action", "disable", "7", "1", "--confirmed"])
                .expect("governed action-routing CLI");

        assert!(matches!(
            cli.command,
            RoutingCommands::Action {
                command: ActionRoutingCommands::Disable {
                    instance_id: 7,
                    action_point_id: 1,
                    confirmed: true,
                }
            }
        ));
    }

    #[tokio::test]
    async fn action_route_upsert_uses_the_governed_http_contract() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/instances/7/actions/1/routing"))
            .and(header("authorization", "Bearer signed-access-token"))
            .and(header_exists("x-request-id"))
            .and(body_json(serde_json::json!({
                "channel_id": 3,
                "four_remote": "A",
                "channel_point_id": 5,
                "enabled": true,
                "confirmed": true
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let client = RoutingClient::with_access_token(&server.uri(), "signed-access-token")
            .expect("routing client");
        client
            .upsert_action_route(7, 1, 3, "A", 5, true, true)
            .await
            .expect("governed upsert");
    }

    #[tokio::test]
    async fn action_route_mutation_rejects_unconfirmed_before_http() {
        let server = MockServer::start().await;
        let client = RoutingClient::with_access_token(&server.uri(), "signed-access-token")
            .expect("routing client");

        let error = client
            .delete_action_route(7, 1, false)
            .await
            .expect_err("unconfirmed routing mutation must fail");
        assert!(error.to_string().contains("explicit confirmation"));
        assert!(
            server
                .received_requests()
                .await
                .expect("received requests")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn scoped_delete_uses_bearer_confirmation_and_correlation() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/api/routing/channels/3"))
            .and(query_param("confirm", "true"))
            .and(header("authorization", "Bearer signed-access-token"))
            .and(header_exists("x-request-id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let client = RoutingClient::with_access_token(&server.uri(), "signed-access-token")
            .expect("routing client");
        client
            .delete_channel_routing(3, true)
            .await
            .expect("governed channel delete");
    }
}
