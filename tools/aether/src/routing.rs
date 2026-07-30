//! Routing management module
//!
//! Provides functionality to manage channel-to-instance point routing via HTTP API

use anyhow::Result;
use clap::{Subcommand, ValueEnum};
use reqwest::{Client, Method};
use serde_json::Value;

/// Physical point types valid as measurement destinations.
#[derive(Clone, ValueEnum)]
pub(crate) enum MeasurementFourRemote {
    /// Telemetry
    T,
    /// Signal
    S,
}

impl MeasurementFourRemote {
    const fn as_str(&self) -> &'static str {
        match self {
            Self::T => "T",
            Self::S => "S",
        }
    }
}

/// Physical point types valid as action-command destinations.
#[derive(Clone, ValueEnum)]
pub(crate) enum ActionFourRemote {
    /// Binary control point
    C,
    /// Analog adjustment point
    A,
}

impl ActionFourRemote {
    const fn as_str(&self) -> &'static str {
        match self {
            Self::C => "C",
            Self::A => "A",
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
        /// Current logical-routing revision
        #[arg(long)]
        expected_revision: u64,
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
        /// Current logical-routing revision
        #[arg(long)]
        expected_revision: u64,
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
        /// Current logical-routing revision
        #[arg(long)]
        expected_revision: u64,
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
        /// Current logical-routing revision
        #[arg(long)]
        expected_revision: u64,
        /// Explicitly confirm this physical command-topology change
        #[arg(long)]
        confirmed: bool,
    },
}

#[derive(Subcommand)]
pub enum MeasurementRoutingCommands {
    /// Create or replace one measurement route
    Upsert {
        /// Instance ID
        instance_id: u32,
        /// Logical measurement-point ID
        measurement_point_id: u32,
        /// Physical source channel
        #[arg(long)]
        channel_id: u32,
        /// Physical source type: T (telemetry) or S (signal)
        #[arg(long, value_enum)]
        channel_type: MeasurementFourRemote,
        /// Physical source point ID
        #[arg(long)]
        channel_point_id: u32,
        /// Create the route disabled
        #[arg(long)]
        disabled: bool,
        /// Current logical-routing revision
        #[arg(long)]
        expected_revision: u64,
        /// Explicitly confirm this topology change
        #[arg(long)]
        confirmed: bool,
    },
    /// Delete one measurement route
    Delete {
        instance_id: u32,
        measurement_point_id: u32,
        #[arg(long)]
        expected_revision: u64,
        #[arg(long)]
        confirmed: bool,
    },
    /// Enable one measurement route
    Enable {
        instance_id: u32,
        measurement_point_id: u32,
        #[arg(long)]
        expected_revision: u64,
        #[arg(long)]
        confirmed: bool,
    },
    /// Disable one measurement route
    Disable {
        instance_id: u32,
        measurement_point_id: u32,
        #[arg(long)]
        expected_revision: u64,
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

    /// Manage governed measurement routes
    Measurement {
        #[command(subcommand)]
        command: MeasurementRoutingCommands,
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
        RoutingCommands::Measurement { command } => {
            let result = match command {
                MeasurementRoutingCommands::Upsert {
                    instance_id,
                    measurement_point_id,
                    channel_id,
                    channel_type,
                    channel_point_id,
                    disabled,
                    expected_revision,
                    confirmed,
                } => {
                    client
                        .upsert_measurement_route(
                            instance_id,
                            measurement_point_id,
                            RoutingTarget {
                                channel_id,
                                channel_type: channel_type.as_str(),
                                channel_point_id,
                                enabled: !disabled,
                            },
                            expected_revision,
                            confirmed,
                        )
                        .await?
                },
                MeasurementRoutingCommands::Delete {
                    instance_id,
                    measurement_point_id,
                    expected_revision,
                    confirmed,
                } => {
                    client
                        .delete_measurement_route(
                            instance_id,
                            measurement_point_id,
                            expected_revision,
                            confirmed,
                        )
                        .await?
                },
                MeasurementRoutingCommands::Enable {
                    instance_id,
                    measurement_point_id,
                    expected_revision,
                    confirmed,
                } => {
                    client
                        .set_measurement_route_enabled(
                            instance_id,
                            measurement_point_id,
                            true,
                            expected_revision,
                            confirmed,
                        )
                        .await?
                },
                MeasurementRoutingCommands::Disable {
                    instance_id,
                    measurement_point_id,
                    expected_revision,
                    confirmed,
                } => {
                    client
                        .set_measurement_route_enabled(
                            instance_id,
                            measurement_point_id,
                            false,
                            expected_revision,
                            confirmed,
                        )
                        .await?
                },
            };
            print_mutation_result("Measurement routing", &result, json)?;
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
                    expected_revision,
                    confirmed,
                } => {
                    client
                        .upsert_action_route(
                            instance_id,
                            action_point_id,
                            RoutingTarget {
                                channel_id,
                                channel_type: channel_type.as_str(),
                                channel_point_id,
                                enabled: !disabled,
                            },
                            expected_revision,
                            confirmed,
                        )
                        .await?
                },
                ActionRoutingCommands::Delete {
                    instance_id,
                    action_point_id,
                    expected_revision,
                    confirmed,
                } => {
                    client
                        .delete_action_route(
                            instance_id,
                            action_point_id,
                            expected_revision,
                            confirmed,
                        )
                        .await?
                },
                ActionRoutingCommands::Enable {
                    instance_id,
                    action_point_id,
                    expected_revision,
                    confirmed,
                } => {
                    client
                        .set_action_route_enabled(
                            instance_id,
                            action_point_id,
                            true,
                            expected_revision,
                            confirmed,
                        )
                        .await?
                },
                ActionRoutingCommands::Disable {
                    instance_id,
                    action_point_id,
                    expected_revision,
                    confirmed,
                } => {
                    client
                        .set_action_route_enabled(
                            instance_id,
                            action_point_id,
                            false,
                            expected_revision,
                            confirmed,
                        )
                        .await?
                },
            };
            print_mutation_result("Action routing", &result, json)?;
        },
    }

    Ok(())
}

fn print_mutation_result(label: &str, result: &Value, json: bool) -> Result<()> {
    if json {
        crate::output::print_success(result);
    } else {
        println!("{label}: {}", serde_json::to_string_pretty(result)?);
    }
    Ok(())
}

// HTTP client for routing management
pub(crate) struct RoutingClient {
    client: Client,
    base_url: String,
    access_token: Option<String>,
}

pub(crate) struct RoutingTarget<'a> {
    pub(crate) channel_id: u32,
    pub(crate) channel_type: &'a str,
    pub(crate) channel_point_id: u32,
    pub(crate) enabled: bool,
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

    fn apply_auth(&self, request: reqwest::RequestBuilder) -> Result<reqwest::RequestBuilder> {
        match &self.access_token {
            Some(token) => {
                crate::transport_security::require_secure_bearer_transport(&self.base_url)?;
                Ok(request.bearer_auth(token))
            },
            None => Ok(request),
        }
    }

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

    pub(crate) async fn upsert_measurement_route(
        &self,
        instance_id: u32,
        measurement_id: u32,
        target: RoutingTarget<'_>,
        expected_revision: u64,
        confirmed: bool,
    ) -> Result<Value> {
        if !matches!(target.channel_type, "T" | "S") {
            anyhow::bail!("measurement routing channel type must be T or S");
        }
        self.send_mutation(
            Method::PUT,
            format!("/api/instances/{instance_id}/measurements/{measurement_id}/routing"),
            serde_json::json!({
                "channel_id": target.channel_id,
                "four_remote": target.channel_type,
                "channel_point_id": target.channel_point_id,
                "enabled": target.enabled,
                "expected_revision": expected_revision,
                "confirmed": true
            }),
            "Failed to upsert measurement routing",
            expected_revision,
            confirmed,
        )
        .await
    }

    pub(crate) async fn delete_measurement_route(
        &self,
        instance_id: u32,
        measurement_id: u32,
        expected_revision: u64,
        confirmed: bool,
    ) -> Result<Value> {
        self.send_mutation(
            Method::DELETE,
            format!("/api/instances/{instance_id}/measurements/{measurement_id}/routing"),
            serde_json::json!({
                "expected_revision": expected_revision,
                "confirmed": true
            }),
            "Failed to delete measurement routing",
            expected_revision,
            confirmed,
        )
        .await
    }

    pub(crate) async fn set_measurement_route_enabled(
        &self,
        instance_id: u32,
        measurement_id: u32,
        enabled: bool,
        expected_revision: u64,
        confirmed: bool,
    ) -> Result<Value> {
        self.send_mutation(
            Method::PATCH,
            format!("/api/instances/{instance_id}/measurements/{measurement_id}/routing"),
            serde_json::json!({
                "enabled": enabled,
                "expected_revision": expected_revision,
                "confirmed": true
            }),
            "Failed to change measurement-routing state",
            expected_revision,
            confirmed,
        )
        .await
    }

    pub(crate) async fn upsert_action_route(
        &self,
        instance_id: u32,
        action_id: u32,
        target: RoutingTarget<'_>,
        expected_revision: u64,
        confirmed: bool,
    ) -> Result<Value> {
        if !matches!(target.channel_type, "C" | "A") {
            anyhow::bail!("action routing channel type must be C or A");
        }
        self.send_mutation(
            Method::PUT,
            format!("/api/instances/{instance_id}/actions/{action_id}/routing"),
            serde_json::json!({
                "channel_id": target.channel_id,
                "four_remote": target.channel_type,
                "channel_point_id": target.channel_point_id,
                "enabled": target.enabled,
                "expected_revision": expected_revision,
                "confirmed": true
            }),
            "Failed to upsert action routing",
            expected_revision,
            confirmed,
        )
        .await
    }

    pub(crate) async fn delete_action_route(
        &self,
        instance_id: u32,
        action_id: u32,
        expected_revision: u64,
        confirmed: bool,
    ) -> Result<Value> {
        self.send_mutation(
            Method::DELETE,
            format!("/api/instances/{instance_id}/actions/{action_id}/routing"),
            serde_json::json!({
                "expected_revision": expected_revision,
                "confirmed": true
            }),
            "Failed to delete action routing",
            expected_revision,
            confirmed,
        )
        .await
    }

    pub(crate) async fn set_action_route_enabled(
        &self,
        instance_id: u32,
        action_id: u32,
        enabled: bool,
        expected_revision: u64,
        confirmed: bool,
    ) -> Result<Value> {
        self.send_mutation(
            Method::PATCH,
            format!("/api/instances/{instance_id}/actions/{action_id}/routing"),
            serde_json::json!({
                "enabled": enabled,
                "expected_revision": expected_revision,
                "confirmed": true
            }),
            "Failed to change action-routing state",
            expected_revision,
            confirmed,
        )
        .await
    }

    async fn send_mutation(
        &self,
        method: Method,
        path: String,
        body: Value,
        error_context: &'static str,
        expected_revision: u64,
        confirmed: bool,
    ) -> Result<Value> {
        self.require_routing_management_auth(expected_revision, confirmed)?;
        let request = self
            .client
            .request(method, format!("{}{path}", self.base_url))
            .header("x-request-id", uuid::Uuid::new_v4().to_string())
            .header("x-aether-confirmed", "true")
            .json(&body);
        let response = self.apply_auth(request)?.send().await?;
        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            Err(crate::output::parse_error_body(error_context, response).await)
        }
    }

    fn require_routing_management_auth(
        &self,
        expected_revision: u64,
        confirmed: bool,
    ) -> Result<()> {
        if expected_revision == 0 || expected_revision >= i64::MAX as u64 {
            anyhow::bail!("expected revision must be in 1..i64::MAX");
        }
        if !confirmed {
            anyhow::bail!("routing mutation requires explicit confirmation (--confirmed)");
        }
        crate::transport_security::require_secure_bearer_transport(&self.base_url)?;
        if self.access_token.is_none() {
            anyhow::bail!(
                "routing mutation requires AETHER_ACCESS_TOKEN from an authenticated Admin or Engineer session"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ActionRoutingCommands, MeasurementRoutingCommands, RoutingClient, RoutingCommands,
        RoutingTarget,
    };
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

        let client = RoutingClient {
            client: reqwest::Client::new(),
            base_url: server.uri(),
            access_token: None,
        };
        client.list_all().await.expect("tokenless list");

        let requests = server.received_requests().await.expect("received requests");
        assert!(
            requests
                .iter()
                .all(|request| !request.headers.contains_key("authorization")),
            "tokenless reads must not carry an authorization header"
        );
    }

    #[tokio::test]
    async fn measurement_route_upsert_uses_the_governed_per_point_contract() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/instances/7/measurements/2/routing"))
            .and(header("authorization", "Bearer signed-access-token"))
            .and(header("x-aether-confirmed", "true"))
            .and(header_exists("x-request-id"))
            .and(body_json(serde_json::json!({
                "channel_id": 3,
                "four_remote": "T",
                "channel_point_id": 5,
                "enabled": true,
                "expected_revision": 9,
                "confirmed": true
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let client = RoutingClient::with_access_token(&server.uri(), "signed-access-token")
            .expect("routing client");
        client
            .upsert_measurement_route(
                7,
                2,
                RoutingTarget {
                    channel_id: 3,
                    channel_type: "T",
                    channel_point_id: 5,
                    enabled: true,
                },
                9,
                true,
            )
            .await
            .expect("governed measurement upsert");
    }

    #[test]
    fn bearer_writes_reject_remote_plaintext_before_token_access() {
        let client = RoutingClient {
            client: reqwest::Client::new(),
            base_url: "http://192.0.2.10:6002".to_string(),
            access_token: None,
        };

        let error = client
            .require_routing_management_auth(9, true)
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
        let cli = RoutingCli::try_parse_from([
            "routing",
            "action",
            "disable",
            "7",
            "1",
            "--expected-revision",
            "9",
            "--confirmed",
        ])
        .expect("governed action-routing CLI");

        assert!(matches!(
            cli.command,
            RoutingCommands::Action {
                command: ActionRoutingCommands::Disable {
                    instance_id: 7,
                    action_point_id: 1,
                    expected_revision: 9,
                    confirmed: true,
                }
            }
        ));
    }

    #[test]
    fn measurement_subcommands_require_revision_and_confirmation() {
        let cli = RoutingCli::try_parse_from([
            "routing",
            "measurement",
            "upsert",
            "7",
            "2",
            "--channel-id",
            "3",
            "--channel-type",
            "t",
            "--channel-point-id",
            "5",
            "--expected-revision",
            "9",
            "--confirmed",
        ])
        .expect("governed measurement-routing CLI");

        assert!(matches!(
            cli.command,
            RoutingCommands::Measurement {
                command: MeasurementRoutingCommands::Upsert {
                    instance_id: 7,
                    measurement_point_id: 2,
                    expected_revision: 9,
                    confirmed: true,
                    ..
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
                "expected_revision": 9,
                "confirmed": true
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let client = RoutingClient::with_access_token(&server.uri(), "signed-access-token")
            .expect("routing client");
        client
            .upsert_action_route(
                7,
                1,
                RoutingTarget {
                    channel_id: 3,
                    channel_type: "A",
                    channel_point_id: 5,
                    enabled: true,
                },
                9,
                true,
            )
            .await
            .expect("governed upsert");
    }

    #[tokio::test]
    async fn action_route_mutation_rejects_unconfirmed_before_http() {
        let server = MockServer::start().await;
        let client = RoutingClient::with_access_token(&server.uri(), "signed-access-token")
            .expect("routing client");

        let error = client
            .delete_action_route(7, 1, 9, false)
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

    #[test]
    fn retired_ambiguous_write_commands_are_not_parseable() {
        for command in ["create", "batch", "delete-instance", "delete-channel"] {
            let result = RoutingCli::try_parse_from(["routing", command]);
            assert!(
                result.is_err(),
                "retired command must stay absent: {command}"
            );
        }
    }
}
