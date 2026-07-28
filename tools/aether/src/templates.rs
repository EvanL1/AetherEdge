//! Template management module
//!
//! Provides functionality to manage channel templates via HTTP API

use anyhow::Result;
use clap::Subcommand;
use serde_json::Value;

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
}

pub async fn handle_command(cmd: TemplateCommands, base_url: &str, json: bool) -> Result<()> {
    let client = TemplateClient::new(base_url)?;

    match cmd {
        TemplateCommands::List { protocol } => {
            handle_list(&client, protocol.as_deref(), json).await?
        },
        TemplateCommands::Get { id } => handle_get(&client, id, json).await?,
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

crate::api_client::authenticated_api_client!(pub(crate) TemplateClient);

impl TemplateClient {
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
}

#[cfg(test)]
mod tests {
    use super::TemplateClient;
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

        let client = TemplateClient::without_access_token(&server.uri());
        client.list_templates(None).await.unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert!(
            requests[0].headers.get("authorization").is_none(),
            "tokenless reads must go out unauthenticated"
        );
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
