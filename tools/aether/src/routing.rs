//! Routing management module
//!
//! Provides functionality to manage channel-to-instance point routing via HTTP API

use anyhow::Result;
use clap::{Subcommand, ValueEnum};
use serde_json::Value;

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
    }

    Ok(())
}

// HTTP client for routing management
crate::api_client::authenticated_api_client!(pub(crate) RoutingClient);

impl RoutingClient {
    pub(crate) async fn list_all(&self) -> Result<Value> {
        let request = self.client.get(format!("{}/api/routing", self.base_url));
        let response = self.apply_auth(request)?.send().await?;

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
        let request = self
            .client
            .get(format!("{}/api/instances/{}/routing", self.base_url, id));
        let response = self.apply_auth(request)?.send().await?;

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
        let request = self
            .client
            .get(format!("{}/api/routing/by-channel/{}", self.base_url, id));
        let response = self.apply_auth(request)?.send().await?;

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
        self.require_routing_management_auth(confirmed)?;
        let request = self
            .client
            .put(format!(
                "{}/api/instances/{instance_id}/actions/{action_id}/routing",
                self.base_url
            ))
            .header("x-request-id", uuid::Uuid::new_v4().to_string())
            .header("x-aether-confirmed", "true")
            .json(&serde_json::json!({
                "channel_id": channel_id,
                "four_remote": channel_type,
                "channel_point_id": channel_point_id,
                "enabled": enabled,
                "confirmed": true
            }));
        let response = self.apply_auth(request)?.send().await?;
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
        self.require_routing_management_auth(confirmed)?;
        let request = self
            .client
            .delete(format!(
                "{}/api/instances/{instance_id}/actions/{action_id}/routing",
                self.base_url
            ))
            .header("x-request-id", uuid::Uuid::new_v4().to_string())
            .header("x-aether-confirmed", "true")
            .json(&serde_json::json!({ "confirmed": true }));
        let response = self.apply_auth(request)?.send().await?;
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
        self.require_routing_management_auth(confirmed)?;
        let request = self
            .client
            .patch(format!(
                "{}/api/instances/{instance_id}/actions/{action_id}/routing",
                self.base_url
            ))
            .header("x-request-id", uuid::Uuid::new_v4().to_string())
            .header("x-aether-confirmed", "true")
            .json(&serde_json::json!({
                "enabled": enabled,
                "confirmed": true
            }));
        let response = self.apply_auth(request)?.send().await?;
        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            Err(
                crate::output::parse_error_body("Failed to change action-routing state", response)
                    .await,
            )
        }
    }

    fn require_routing_management_auth(&self, confirmed: bool) -> Result<()> {
        self.require_governed_auth(
            confirmed,
            "action routing requires explicit confirmation (--confirmed)",
            "action routing requires AETHER_ACCESS_TOKEN from an authenticated Admin or Engineer session",
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{ActionRoutingCommands, RoutingClient, RoutingCommands};
    use clap::Parser;
    use wiremock::matchers::{body_json, header, header_exists, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn list_all_attaches_bearer_when_access_token_is_present() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/routing"))
            .and(header("authorization", "Bearer signed-access-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .expect(1)
            .mount(&server)
            .await;

        let client = RoutingClient::with_access_token(&server.uri(), "signed-access-token")
            .expect("routing client");
        client.list_all().await.expect("authenticated list");
    }

    #[tokio::test]
    async fn list_all_stays_unauthenticated_without_access_token() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/routing"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .expect(1)
            .mount(&server)
            .await;

        let client = RoutingClient::without_access_token(&server.uri());
        client.list_all().await.expect("tokenless list");

        let requests = server.received_requests().await.expect("received requests");
        assert!(
            requests
                .iter()
                .all(|request| !request.headers.contains_key("authorization")),
            "tokenless reads must not carry an authorization header"
        );
    }

    #[test]
    fn bearer_writes_reject_remote_plaintext_before_token_access() {
        let client = RoutingClient::without_access_token("http://192.0.2.10:6002");

        let error = client
            .require_routing_management_auth(true)
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
            .and(header("x-aether-confirmed", "true"))
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
}
