//! Channel management module
//!
//! Provides functionality to manage communication channels via HTTP API

use anyhow::Result;
use clap::Subcommand;
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;

#[derive(Subcommand)]
pub enum ChannelCommands {
    /// List all channels
    #[command(about = "List all configured communication channels")]
    List,

    /// Get channel status
    #[command(about = "Get status of a specific channel")]
    Status {
        /// Channel ID
        channel_id: u32,
    },

    /// Reconcile all channel runtimes from authoritative desired state
    #[command(about = "Reconcile all channel runtimes from authoritative desired state")]
    Reload {
        /// Explicitly confirm this high-risk runtime reconciliation
        #[arg(long)]
        confirmed: bool,
    },

    /// Check service health
    #[command(about = "Check communication service health")]
    Health,

    /// Create a new channel
    #[command(about = "Create a new communication channel")]
    Create {
        /// Channel name (must be unique)
        #[arg(long)]
        name: String,
        /// Protocol type (modbus_tcp, modbus_rtu, virtual, di_do, can)
        #[arg(long)]
        protocol: String,
        /// Protocol parameters as JSON string (e.g. '{"host":"192.168.1.10","port":502}')
        #[arg(long)]
        params: String,
        /// Channel description
        #[arg(long)]
        description: Option<String>,
        /// Start channel immediately (default: false)
        #[arg(long, default_value_t = false, action = clap::ArgAction::Set)]
        enabled: bool,
        /// Override channel ID (auto-assigned if omitted)
        #[arg(long)]
        id: Option<u32>,
        /// Explicitly confirm this high-risk commissioning change
        #[arg(long)]
        confirmed: bool,
    },

    /// Update channel configuration
    #[command(about = "Update an existing channel's configuration")]
    Update {
        /// Channel ID to update
        channel_id: u32,
        /// New channel name
        #[arg(long)]
        name: Option<String>,
        /// Updated protocol parameters as JSON string
        #[arg(long)]
        params: Option<String>,
        /// Updated description
        #[arg(long)]
        description: Option<String>,
        /// Compare-and-set guard for the current desired-state revision
        #[arg(long)]
        expected_revision: u64,
        /// Explicitly confirm this high-risk commissioning change
        #[arg(long)]
        confirmed: bool,
    },

    /// Delete a channel and its measurement-owned dependents
    #[command(
        about = "Delete a channel and its measurement-owned dependents (action routes must be removed first)"
    )]
    Delete {
        /// Channel ID to delete
        channel_id: u32,
        /// Skip confirmation prompt
        #[arg(short, long)]
        force: bool,
        /// Compare-and-set guard for the current desired-state revision
        #[arg(long)]
        expected_revision: u64,
        /// Explicitly confirm this high-risk commissioning change
        #[arg(long)]
        confirmed: bool,
    },

    /// Enable a channel
    #[command(about = "Enable a channel")]
    Enable {
        /// Channel ID
        channel_id: u32,
        /// Compare-and-set guard for the current desired-state revision
        #[arg(long)]
        expected_revision: u64,
        /// Explicitly confirm this high-risk commissioning change
        #[arg(long)]
        confirmed: bool,
    },

    /// Disable a channel
    #[command(about = "Disable a channel")]
    Disable {
        /// Channel ID
        channel_id: u32,
        /// Compare-and-set guard for the current desired-state revision
        #[arg(long)]
        expected_revision: u64,
        /// Explicitly confirm this high-risk commissioning change
        #[arg(long)]
        confirmed: bool,
    },

    /// Show a channel's point mappings
    #[command(about = "Show a channel's point mappings")]
    Mappings {
        /// Channel ID
        channel_id: u32,
    },

    /// List points on a channel that have no protocol address mapping
    #[command(about = "List points on a channel with no protocol address mapping")]
    UnmappedPoints {
        /// Channel ID
        channel_id: u32,
    },

    /// Inject a simulated telemetry or signal value into SHM
    #[command(about = "Inject a T/S simulation value into the acquisition plane")]
    Write {
        /// Channel ID
        channel_id: u32,
        /// Point type: T | S
        #[arg(long = "type", value_parser = ["T", "S"])]
        point_type: String,
        /// Point ID (numeric or semantic)
        #[arg(long)]
        id: String,
        /// Value to write
        #[arg(long)]
        value: f64,
    },

    /// Manage points on a channel
    #[command(about = "Manage channel points (T/S/C/A)")]
    Points {
        #[command(subcommand)]
        command: PointCommands,
    },
}

#[derive(Subcommand)]
pub enum PointCommands {
    /// List all points for a channel
    #[command(about = "List points (grouped by T/S/C/A)")]
    List {
        /// Channel ID
        channel_id: u32,
        /// Filter by point type: T, S, C, or A
        #[arg(long, value_name = "TYPE")]
        r#type: Option<String>,
    },

    /// Add a point to a channel
    #[command(about = "Add a point to a channel")]
    Add {
        /// Channel ID
        channel_id: u32,
        /// Point type: T (telemetry), S (signal), C (control), A (adjustment)
        point_type: String,
        /// Point ID
        point_id: u32,
        /// Signal name
        #[arg(long)]
        name: String,
        /// Unit (e.g., V, A, kW)
        #[arg(long, default_value = "")]
        unit: String,
        /// Scale factor
        #[arg(long)]
        scale: Option<f64>,
        /// Description
        #[arg(long)]
        description: Option<String>,
        /// Data type (default: float32 for T/A, bool for S/C)
        #[arg(long)]
        data_type: Option<String>,
    },

    /// Update a point
    #[command(about = "Update a point's attributes")]
    Update {
        /// Channel ID
        channel_id: u32,
        /// Point type: T, S, C, A
        point_type: String,
        /// Point ID
        point_id: u32,
        /// Signal name
        #[arg(long)]
        name: Option<String>,
        /// Unit
        #[arg(long)]
        unit: Option<String>,
        /// Scale factor
        #[arg(long)]
        scale: Option<f64>,
        /// Description
        #[arg(long)]
        description: Option<String>,
    },

    /// Remove a point from a channel
    #[command(about = "Remove a point from a channel")]
    Remove {
        /// Channel ID
        channel_id: u32,
        /// Point type: T, S, C, A
        point_type: String,
        /// Point ID
        point_id: u32,
        /// Force deletion without confirmation
        #[arg(short, long)]
        force: bool,
    },

    /// Apply a batch of point create/update/delete operations from a JSON file
    #[command(about = "Batch create/update/delete points from a JSON file")]
    Batch {
        /// Channel ID
        channel_id: u32,
        /// Path to a JSON file: {"create":[],"update":[],"delete":[]}
        #[arg(long)]
        file: String,
    },

    /// Show the instance mapping for one point
    #[command(about = "Show the instance mapping for a single point")]
    Mapping {
        /// Channel ID
        channel_id: u32,
        /// Point type: T | S | C | A
        #[arg(value_parser = ["T", "S", "C", "A"])]
        point_type: String,
        /// Point ID
        point_id: u32,
    },
}

pub async fn handle_command(cmd: ChannelCommands, base_url: &str, json: bool) -> Result<()> {
    let client = ChannelClient::new(base_url)?;

    match cmd {
        ChannelCommands::List => {
            let channels = client.list_channels().await?;
            if json {
                crate::output::print_success(&channels);
            } else {
                println!("Channels: {}", serde_json::to_string_pretty(&channels)?);
            }
        },
        ChannelCommands::Status { channel_id } => {
            let status = client.get_channel_status(channel_id).await?;
            if json {
                crate::output::print_success(&status);
            } else {
                println!(
                    "Channel {} status: {}",
                    channel_id,
                    serde_json::to_string_pretty(&status)?
                );
            }
        },
        ChannelCommands::Reload { confirmed } => {
            let result = client.reconcile_channels(confirmed).await?;
            print_reconciliation_receipt(&result, json)?;
        },
        ChannelCommands::Health => {
            let health = client.check_health().await?;
            if json {
                crate::output::print_success(&health);
            } else {
                println!("Service health: {}", serde_json::to_string_pretty(&health)?);
            }
        },
        ChannelCommands::Create {
            name,
            protocol,
            params,
            description,
            enabled,
            id,
            confirmed,
        } => {
            let parameters: Value = serde_json::from_str(&params)
                .map_err(|e| anyhow::anyhow!("--params must be valid JSON: {}", e))?;
            let result = client
                .create_channel(
                    &name,
                    &protocol,
                    parameters,
                    description.as_deref(),
                    id,
                    enabled,
                    confirmed,
                )
                .await?;
            print_mutation_receipt(&result, json)?;
        },
        ChannelCommands::Update {
            channel_id,
            name,
            params,
            description,
            expected_revision,
            confirmed,
        } => {
            let mut body = serde_json::Map::new();
            if let Some(n) = name {
                body.insert("name".to_string(), Value::String(n));
            }
            if let Some(p) = params {
                let parameters: Value = serde_json::from_str(&p)
                    .map_err(|e| anyhow::anyhow!("--params must be valid JSON: {}", e))?;
                body.insert("parameters".to_string(), parameters);
            }
            if let Some(d) = description {
                body.insert("description".to_string(), Value::String(d));
            }
            let result = client
                .update_channel(
                    channel_id,
                    Value::Object(body),
                    confirmed,
                    Some(expected_revision),
                )
                .await?;
            print_mutation_receipt(&result, json)?;
        },
        ChannelCommands::Delete {
            channel_id,
            force,
            expected_revision,
            confirmed,
        } => {
            client.validate_mutation(confirmed, Some(expected_revision))?;
            if !force && !json {
                println!("Delete channel {}? [y/N]", channel_id);
                let mut input = String::new();
                std::io::stdin().read_line(&mut input)?;
                if !input.trim().eq_ignore_ascii_case("y") {
                    println!("Cancelled");
                    return Ok(());
                }
            }
            let result = client
                .delete_channel(channel_id, confirmed, Some(expected_revision))
                .await?;
            print_mutation_receipt(&result, json)?;
        },
        ChannelCommands::Enable {
            channel_id,
            expected_revision,
            confirmed,
        } => {
            let data = client
                .set_enabled(channel_id, true, confirmed, Some(expected_revision))
                .await?;
            print_mutation_receipt(&data, json)?;
        },
        ChannelCommands::Disable {
            channel_id,
            expected_revision,
            confirmed,
        } => {
            let data = client
                .set_enabled(channel_id, false, confirmed, Some(expected_revision))
                .await?;
            print_mutation_receipt(&data, json)?;
        },
        ChannelCommands::Mappings { channel_id } => {
            let data = client.mappings(channel_id).await?;
            crate::output::print_value(&data, json);
        },
        ChannelCommands::UnmappedPoints { channel_id } => {
            let data = client.unmapped_points(channel_id).await?;
            crate::output::print_value(&data, json);
        },
        ChannelCommands::Write {
            channel_id,
            point_type,
            id,
            value,
        } => {
            let data = client
                .write_point(channel_id, &point_type, &id, value)
                .await?;
            crate::output::print_action(
                &data,
                &format!("Wrote {value} to channel {channel_id} point {point_type}/{id}"),
                json,
            );
        },
        ChannelCommands::Points { command } => {
            let pc = PointClient::new(base_url)?;
            match command {
                PointCommands::List { channel_id, r#type } => {
                    let data = pc.list_points(channel_id, r#type.as_deref()).await?;
                    crate::output::print_value(&data, json);
                },
                PointCommands::Add {
                    channel_id,
                    point_type,
                    point_id,
                    name,
                    unit,
                    scale,
                    description,
                    data_type,
                } => {
                    let data = pc
                        .add_point(
                            channel_id,
                            &point_type,
                            point_id,
                            &name,
                            &unit,
                            scale,
                            description.as_deref(),
                            data_type.as_deref(),
                        )
                        .await?;
                    if json {
                        crate::output::print_success(&data);
                    } else {
                        println!(
                            "Point {}/{} added to channel {}",
                            point_type.to_uppercase(),
                            point_id,
                            channel_id
                        );
                    }
                },
                PointCommands::Update {
                    channel_id,
                    point_type,
                    point_id,
                    name,
                    unit,
                    scale,
                    description,
                } => {
                    let data = pc
                        .update_point(
                            channel_id,
                            &point_type,
                            point_id,
                            name.as_deref(),
                            unit.as_deref(),
                            scale,
                            description.as_deref(),
                        )
                        .await?;
                    if json {
                        crate::output::print_success(&data);
                    } else {
                        println!(
                            "Point {}/{} updated on channel {}",
                            point_type.to_uppercase(),
                            point_id,
                            channel_id
                        );
                    }
                },
                PointCommands::Remove {
                    channel_id,
                    point_type,
                    point_id,
                    force,
                } => {
                    if !force && !json {
                        println!(
                            "Delete point {}/{} from channel {}? [y/N]",
                            point_type.to_uppercase(),
                            point_id,
                            channel_id
                        );
                        let mut input = String::new();
                        std::io::stdin().read_line(&mut input)?;
                        if !input.trim().eq_ignore_ascii_case("y") {
                            println!("Cancelled");
                            return Ok(());
                        }
                    }
                    let data = pc.remove_point(channel_id, &point_type, point_id).await?;
                    if json {
                        crate::output::print_success(&data);
                    } else {
                        println!(
                            "Point {}/{} removed from channel {}",
                            point_type.to_uppercase(),
                            point_id,
                            channel_id
                        );
                    }
                },
                PointCommands::Batch { channel_id, file } => {
                    let raw = std::fs::read_to_string(&file)
                        .map_err(|e| anyhow::anyhow!("Failed to read batch file {file}: {e}"))?;
                    let body: Value = serde_json::from_str(&raw)
                        .map_err(|e| anyhow::anyhow!("Invalid JSON in batch file {file}: {e}"))?;
                    // io returns HTTP 200 even when every operation failed; the
                    // per-op outcome lives in the PointBatchResult body (succeeded,
                    // failed, errors). Print the payload rather than a bare ack so a
                    // fully-failed batch is visible without --json.
                    let data = pc.points_batch(channel_id, &body).await?;
                    crate::output::print_value(&data, json);
                },
                PointCommands::Mapping {
                    channel_id,
                    point_type,
                    point_id,
                } => {
                    let data = pc.point_mapping(channel_id, &point_type, point_id).await?;
                    crate::output::print_value(&data, json);
                },
            }
        },
    }

    Ok(())
}

#[derive(Deserialize)]
struct ChannelMutationEnvelope {
    data: ChannelMutationReceipt,
}

#[derive(Deserialize)]
struct ChannelMutationReceipt {
    channel_id: u32,
    request_id: String,
    operation: String,
    resulting_revision: u64,
    desired_enabled: bool,
    runtime_projection: String,
    reconciliation_required: bool,
    completion_audit: ChannelCompletionAuditReceipt,
    retryable: bool,
}

#[derive(Debug, Deserialize)]
struct ChannelCompletionAuditReceipt {
    status: String,
    retryable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ChannelReconciliationScopeReceipt {
    All,
    One,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ChannelRuntimeProjectionReceipt {
    Stopped,
    ActivationPending,
    Active,
    Degraded,
    Removed,
}

impl ChannelRuntimeProjectionReceipt {
    const fn reconciliation_required(self) -> bool {
        matches!(self, Self::ActivationPending | Self::Degraded)
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum ChannelDesiredStateReceipt {
    Present { revision: u64, enabled: bool },
    Absent { last_revision: Option<u64> },
}

#[derive(Debug, Deserialize)]
struct ChannelReconciliationItemReceipt {
    channel_id: u32,
    desired: ChannelDesiredStateReceipt,
    runtime_projection: ChannelRuntimeProjectionReceipt,
    reconciliation_required: bool,
}

#[derive(Debug, Deserialize)]
struct ChannelReconciliationReceipt {
    request_id: String,
    scope: ChannelReconciliationScopeReceipt,
    channel_id: Option<u32>,
    items: Vec<ChannelReconciliationItemReceipt>,
    degraded_count: usize,
    reconciliation_required: bool,
    completion_audit: ChannelCompletionAuditReceipt,
    retryable: bool,
    message: String,
}

#[derive(Deserialize)]
struct ChannelReconciliationEnvelope {
    success: bool,
    data: ChannelReconciliationReceipt,
}

fn mutation_receipt_summary(response: &Value) -> Result<String> {
    let envelope: ChannelMutationEnvelope = serde_json::from_value(response.clone())
        .map_err(|error| anyhow::anyhow!("invalid channel mutation receipt: {error}"))?;
    let receipt = envelope.data;
    if receipt.retryable || receipt.completion_audit.retryable {
        anyhow::bail!("invalid channel mutation receipt: channel commands are non-idempotent");
    }
    let desired_state = if receipt.desired_enabled {
        "enabled"
    } else {
        "disabled"
    };
    let reconciliation = if receipt.reconciliation_required {
        "reconciliation required"
    } else {
        "runtime projection reconciled"
    };
    Ok(format!(
        "Channel {} {} accepted at revision {}; desired {}; runtime projection {}; {}; completion audit {}; request {}; do not retry automatically",
        receipt.channel_id,
        receipt.operation,
        receipt.resulting_revision,
        desired_state,
        receipt.runtime_projection,
        reconciliation,
        receipt.completion_audit.status,
        receipt.request_id
    ))
}

fn print_mutation_receipt(response: &Value, json: bool) -> Result<()> {
    if json {
        crate::output::print_success(response);
    } else {
        println!("{}", mutation_receipt_summary(response)?);
    }
    Ok(())
}

fn parse_reconciliation_receipt(response: &Value) -> Result<ChannelReconciliationReceipt> {
    let envelope: ChannelReconciliationEnvelope = serde_json::from_value(response.clone())
        .map_err(|error| anyhow::anyhow!("invalid channel reconciliation receipt: {error}"))?;
    if !envelope.success {
        anyhow::bail!("invalid channel reconciliation receipt: success must be true");
    }

    let receipt = envelope.data;
    match (receipt.scope, receipt.channel_id) {
        (ChannelReconciliationScopeReceipt::All, None)
        | (ChannelReconciliationScopeReceipt::One, Some(_)) => {},
        _ => anyhow::bail!("invalid channel reconciliation receipt: scope and channel_id disagree"),
    }
    uuid::Uuid::parse_str(&receipt.request_id)
        .map_err(|error| anyhow::anyhow!("invalid channel reconciliation request_id: {error}"))?;
    if receipt.retryable || receipt.completion_audit.retryable {
        anyhow::bail!("invalid channel reconciliation receipt: reconciliation is non-idempotent");
    }
    if receipt.message.trim().is_empty() {
        anyhow::bail!("invalid channel reconciliation receipt: message must not be empty");
    }

    let mut computed_degraded_count = 0;
    let mut computed_reconciliation_required = false;
    for item in &receipt.items {
        if item.channel_id > 9_999 {
            anyhow::bail!(
                "invalid channel reconciliation receipt: channel ID {} exceeds 9999",
                item.channel_id
            );
        }
        match item.desired {
            ChannelDesiredStateReceipt::Present { revision, enabled } => {
                if revision == 0 {
                    anyhow::bail!(
                        "invalid channel reconciliation receipt: desired revision must be at least 1"
                    );
                }
                let _desired_enabled = enabled;
            },
            ChannelDesiredStateReceipt::Absent { last_revision } => {
                if last_revision == Some(0) {
                    anyhow::bail!(
                        "invalid channel reconciliation receipt: last revision must be at least 1"
                    );
                }
            },
        }
        if item.runtime_projection == ChannelRuntimeProjectionReceipt::Degraded {
            computed_degraded_count += 1;
        }
        let item_reconciliation_required = item.runtime_projection.reconciliation_required();
        if item.reconciliation_required != item_reconciliation_required {
            anyhow::bail!(
                "invalid channel reconciliation receipt: channel {} projection and reconciliation flag disagree",
                item.channel_id
            );
        }
        computed_reconciliation_required |= item_reconciliation_required;
    }
    if receipt.degraded_count != computed_degraded_count {
        anyhow::bail!(
            "invalid channel reconciliation receipt: degraded_count does not match items"
        );
    }
    if receipt.reconciliation_required != computed_reconciliation_required {
        anyhow::bail!(
            "invalid channel reconciliation receipt: reconciliation_required does not match items"
        );
    }

    Ok(receipt)
}

fn reconciliation_receipt_summary(response: &Value) -> Result<String> {
    let receipt = parse_reconciliation_receipt(response)?;
    let scope = match (receipt.scope, receipt.channel_id) {
        (ChannelReconciliationScopeReceipt::All, None) => "all channels".to_string(),
        (ChannelReconciliationScopeReceipt::One, Some(channel_id)) => {
            format!("channel {channel_id}")
        },
        _ => anyhow::bail!("invalid channel reconciliation receipt: scope and channel_id disagree"),
    };
    let reconciliation = if receipt.reconciliation_required {
        "reconciliation required"
    } else {
        "runtime projections reconciled"
    };
    Ok(format!(
        "Runtime reconciliation for {scope} accepted: {} channel(s), {} degraded; {reconciliation}; completion audit {}; request {}; do not retry automatically",
        receipt.items.len(),
        receipt.degraded_count,
        receipt.completion_audit.status,
        receipt.request_id
    ))
}

fn print_reconciliation_receipt(response: &Value, json: bool) -> Result<()> {
    let summary = reconciliation_receipt_summary(response)?;
    if json {
        crate::output::print_success(response);
    } else {
        println!("{summary}");
    }
    Ok(())
}

fn access_token_from_env() -> Option<String> {
    std::env::var("AETHER_ACCESS_TOKEN")
        .ok()
        .filter(|value| !value.trim().is_empty() && value.trim() == value)
}

// HTTP client for channel management
pub(crate) struct ChannelClient {
    client: Client,
    base_url: String,
    access_token: Option<String>,
}

impl ChannelClient {
    pub(crate) fn new(base_url: &str) -> Result<Self> {
        Ok(Self {
            client: Client::new(),
            base_url: base_url.to_string(),
            access_token: access_token_from_env(),
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

    pub(crate) async fn list_channels(&self) -> Result<Value> {
        let request = self.client.get(format!("{}/api/channels", self.base_url));
        let response = self.apply_auth(request)?.send().await?;

        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            Err(anyhow::anyhow!(
                "Failed to get channels: {} - ensure io is running (aether services start)",
                response.status()
            ))
        }
    }

    pub(crate) async fn get_channel_status(&self, channel_id: u32) -> Result<Value> {
        let request = self.client.get(format!(
            "{}/api/channels/{}/status",
            self.base_url, channel_id
        ));
        let response = self.apply_auth(request)?.send().await?;

        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            Err(anyhow::anyhow!(
                "Failed to get channel status: {}",
                response.status()
            ))
        }
    }

    pub(crate) async fn reconcile_channels(&self, confirmed: bool) -> Result<Value> {
        let request = self
            .client
            .post(format!("{}/api/channels/reconcile", self.base_url));
        let response = self
            .governed_request(request, confirmed, None)?
            .send()
            .await?;

        if response.status().is_success() {
            let value = response.json().await?;
            let receipt = parse_reconciliation_receipt(&value)?;
            if receipt.scope != ChannelReconciliationScopeReceipt::All
                || receipt.channel_id.is_some()
            {
                anyhow::bail!(
                    "invalid channel reconciliation receipt: full reconciliation must report scope=all"
                );
            }
            Ok(value)
        } else {
            Err(
                crate::output::parse_error_body("Failed to reconcile channel runtimes", response)
                    .await,
            )
        }
    }

    async fn check_health(&self) -> Result<Value> {
        let request = self.client.get(format!("{}/health", self.base_url));
        let response = self.apply_auth(request)?.send().await?;

        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            Err(anyhow::anyhow!("Service unhealthy: {}", response.status()))
        }
    }

    #[allow(clippy::disallowed_methods)] // json! macro internally uses unwrap (safe for known valid JSON)
    pub(crate) async fn create_channel(
        &self,
        name: &str,
        protocol: &str,
        parameters: Value,
        description: Option<&str>,
        id: Option<u32>,
        enabled: bool,
        confirmed: bool,
    ) -> Result<Value> {
        let mut body = serde_json::json!({
            "name": name,
            "protocol": protocol,
            "parameters": parameters,
            "enabled": enabled,
        });
        if let Some(desc) = description {
            body["description"] = Value::String(desc.to_string());
        }
        if let Some(channel_id) = id {
            body["channel_id"] = Value::Number(channel_id.into());
        }
        let request = self
            .client
            .post(format!("{}/api/channels", self.base_url))
            .json(&body);
        let response = self
            .governed_request(request, confirmed, None)?
            .send()
            .await?;

        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            Err(crate::output::parse_error_body("Failed to create channel", response).await)
        }
    }

    pub(crate) async fn update_channel(
        &self,
        channel_id: u32,
        body: Value,
        confirmed: bool,
        expected_revision: Option<u64>,
    ) -> Result<Value> {
        let request = self
            .client
            .put(format!("{}/api/channels/{}", self.base_url, channel_id))
            .json(&body);
        let response = self
            .governed_revisioned_request(request, confirmed, expected_revision)?
            .send()
            .await?;

        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            Err(crate::output::parse_error_body(
                &format!("Failed to update channel {channel_id}"),
                response,
            )
            .await)
        }
    }

    pub(crate) async fn delete_channel(
        &self,
        channel_id: u32,
        confirmed: bool,
        expected_revision: Option<u64>,
    ) -> Result<Value> {
        let request = self
            .client
            .delete(format!("{}/api/channels/{}", self.base_url, channel_id));
        let response = self
            .governed_revisioned_request(request, confirmed, expected_revision)?
            .send()
            .await?;

        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            Err(crate::output::parse_error_body(
                &format!("Failed to delete channel {channel_id}"),
                response,
            )
            .await)
        }
    }

    pub(crate) async fn set_enabled(
        &self,
        channel_id: u32,
        enabled: bool,
        confirmed: bool,
        expected_revision: Option<u64>,
    ) -> Result<Value> {
        let request = self
            .client
            .put(format!(
                "{}/api/channels/{}/enabled",
                self.base_url, channel_id
            ))
            .json(&serde_json::json!({ "enabled": enabled }));
        let resp = self
            .governed_revisioned_request(request, confirmed, expected_revision)?
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(crate::output::parse_error_body("Failed to set channel enabled state", resp).await)
        }
    }

    fn validate_mutation(&self, confirmed: bool, expected_revision: Option<u64>) -> Result<&str> {
        if !confirmed {
            anyhow::bail!("channel management requires explicit --confirmed");
        }
        if expected_revision == Some(0) {
            anyhow::bail!("--expected-revision must be at least 1");
        }
        crate::transport_security::require_secure_bearer_transport(&self.base_url)?;
        self.access_token.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "channel management requires AETHER_ACCESS_TOKEN from an authenticated Admin or Engineer session"
            )
        })
    }

    fn governed_request(
        &self,
        request: reqwest::RequestBuilder,
        confirmed: bool,
        expected_revision: Option<u64>,
    ) -> Result<reqwest::RequestBuilder> {
        self.validate_mutation(confirmed, expected_revision)?;
        let mut request = self
            .apply_auth(request)?
            .header("x-request-id", uuid::Uuid::new_v4().to_string())
            .header("x-aether-confirmed", "true");
        if let Some(revision) = expected_revision {
            request = request.header("x-aether-expected-revision", revision.to_string());
        }
        Ok(request)
    }

    fn governed_revisioned_request(
        &self,
        request: reqwest::RequestBuilder,
        confirmed: bool,
        expected_revision: Option<u64>,
    ) -> Result<reqwest::RequestBuilder> {
        let revision = expected_revision.ok_or_else(|| {
            anyhow::anyhow!(
                "online channel mutations require --expected-revision from the latest channel read"
            )
        })?;
        if revision == 0 {
            anyhow::bail!("--expected-revision must be at least 1");
        }
        self.governed_request(request, confirmed, Some(revision))
    }

    pub(crate) async fn mappings(&self, channel_id: u32) -> Result<Value> {
        let request = self.client.get(format!(
            "{}/api/channels/{}/mappings",
            self.base_url, channel_id
        ));
        let resp = self.apply_auth(request)?.send().await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(crate::output::parse_error_body("Failed to get channel mappings", resp).await)
        }
    }

    pub(crate) async fn unmapped_points(&self, channel_id: u32) -> Result<Value> {
        let request = self.client.get(format!(
            "{}/api/channels/{}/unmapped-points",
            self.base_url, channel_id
        ));
        let resp = self.apply_auth(request)?.send().await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(crate::output::parse_error_body("Failed to get unmapped points", resp).await)
        }
    }

    /// io's `WritePointRequest` flattens the point payload into the top level,
    /// so a single-point write is `{"type":..,"id":..,"value":..}`, not a nested object.
    pub(crate) async fn write_point(
        &self,
        channel_id: u32,
        point_type: &str,
        id: &str,
        value: f64,
    ) -> Result<Value> {
        if !matches!(point_type.to_ascii_uppercase().as_str(), "T" | "S") {
            anyhow::bail!(
                "direct C/A device writes are disabled; use `aether models instances action`"
            );
        }
        let body = serde_json::json!({ "type": point_type, "id": id, "value": value });
        let request = self
            .client
            .post(format!(
                "{}/api/channels/{}/write",
                self.base_url, channel_id
            ))
            .json(&body)
            .header("x-aether-confirmed", "true");
        let resp = self.apply_auth(request)?.send().await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(crate::output::parse_error_body("Failed to write point", resp).await)
        }
    }
}

// HTTP client for point management
pub(crate) struct PointClient {
    client: Client,
    base_url: String,
    access_token: Option<String>,
}

impl PointClient {
    pub(crate) fn new(base_url: &str) -> Result<Self> {
        Ok(Self {
            client: Client::new(),
            base_url: base_url.to_string(),
            access_token: access_token_from_env(),
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

    pub(crate) async fn list_points(
        &self,
        channel_id: u32,
        type_filter: Option<&str>,
    ) -> Result<Value> {
        let mut url = format!("{}/api/channels/{}/points", self.base_url, channel_id);
        if let Some(t) = type_filter {
            url.push_str(&format!("?type={}", t));
        }
        let response = self.apply_auth(self.client.get(&url))?.send().await?;
        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            Err(anyhow::anyhow!(
                "Failed to list points: {}",
                response.status()
            ))
        }
    }

    #[allow(clippy::disallowed_methods, clippy::too_many_arguments)]
    async fn add_point(
        &self,
        channel_id: u32,
        point_type: &str,
        point_id: u32,
        signal_name: &str,
        unit: &str,
        scale: Option<f64>,
        description: Option<&str>,
        data_type: Option<&str>,
    ) -> Result<Value> {
        let pt = point_type.to_uppercase();
        let default_data_type = match pt.as_str() {
            "S" | "C" => "bool",
            _ => "float32",
        };
        let body = serde_json::json!({
            "point_id": point_id,
            "signal_name": signal_name,
            "unit": unit,
            "scale": scale.unwrap_or(1.0),
            "offset": 0.0,
            "data_type": data_type.unwrap_or(default_data_type),
            "reverse": false,
            "description": description.unwrap_or("")
        });
        let url = format!(
            "{}/api/channels/{}/{}/points/{}",
            self.base_url, channel_id, pt, point_id
        );
        let request = self
            .client
            .post(&url)
            .json(&body)
            .header("x-aether-confirmed", "true");
        let response = self.apply_auth(request)?.send().await?;
        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            Err(anyhow::anyhow!(
                "Failed to add point: {} - {}",
                status,
                text
            ))
        }
    }

    #[allow(clippy::disallowed_methods)]
    async fn update_point(
        &self,
        channel_id: u32,
        point_type: &str,
        point_id: u32,
        name: Option<&str>,
        unit: Option<&str>,
        scale: Option<f64>,
        description: Option<&str>,
    ) -> Result<Value> {
        let pt = point_type.to_uppercase();
        let mut body = serde_json::Map::new();
        if let Some(n) = name {
            body.insert("signal_name".to_string(), serde_json::json!(n));
        }
        if let Some(u) = unit {
            body.insert("unit".to_string(), serde_json::json!(u));
        }
        if let Some(s) = scale {
            body.insert("scale".to_string(), serde_json::json!(s));
        }
        if let Some(d) = description {
            body.insert("description".to_string(), serde_json::json!(d));
        }
        if body.is_empty() {
            return Err(anyhow::anyhow!("No fields to update"));
        }
        let url = format!(
            "{}/api/channels/{}/{}/points/{}",
            self.base_url, channel_id, pt, point_id
        );
        let request = self
            .client
            .put(&url)
            .json(&serde_json::Value::Object(body))
            .header("x-aether-confirmed", "true");
        let response = self.apply_auth(request)?.send().await?;
        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            Err(anyhow::anyhow!(
                "Failed to update point: {} - {}",
                status,
                text
            ))
        }
    }

    async fn remove_point(
        &self,
        channel_id: u32,
        point_type: &str,
        point_id: u32,
    ) -> Result<Value> {
        let pt = point_type.to_uppercase();
        let url = format!(
            "{}/api/channels/{}/{}/points/{}",
            self.base_url, channel_id, pt, point_id
        );
        let request = self
            .client
            .delete(&url)
            .header("x-aether-confirmed", "true");
        let response = self.apply_auth(request)?.send().await?;
        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            Err(anyhow::anyhow!(
                "Failed to remove point: {} - {}",
                status,
                text
            ))
        }
    }

    pub(crate) async fn points_batch(&self, channel_id: u32, body: &Value) -> Result<Value> {
        let request = self
            .client
            .post(format!(
                "{}/api/channels/{}/points/batch",
                self.base_url, channel_id
            ))
            .json(body)
            .header("x-aether-confirmed", "true");
        let resp = self.apply_auth(request)?.send().await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(crate::output::parse_error_body("Failed to apply point batch", resp).await)
        }
    }

    pub(crate) async fn point_mapping(
        &self,
        channel_id: u32,
        point_type: &str,
        point_id: u32,
    ) -> Result<Value> {
        let request = self.client.get(format!(
            "{}/api/channels/{}/{}/points/{}/mapping",
            self.base_url, channel_id, point_type, point_id
        ));
        let resp = self.apply_auth(request)?.send().await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(crate::output::parse_error_body("Failed to get point mapping", resp).await)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ChannelClient, ChannelCommands, PointClient, mutation_receipt_summary,
        reconciliation_receipt_summary,
    };
    use clap::Parser;
    use reqwest::Client;
    use wiremock::matchers::{body_json, header, header_exists, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[derive(Parser)]
    struct ChannelCli {
        #[command(subcommand)]
        command: ChannelCommands,
    }

    #[test]
    fn create_is_disabled_by_default_and_requires_explicit_confirmation() {
        let cli = ChannelCli::try_parse_from([
            "channels",
            "create",
            "--name",
            "meter",
            "--protocol",
            "modbus",
            "--params",
            "{}",
            "--confirmed",
        ])
        .unwrap();

        match cli.command {
            ChannelCommands::Create {
                enabled, confirmed, ..
            } => {
                assert!(!enabled, "new channels must be inert by default");
                assert!(confirmed);
            },
            _ => panic!("expected create command"),
        }
    }

    #[test]
    fn lifecycle_commands_parse_expected_revision_and_confirmation() {
        let cli = ChannelCli::try_parse_from([
            "channels",
            "delete",
            "1001",
            "--force",
            "--confirmed",
            "--expected-revision",
            "7",
        ])
        .unwrap();

        match cli.command {
            ChannelCommands::Delete {
                force,
                confirmed,
                expected_revision,
                ..
            } => {
                assert!(force);
                assert!(confirmed, "--force must not replace --confirmed");
                assert_eq!(expected_revision, 7);
            },
            _ => panic!("expected delete command"),
        }
    }

    #[test]
    fn channel_mutation_cli_rejects_a_missing_expected_revision() {
        for args in [
            vec![
                "channels",
                "update",
                "1001",
                "--name",
                "meter-2",
                "--confirmed",
            ],
            vec!["channels", "delete", "1001", "--force", "--confirmed"],
            vec!["channels", "enable", "1001", "--confirmed"],
            vec!["channels", "disable", "1001", "--confirmed"],
        ] {
            let error = ChannelCli::try_parse_from(args)
                .err()
                .expect("online channel mutations must require a CAS revision");
            assert!(error.to_string().contains("--expected-revision"), "{error}");
        }
    }

    #[test]
    fn reload_requires_explicit_confirmation_in_the_cli_schema() {
        let cli = ChannelCli::try_parse_from(["channels", "reload", "--confirmed"]).unwrap();

        match cli.command {
            ChannelCommands::Reload { confirmed } => assert!(confirmed),
            _ => panic!("expected reload command"),
        }
    }

    #[test]
    fn update_enable_and_disable_parse_revision_guards_and_confirmation() {
        let cases = [
            vec![
                "channels",
                "update",
                "1001",
                "--name",
                "meter-2",
                "--confirmed",
                "--expected-revision",
                "7",
            ],
            vec![
                "channels",
                "enable",
                "1001",
                "--confirmed",
                "--expected-revision",
                "7",
            ],
            vec![
                "channels",
                "disable",
                "1001",
                "--confirmed",
                "--expected-revision",
                "7",
            ],
        ];

        for args in cases {
            let cli = ChannelCli::try_parse_from(args).unwrap();
            let (confirmed, expected_revision) = match cli.command {
                ChannelCommands::Update {
                    confirmed,
                    expected_revision,
                    ..
                }
                | ChannelCommands::Enable {
                    confirmed,
                    expected_revision,
                    ..
                }
                | ChannelCommands::Disable {
                    confirmed,
                    expected_revision,
                    ..
                } => (confirmed, expected_revision),
                _ => panic!("expected governed channel mutation"),
            };
            assert!(confirmed);
            assert_eq!(expected_revision, 7);
        }
    }

    #[test]
    fn typed_receipt_summary_exposes_degraded_runtime_and_incomplete_audit() {
        let response = serde_json::json!({
            "success": true,
            "data": {
                "channel_id": 1001,
                "request_id": "018f2a74-5700-7f42-9da4-73b247c9c001",
                "operation": "enable",
                "resulting_revision": 8,
                "desired_enabled": true,
                "runtime_projection": "degraded",
                "reconciliation_required": true,
                "completion_audit": {
                    "status": "incomplete",
                    "retryable": false,
                    "message": "terminal audit must be reconciled"
                },
                "retryable": false
            }
        });

        let summary = mutation_receipt_summary(&response).expect("typed receipt");
        assert!(summary.contains("desired enabled"), "{summary}");
        assert!(summary.contains("runtime projection degraded"), "{summary}");
        assert!(summary.contains("reconciliation required"), "{summary}");
        assert!(summary.contains("completion audit incomplete"), "{summary}");
        assert!(summary.contains("do not retry automatically"), "{summary}");
        assert!(!summary.contains("connected"), "{summary}");
    }

    #[test]
    fn typed_reconciliation_receipt_exposes_scope_degradation_and_audit_state() {
        let response = reconciliation_response();

        let summary = reconciliation_receipt_summary(&response).expect("typed receipt");
        assert!(summary.contains("all channels"), "{summary}");
        assert!(summary.contains("2 channel(s)"), "{summary}");
        assert!(summary.contains("1 degraded"), "{summary}");
        assert!(summary.contains("reconciliation required"), "{summary}");
        assert!(summary.contains("completion audit incomplete"), "{summary}");
        assert!(summary.contains("do not retry automatically"), "{summary}");
        assert!(!summary.contains("parameters"), "{summary}");
    }

    fn reconciliation_response() -> serde_json::Value {
        serde_json::json!({
            "success": true,
            "data": {
                "request_id": "018f2a74-5700-7f42-9da4-73b247c9c002",
                "scope": "all",
                "channel_id": null,
                "items": [
                    {
                        "channel_id": 7,
                        "desired": {
                            "status": "present",
                            "revision": 3,
                            "enabled": true
                        },
                        "runtime_projection": "active",
                        "reconciliation_required": false
                    },
                    {
                        "channel_id": 8,
                        "desired": {
                            "status": "absent",
                            "last_revision": 4
                        },
                        "runtime_projection": "degraded",
                        "reconciliation_required": true
                    }
                ],
                "degraded_count": 1,
                "reconciliation_required": true,
                "completion_audit": {
                    "status": "incomplete",
                    "retryable": false,
                    "message": "terminal audit must be reconciled"
                },
                "retryable": false,
                "message": "runtime reconciliation for all channels accepted"
            }
        })
    }

    #[tokio::test]
    async fn channel_mutation_fails_before_http_without_confirmation_token_or_valid_revision() {
        let server = MockServer::start().await;
        let authenticated =
            ChannelClient::with_access_token(&server.uri(), "signed-access-token").unwrap();

        let unconfirmed = [
            authenticated
                .create_channel(
                    "blocked",
                    "virtual",
                    serde_json::json!({}),
                    None,
                    None,
                    false,
                    false,
                )
                .await,
            authenticated
                .update_channel(
                    1001,
                    serde_json::json!({ "name": "blocked" }),
                    false,
                    Some(1),
                )
                .await,
            authenticated.delete_channel(1001, false, Some(1)).await,
            authenticated.set_enabled(1001, true, false, Some(1)).await,
            authenticated.set_enabled(1001, false, false, Some(1)).await,
        ];
        assert!(unconfirmed.iter().all(Result::is_err), "{unconfirmed:?}");
        assert!(
            unconfirmed.iter().all(|result| result
                .as_ref()
                .unwrap_err()
                .to_string()
                .contains("--confirmed")),
            "{unconfirmed:?}"
        );

        let unauthenticated = ChannelClient {
            client: Client::new(),
            base_url: server.uri(),
            access_token: None,
        };
        let unauthenticated_results = [
            unauthenticated
                .create_channel(
                    "blocked",
                    "virtual",
                    serde_json::json!({}),
                    None,
                    None,
                    false,
                    true,
                )
                .await,
            unauthenticated
                .update_channel(
                    1001,
                    serde_json::json!({ "name": "blocked" }),
                    true,
                    Some(1),
                )
                .await,
            unauthenticated.delete_channel(1001, true, Some(1)).await,
            unauthenticated.set_enabled(1001, true, true, Some(1)).await,
            unauthenticated
                .set_enabled(1001, false, true, Some(1))
                .await,
        ];
        assert!(
            unauthenticated_results.iter().all(Result::is_err),
            "{unauthenticated_results:?}"
        );
        assert!(
            unauthenticated_results.iter().all(|result| result
                .as_ref()
                .unwrap_err()
                .to_string()
                .contains("AETHER_ACCESS_TOKEN")),
            "{unauthenticated_results:?}"
        );

        let error = authenticated
            .update_channel(1001, serde_json::json!({ "name": "meter" }), true, Some(0))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("at least 1"), "{error}");

        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn channel_mutation_fails_before_http_without_an_expected_revision() {
        let server = MockServer::start().await;
        let client =
            ChannelClient::with_access_token(&server.uri(), "signed-access-token").unwrap();

        let results = [
            client
                .update_channel(1001, serde_json::json!({ "name": "blocked" }), true, None)
                .await,
            client.delete_channel(1001, true, None).await,
            client.set_enabled(1001, true, true, None).await,
        ];

        assert!(results.iter().all(Result::is_err), "{results:?}");
        assert!(
            results.iter().all(|result| result
                .as_ref()
                .unwrap_err()
                .to_string()
                .contains("--expected-revision")),
            "{results:?}"
        );
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn reconciliation_fails_before_http_without_confirmation_or_token() {
        let server = MockServer::start().await;
        let authenticated =
            ChannelClient::with_access_token(&server.uri(), "signed-access-token").unwrap();
        let unconfirmed = authenticated
            .reconcile_channels(false)
            .await
            .unwrap_err()
            .to_string();
        assert!(unconfirmed.contains("--confirmed"), "{unconfirmed}");

        let unauthenticated = ChannelClient {
            client: Client::new(),
            base_url: server.uri(),
            access_token: None,
        };
        let missing_token = unauthenticated
            .reconcile_channels(true)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            missing_token.contains("AETHER_ACCESS_TOKEN"),
            "{missing_token}"
        );
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[test]
    fn channel_mutation_never_attaches_bearer_to_remote_plaintext_http() {
        let plaintext =
            ChannelClient::with_access_token("http://192.0.2.10:6001", "signed-access-token")
                .unwrap();
        let error = plaintext
            .validate_mutation(true, None)
            .expect_err("remote plaintext must fail before request construction")
            .to_string();
        assert!(error.contains("non-loopback plaintext"), "{error}");

        let https =
            ChannelClient::with_access_token("https://edge.example.test", "signed-access-token")
                .unwrap();
        assert_eq!(
            https.validate_mutation(true, Some(1)).unwrap(),
            "signed-access-token"
        );
    }

    #[tokio::test]
    async fn create_posts_governed_headers_and_defaults_disabled() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/channels"))
            .and(header("authorization", "Bearer signed-access-token"))
            .and(header_exists("x-request-id"))
            .and(header("x-aether-confirmed", "true"))
            .and(body_json(serde_json::json!({
                "name": "meter",
                "protocol": "modbus",
                "parameters": {},
                "enabled": false,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            ChannelClient::with_access_token(&server.uri(), "signed-access-token").unwrap();
        client
            .create_channel(
                "meter",
                "modbus",
                serde_json::json!({}),
                None,
                None,
                false,
                true,
            )
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert!(
            requests[0]
                .headers
                .get("x-aether-expected-revision")
                .is_none(),
            "create has no prior revision and must omit the CAS header"
        );
    }

    #[tokio::test]
    async fn reconciliation_posts_the_canonical_governed_endpoint_with_a_uuid_request_id() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/channels/reconcile"))
            .and(header("authorization", "Bearer signed-access-token"))
            .and(header_exists("x-request-id"))
            .and(header("x-aether-confirmed", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(reconciliation_response()))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            ChannelClient::with_access_token(&server.uri(), "signed-access-token").unwrap();
        let response = client.reconcile_channels(true).await.unwrap();
        assert_eq!(response["data"]["scope"], "all");
        assert_eq!(response["data"]["retryable"], false);

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].url.path(), "/api/channels/reconcile");
        let request_id = requests[0]
            .headers
            .get("x-request-id")
            .expect("request ID header")
            .to_str()
            .expect("ASCII request ID");
        uuid::Uuid::parse_str(request_id).expect("UUID request ID");
    }

    #[tokio::test]
    async fn update_delete_and_enabled_forward_revision_and_governance_headers() {
        for (method_name, endpoint, body) in [
            (
                "PUT",
                "/api/channels/1001",
                Some(serde_json::json!({ "name": "meter" })),
            ),
            ("DELETE", "/api/channels/1001", None),
            (
                "PUT",
                "/api/channels/1001/enabled",
                Some(serde_json::json!({ "enabled": true })),
            ),
        ] {
            let server = MockServer::start().await;
            let mut mock = Mock::given(method(method_name))
                .and(path(endpoint))
                .and(header("authorization", "Bearer signed-access-token"))
                .and(header_exists("x-request-id"))
                .and(header("x-aether-confirmed", "true"))
                .and(header("x-aether-expected-revision", "7"));
            if let Some(body) = body.clone() {
                mock = mock.and(body_json(body));
            }
            mock.respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
                .expect(1)
                .mount(&server)
                .await;

            let client =
                ChannelClient::with_access_token(&server.uri(), "signed-access-token").unwrap();
            match endpoint {
                "/api/channels/1001" if method_name == "PUT" => {
                    client
                        .update_channel(1001, body.unwrap(), true, Some(7))
                        .await
                        .unwrap();
                },
                "/api/channels/1001" => {
                    client.delete_channel(1001, true, Some(7)).await.unwrap();
                },
                _ => {
                    client.set_enabled(1001, true, true, Some(7)).await.unwrap();
                },
            }
        }
    }

    #[tokio::test]
    async fn channel_queries_remain_available_without_an_access_token() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/channels"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .expect(1)
            .mount(&server)
            .await;

        let client = ChannelClient {
            client: Client::new(),
            base_url: server.uri(),
            access_token: None,
        };
        client.list_channels().await.unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert!(
            requests[0].headers.get("authorization").is_none(),
            "tokenless reads must go out unauthenticated"
        );
    }

    #[tokio::test]
    async fn channel_reads_attach_bearer_when_a_token_is_present() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/channels"))
            .and(header("authorization", "Bearer signed-access-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            ChannelClient::with_access_token(&server.uri(), "signed-access-token").unwrap();
        client.list_channels().await.unwrap();
    }

    #[tokio::test]
    async fn authenticated_channel_reads_refuse_remote_plaintext_transport() {
        let client =
            ChannelClient::with_access_token("http://192.0.2.10:6005", "signed-access-token")
                .unwrap();
        let error = client.list_channels().await.unwrap_err().to_string();
        assert!(error.contains("non-loopback plaintext"), "{error}");
    }

    #[tokio::test]
    async fn point_reads_attach_bearer_when_a_token_is_present() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/channels/1001/points"))
            .and(header("authorization", "Bearer signed-access-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let client = PointClient::with_access_token(&server.uri(), "signed-access-token").unwrap();
        client.list_points(1001, None).await.unwrap();
    }

    #[tokio::test]
    async fn point_reads_remain_available_without_an_access_token() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/channels/1001/points"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let client = PointClient {
            client: Client::new(),
            base_url: server.uri(),
            access_token: None,
        };
        client.list_points(1001, None).await.unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert!(
            requests[0].headers.get("authorization").is_none(),
            "tokenless reads must go out unauthenticated"
        );
    }

    #[tokio::test]
    async fn set_enabled_puts_enabled_body() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/channels/1001/enabled"))
            .and(body_json(serde_json::json!({ "enabled": false })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            ChannelClient::with_access_token(&server.uri(), "signed-access-token").unwrap();
        client
            .set_enabled(1001, false, true, Some(1))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn set_enabled_surfaces_typed_error_message_and_suggestion() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/channels/9/enabled"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "success": false,
                "error": {
                    "code": "CHANNEL_NOT_FOUND",
                    "message": "channel 9 missing",
                    "suggestion": "run aether sync"
                }
            })))
            .mount(&server)
            .await;

        let client =
            ChannelClient::with_access_token(&server.uri(), "signed-access-token").unwrap();
        let err = client
            .set_enabled(9, true, true, Some(1))
            .await
            .unwrap_err()
            .to_string();

        assert!(err.contains("channel 9 missing"), "{err}");
        assert!(err.contains("run aether sync"), "{err}");
    }

    #[tokio::test]
    async fn mappings_gets_the_mappings_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/channels/1001/mappings"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "mappings": [] })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = ChannelClient::new(&server.uri()).unwrap();
        client.mappings(1001).await.unwrap();
    }

    #[tokio::test]
    async fn unmapped_points_gets_the_unmapped_points_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/channels/1001/unmapped-points"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "points": [] })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = ChannelClient::new(&server.uri()).unwrap();
        client.unmapped_points(1001).await.unwrap();
    }

    #[tokio::test]
    async fn mappings_surfaces_typed_error_message() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/channels/9/mappings"))
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "success": false,
                "error": { "code": "CHANNEL_NOT_FOUND", "message": "channel 9 missing" }
            })))
            .mount(&server)
            .await;

        let client = ChannelClient::new(&server.uri()).unwrap();
        let err = client.mappings(9).await.unwrap_err().to_string();

        assert!(err.contains("channel 9 missing"), "{err}");
    }

    #[tokio::test]
    async fn unmapped_points_surfaces_typed_error_message() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/channels/9/unmapped-points"))
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "success": false,
                "error": { "code": "CHANNEL_NOT_FOUND", "message": "channel 9 missing" }
            })))
            .mount(&server)
            .await;

        let client = ChannelClient::new(&server.uri()).unwrap();
        let err = client.unmapped_points(9).await.unwrap_err().to_string();

        assert!(err.contains("channel 9 missing"), "{err}");
    }

    #[tokio::test]
    async fn write_point_posts_flattened_single_point_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/channels/1001/write"))
            .and(header("x-aether-confirmed", "true"))
            .and(body_json(
                serde_json::json!({ "type": "T", "id": "5", "value": 50.0 }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let client = ChannelClient::new(&server.uri()).unwrap();
        client.write_point(1001, "T", "5", 50.0).await.unwrap();
    }

    #[tokio::test]
    async fn write_point_surfaces_typed_error_message() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/channels/1001/write"))
            .respond_with(ResponseTemplate::new(503).set_body_json(serde_json::json!({
                "success": false,
                "error": { "code": "CHANNEL_OFFLINE", "message": "channel 1001 offline" }
            })))
            .mount(&server)
            .await;

        let client = ChannelClient::new(&server.uri()).unwrap();
        let err = client
            .write_point(1001, "T", "5", 1.0)
            .await
            .unwrap_err()
            .to_string();

        assert!(err.contains("channel 1001 offline"), "{err}");
    }

    #[tokio::test]
    async fn write_point_rejects_direct_device_commands_before_http() {
        let client = ChannelClient::new("http://127.0.0.1:1").unwrap();

        for point_type in ["C", "A"] {
            let error = client
                .write_point(1001, point_type, "5", 1.0)
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains("direct C/A device writes are disabled"));
        }
    }

    #[tokio::test]
    async fn points_batch_posts_body_verbatim() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/channels/1001/points/batch"))
            .and(header("x-aether-confirmed", "true"))
            .and(body_json(
                serde_json::json!({ "delete": [{ "point_id": 3 }] }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let client = PointClient::new(&server.uri()).unwrap();
        let body = serde_json::json!({ "delete": [{ "point_id": 3 }] });
        client.points_batch(1001, &body).await.unwrap();
    }

    #[tokio::test]
    async fn point_mutations_send_the_confirmed_header() {
        let server = MockServer::start().await;
        for verb in ["POST", "PUT", "DELETE"] {
            Mock::given(method(verb))
                .and(path("/api/channels/1001/T/points/5"))
                .and(header("x-aether-confirmed", "true"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
                .expect(1)
                .mount(&server)
                .await;
        }

        let client = PointClient {
            client: Client::new(),
            base_url: server.uri(),
            access_token: None,
        };
        client
            .add_point(1001, "T", 5, "voltage", "V", None, None, None)
            .await
            .unwrap();
        client
            .update_point(1001, "T", 5, Some("voltage"), None, None, None)
            .await
            .unwrap();
        client.remove_point(1001, "T", 5).await.unwrap();
    }

    #[tokio::test]
    async fn points_batch_surfaces_typed_error_message() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/channels/1001/points/batch"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "success": false,
                "error": { "code": "INVALID_POINT", "message": "point 3 not found" }
            })))
            .mount(&server)
            .await;

        let client = PointClient::new(&server.uri()).unwrap();
        let body = serde_json::json!({ "delete": [{ "point_id": 3 }] });
        let err = client
            .points_batch(1001, &body)
            .await
            .unwrap_err()
            .to_string();

        assert!(err.contains("point 3 not found"), "{err}");
    }

    #[tokio::test]
    async fn point_mapping_uses_type_in_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/channels/1001/T/points/5/mapping"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "instance_id": 3 })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = PointClient::new(&server.uri()).unwrap();
        let v = client.point_mapping(1001, "T", 5).await.unwrap();

        assert_eq!(v["instance_id"], 3);
    }

    #[tokio::test]
    async fn point_mapping_uses_a_different_type_segment() {
        // Mounted only on "C" — proves the type segment is the actual parameter,
        // not a hardcoded "T" (see teeth check 3).
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/channels/1001/C/points/5/mapping"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "instance_id": 9 })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = PointClient::new(&server.uri()).unwrap();
        let v = client.point_mapping(1001, "C", 5).await.unwrap();

        assert_eq!(v["instance_id"], 9);
    }

    #[tokio::test]
    async fn point_mapping_surfaces_typed_error_message() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/channels/1001/T/points/5/mapping"))
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "success": false,
                "error": { "code": "MAPPING_NOT_FOUND", "message": "point 5 has no mapping" }
            })))
            .mount(&server)
            .await;

        let client = PointClient::new(&server.uri()).unwrap();
        let err = client
            .point_mapping(1001, "T", 5)
            .await
            .unwrap_err()
            .to_string();

        assert!(err.contains("point 5 has no mapping"), "{err}");
    }
}
