//! Template management module
//!
//! Provides functionality to manage channel templates via HTTP API

use anyhow::Result;
use clap::Subcommand;
use reqwest::Client;
use serde_json::Value;
use tracing::info;

#[derive(Subcommand)]
pub enum TemplateCommands {
    /// List all templates
    #[command(about = "List all channel templates")]
    List {
        /// Filter by protocol type
        #[arg(short, long)]
        protocol: Option<String>,
    },

    /// Get template details
    #[command(about = "Show detailed information about a template")]
    Get {
        /// Template ID
        id: i64,
    },

    /// Create template from channel snapshot
    #[command(about = "Snapshot a channel's configuration as a reusable template")]
    Snapshot {
        /// Source channel ID to snapshot
        channel_id: u32,
        /// Template name
        #[arg(short, long)]
        name: String,
        /// Template description
        #[arg(short, long)]
        description: Option<String>,
    },

    /// Apply template to a channel
    #[command(about = "Apply a template to a target channel")]
    Apply {
        /// Template ID
        template_id: i64,
        /// Target channel ID
        channel_id: u32,
        /// Clear existing points before applying
        #[arg(long)]
        clear: bool,
        /// Override slave ID for Modbus
        #[arg(long)]
        slave_id: Option<u8>,
    },

    /// Delete a template
    #[command(about = "Delete a channel template")]
    Delete {
        /// Template ID
        id: i64,
        /// Force deletion without confirmation
        #[arg(short, long)]
        force: bool,
    },
}

pub async fn handle_command(cmd: TemplateCommands, base_url: &str, json: bool) -> Result<()> {
    let client = TemplateClient::new(base_url)?;

    match cmd {
        TemplateCommands::List { protocol } => {
            handle_list(&client, protocol.as_deref(), json).await?
        },
        TemplateCommands::Get { id } => handle_get(&client, id, json).await?,
        TemplateCommands::Snapshot {
            channel_id,
            name,
            description,
        } => {
            handle_snapshot(&client, channel_id, &name, description, json).await?;
        },
        TemplateCommands::Apply {
            template_id,
            channel_id,
            clear,
            slave_id,
        } => {
            handle_apply(&client, template_id, channel_id, clear, slave_id, json).await?;
        },
        TemplateCommands::Delete { id, force } => handle_delete(&client, id, force, json).await?,
    }

    Ok(())
}

async fn handle_list(client: &TemplateClient, protocol: Option<&str>, json: bool) -> Result<()> {
    let templates = client.list_templates(protocol).await?;
    if json {
        crate::output::print_success(&templates);
    } else {
        println!("Templates: {}", serde_json::to_string_pretty(&templates)?);
    }
    Ok(())
}

async fn handle_get(client: &TemplateClient, id: i64, json: bool) -> Result<()> {
    let template = client.get_template(id).await?;
    if json {
        crate::output::print_success(&template);
    } else {
        println!(
            "Template {}: {}",
            id,
            serde_json::to_string_pretty(&template)?
        );
    }
    Ok(())
}

async fn handle_snapshot(
    client: &TemplateClient,
    channel_id: u32,
    name: &str,
    description: Option<String>,
    json: bool,
) -> Result<()> {
    let result = client
        .snapshot_channel(channel_id, name, description)
        .await?;
    if json {
        crate::output::print_success(&result);
    } else {
        println!(
            "Template created from channel {}: {}",
            channel_id,
            serde_json::to_string_pretty(&result)?
        );
    }
    Ok(())
}

async fn handle_apply(
    client: &TemplateClient,
    template_id: i64,
    channel_id: u32,
    clear: bool,
    slave_id: Option<u8>,
    json: bool,
) -> Result<()> {
    let result = client
        .apply_template(template_id, channel_id, clear, slave_id)
        .await?;
    if json {
        crate::output::print_success(&result);
    } else {
        println!(
            "Template {} applied to channel {}: {}",
            template_id,
            channel_id,
            serde_json::to_string_pretty(&result)?
        );
    }
    Ok(())
}

async fn handle_delete(client: &TemplateClient, id: i64, force: bool, json: bool) -> Result<()> {
    if !force && !json {
        println!("Delete template {}? [y/N]", id);
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled");
            return Ok(());
        }
    }
    client.delete_template(id).await?;
    if json {
        crate::output::print_ok();
    } else {
        info!("Template {} deleted", id);
    }
    Ok(())
}

pub(crate) struct TemplateClient {
    client: Client,
    base_url: String,
}

impl TemplateClient {
    pub(crate) fn new(base_url: &str) -> Result<Self> {
        Ok(Self {
            client: Client::new(),
            base_url: base_url.to_string(),
        })
    }

    pub(crate) async fn list_templates(&self, protocol: Option<&str>) -> Result<Value> {
        let mut url = format!("{}/api/templates", self.base_url);
        if let Some(p) = protocol {
            url.push_str(&format!("?protocol={}", p));
        }
        let response = self.client.get(&url).send().await?;
        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            Err(anyhow::anyhow!(
                "Failed to list templates: {} - ensure io is running",
                response.status()
            ))
        }
    }

    async fn get_template(&self, id: i64) -> Result<Value> {
        let response = self
            .client
            .get(format!("{}/api/templates/{}", self.base_url, id))
            .send()
            .await?;
        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            Err(anyhow::anyhow!(
                "Failed to get template: {}",
                response.status()
            ))
        }
    }

    #[allow(clippy::disallowed_methods)]
    async fn snapshot_channel(
        &self,
        channel_id: u32,
        name: &str,
        description: Option<String>,
    ) -> Result<Value> {
        let url = format!(
            "{}/api/templates/from-channel/{}",
            self.base_url, channel_id
        );
        let mut body = serde_json::json!({ "name": name });
        if let Some(d) = description {
            body["description"] = serde_json::json!(d);
        }
        let response = self.client.post(&url).json(&body).send().await?;
        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            Err(anyhow::anyhow!(
                "Failed to snapshot channel: {} - {}",
                status,
                text
            ))
        }
    }

    #[allow(clippy::disallowed_methods)]
    async fn apply_template(
        &self,
        template_id: i64,
        channel_id: u32,
        clear: bool,
        slave_id: Option<u8>,
    ) -> Result<Value> {
        let mut body = serde_json::json!({});
        if clear {
            body["clear_existing"] = serde_json::json!(true);
        }
        if let Some(sid) = slave_id {
            body["slave_id_override"] = serde_json::json!(sid);
        }
        let response = self
            .client
            .post(format!(
                "{}/api/templates/{}/apply/{}",
                self.base_url, template_id, channel_id
            ))
            .json(&body)
            .send()
            .await?;
        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            let status = response.status();
            let body_text = response.text().await.unwrap_or_default();
            Err(anyhow::anyhow!(
                "Failed to apply template: {} - {}",
                status,
                body_text
            ))
        }
    }

    async fn delete_template(&self, id: i64) -> Result<()> {
        let response = self
            .client
            .delete(format!("{}/api/templates/{}", self.base_url, id))
            .send()
            .await?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "Failed to delete template: {}",
                response.status()
            ))
        }
    }
}
