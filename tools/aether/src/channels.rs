//! Channel management module
//!
//! Provides functionality to manage communication channels via HTTP API

use anyhow::Result;
use clap::Subcommand;
use reqwest::Client;
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

    /// Reload channel configuration
    #[command(about = "Reload all channel configurations")]
    Reload,

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
        /// Start channel immediately (default: true)
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        enabled: bool,
        /// Override channel ID (auto-assigned if omitted)
        #[arg(long)]
        id: Option<u32>,
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
    },

    /// Delete a channel (cascades: points, mappings, routing)
    #[command(about = "Delete a channel and cascade-remove its points, mappings, and routing")]
    Delete {
        /// Channel ID to delete
        channel_id: u32,
        /// Skip confirmation prompt
        #[arg(short, long)]
        force: bool,
    },

    /// Enable a channel
    #[command(about = "Enable a channel")]
    Enable {
        /// Channel ID
        channel_id: u32,
    },

    /// Disable a channel
    #[command(about = "Disable a channel")]
    Disable {
        /// Channel ID
        channel_id: u32,
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
        ChannelCommands::Reload => {
            client.reload_config().await?;
            if json {
                crate::output::print_ok();
            } else {
                println!("Configuration reloaded");
            }
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
                )
                .await?;
            if json {
                crate::output::print_success(&result);
            } else {
                println!(
                    "Channel created: {}",
                    result
                        .get("data")
                        .and_then(|d| d.get("channel_id"))
                        .map(|v| v.to_string())
                        .unwrap_or_default()
                );
            }
        },
        ChannelCommands::Update {
            channel_id,
            name,
            params,
            description,
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
                .update_channel(channel_id, Value::Object(body))
                .await?;
            if json {
                crate::output::print_success(&result);
            } else {
                println!("Channel {} updated", channel_id);
            }
        },
        ChannelCommands::Delete { channel_id, force } => {
            if !force && !json {
                println!("Delete channel {}? [y/N]", channel_id);
                let mut input = String::new();
                std::io::stdin().read_line(&mut input)?;
                if !input.trim().eq_ignore_ascii_case("y") {
                    println!("Cancelled");
                    return Ok(());
                }
            }
            client.delete_channel(channel_id).await?;
            if json {
                crate::output::print_ok();
            } else {
                println!("Channel {} deleted", channel_id);
            }
        },
        ChannelCommands::Enable { channel_id } => {
            let data = client.set_enabled(channel_id, true).await?;
            crate::output::print_action(&data, &format!("Channel {channel_id} enabled"), json);
        },
        ChannelCommands::Disable { channel_id } => {
            let data = client.set_enabled(channel_id, false).await?;
            crate::output::print_action(&data, &format!("Channel {channel_id} disabled"), json);
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

// HTTP client for channel management
pub(crate) struct ChannelClient {
    client: Client,
    base_url: String,
}

impl ChannelClient {
    pub(crate) fn new(base_url: &str) -> Result<Self> {
        Ok(Self {
            client: Client::new(),
            base_url: base_url.to_string(),
        })
    }

    pub(crate) async fn list_channels(&self) -> Result<Value> {
        let response = self
            .client
            .get(format!("{}/api/channels", self.base_url))
            .send()
            .await?;

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
        let response = self
            .client
            .get(format!(
                "{}/api/channels/{}/status",
                self.base_url, channel_id
            ))
            .send()
            .await?;

        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            Err(anyhow::anyhow!(
                "Failed to get channel status: {}",
                response.status()
            ))
        }
    }

    async fn reload_config(&self) -> Result<()> {
        let response = self
            .client
            .post(format!("{}/api/channels/reload", self.base_url))
            .send()
            .await?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "Failed to reload config: {}",
                response.status()
            ))
        }
    }

    async fn check_health(&self) -> Result<Value> {
        let response = self
            .client
            .get(format!("{}/health", self.base_url))
            .send()
            .await?;

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
        let response = self
            .client
            .post(format!("{}/api/channels", self.base_url))
            .json(&body)
            .send()
            .await?;

        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            Err(anyhow::anyhow!(
                "Failed to create channel: {} - {}",
                status,
                text
            ))
        }
    }

    pub(crate) async fn update_channel(&self, channel_id: u32, body: Value) -> Result<Value> {
        let response = self
            .client
            .put(format!("{}/api/channels/{}", self.base_url, channel_id))
            .json(&body)
            .send()
            .await?;

        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            Err(anyhow::anyhow!(
                "Failed to update channel {}: {} - {}",
                channel_id,
                status,
                text
            ))
        }
    }

    pub(crate) async fn delete_channel(&self, channel_id: u32) -> Result<Value> {
        let response = self
            .client
            .delete(format!("{}/api/channels/{}", self.base_url, channel_id))
            .send()
            .await?;

        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            Err(anyhow::anyhow!(
                "Failed to delete channel {}: {} - {}",
                channel_id,
                status,
                text
            ))
        }
    }

    pub(crate) async fn set_enabled(&self, channel_id: u32, enabled: bool) -> Result<Value> {
        let resp = self
            .client
            .put(format!(
                "{}/api/channels/{}/enabled",
                self.base_url, channel_id
            ))
            .json(&serde_json::json!({ "enabled": enabled }))
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(crate::output::parse_error_body("Failed to set channel enabled state", resp).await)
        }
    }

    pub(crate) async fn mappings(&self, channel_id: u32) -> Result<Value> {
        let resp = self
            .client
            .get(format!(
                "{}/api/channels/{}/mappings",
                self.base_url, channel_id
            ))
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(crate::output::parse_error_body("Failed to get channel mappings", resp).await)
        }
    }

    pub(crate) async fn unmapped_points(&self, channel_id: u32) -> Result<Value> {
        let resp = self
            .client
            .get(format!(
                "{}/api/channels/{}/unmapped-points",
                self.base_url, channel_id
            ))
            .send()
            .await?;

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
        let resp = self
            .client
            .post(format!(
                "{}/api/channels/{}/write",
                self.base_url, channel_id
            ))
            .json(&body)
            .send()
            .await?;

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
}

impl PointClient {
    pub(crate) fn new(base_url: &str) -> Result<Self> {
        Ok(Self {
            client: Client::new(),
            base_url: base_url.to_string(),
        })
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
        let response = self.client.get(&url).send().await?;
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
        let response = self.client.post(&url).json(&body).send().await?;
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
        let response = self
            .client
            .put(&url)
            .json(&serde_json::Value::Object(body))
            .send()
            .await?;
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
        let response = self.client.delete(&url).send().await?;
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
        let resp = self
            .client
            .post(format!(
                "{}/api/channels/{}/points/batch",
                self.base_url, channel_id
            ))
            .json(body)
            .send()
            .await?;

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
        let resp = self
            .client
            .get(format!(
                "{}/api/channels/{}/{}/points/{}/mapping",
                self.base_url, channel_id, point_type, point_id
            ))
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(crate::output::parse_error_body("Failed to get point mapping", resp).await)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ChannelClient, PointClient};
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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

        let client = ChannelClient::new(&server.uri()).unwrap();
        client.set_enabled(1001, false).await.unwrap();
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

        let client = ChannelClient::new(&server.uri()).unwrap();
        let err = client.set_enabled(9, true).await.unwrap_err().to_string();

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
