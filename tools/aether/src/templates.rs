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

fn access_token_from_env() -> Option<String> {
    std::env::var("AETHER_ACCESS_TOKEN")
        .ok()
        .filter(|value| !value.trim().is_empty() && value.trim() == value)
}

pub(crate) struct TemplateClient {
    client: Client,
    base_url: String,
    access_token: Option<String>,
}

impl TemplateClient {
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

    pub(crate) async fn list_templates(&self, protocol: Option<&str>) -> Result<Value> {
        let mut url = format!("{}/api/templates", self.base_url);
        if let Some(p) = protocol {
            url.push_str(&format!("?protocol={}", p));
        }
        let response = self.apply_auth(self.client.get(&url))?.send().await?;
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
        let request = self
            .client
            .get(format!("{}/api/templates/{}", self.base_url, id));
        let response = self.apply_auth(request)?.send().await?;
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
        let request = self
            .client
            .post(format!(
                "{}/api/templates/{}/apply/{}",
                self.base_url, template_id, channel_id
            ))
            .json(&body)
            .header("x-aether-confirmed", "true");
        let response = self.apply_auth(request)?.send().await?;
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
        let request = self
            .client
            .delete(format!("{}/api/templates/{}", self.base_url, id))
            .header("x-aether-confirmed", "true");
        let response = self.apply_auth(request)?.send().await?;
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

#[cfg(test)]
mod tests {
    use super::TemplateClient;
    use reqwest::Client;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn template_reads_attach_bearer_when_a_token_is_present() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/templates"))
            .and(header("authorization", "Bearer signed-access-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            TemplateClient::with_access_token(&server.uri(), "signed-access-token").unwrap();
        client.list_templates(None).await.unwrap();
    }

    #[tokio::test]
    async fn template_reads_remain_available_without_an_access_token() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/templates"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .expect(1)
            .mount(&server)
            .await;

        let client = TemplateClient {
            client: Client::new(),
            base_url: server.uri(),
            access_token: None,
        };
        client.list_templates(None).await.unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert!(
            requests[0].headers.get("authorization").is_none(),
            "tokenless reads must go out unauthenticated"
        );
    }

    #[tokio::test]
    async fn template_mutations_send_the_confirmed_header() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/templates/from-channel/7"))
            .and(header("x-aether-confirmed", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/templates/1/apply/7"))
            .and(header("x-aether-confirmed", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/api/templates/1"))
            .and(header("x-aether-confirmed", "true"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let client = TemplateClient {
            client: Client::new(),
            base_url: server.uri(),
            access_token: None,
        };
        client.snapshot_channel(7, "snap", None).await.unwrap();
        client.apply_template(1, 7, false, None).await.unwrap();
        client.delete_template(1).await.unwrap();
    }

    #[tokio::test]
    async fn authenticated_template_reads_refuse_remote_plaintext_transport() {
        let client =
            TemplateClient::with_access_token("http://192.0.2.10:6005", "signed-access-token")
                .unwrap();
        let error = client.list_templates(None).await.unwrap_err().to_string();
        assert!(error.contains("non-loopback plaintext"), "{error}");
    }
}
