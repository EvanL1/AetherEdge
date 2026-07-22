//! Rule management module
//!
//! Provides functionality to manage business rules via HTTP API

use anyhow::Result;
use clap::Subcommand;
use reqwest::Client;
use serde_json::Value;

#[derive(Subcommand)]
pub enum RuleCommands {
    /// List all rules
    #[command(about = "List all configured business rules")]
    List {
        /// Show only enabled rules
        #[arg(long)]
        enabled: bool,
    },

    /// Get rule details
    #[command(about = "Show detailed information about a rule")]
    Get {
        /// Rule ID
        rule_id: i64,
    },

    /// Enable a rule
    #[command(about = "Enable a business rule")]
    Enable {
        /// Rule ID
        rule_id: i64,
        /// Confirm this high-risk rule-policy mutation
        #[arg(long)]
        confirmed: bool,
    },

    /// Disable a rule
    #[command(about = "Disable a business rule")]
    Disable {
        /// Rule ID
        rule_id: i64,
        /// Confirm this high-risk rule-policy mutation
        #[arg(long)]
        confirmed: bool,
    },

    /// Execute a rule
    #[command(about = "Execute a rule (evaluate and execute if conditions met)")]
    Execute {
        /// Rule ID
        rule_id: i64,
        /// Confirm that executing the rule may dispatch real device commands
        #[arg(long)]
        confirmed: bool,
    },

    /// Create a new rule (empty shell, configure with 'update')
    #[command(about = "Create a new business rule")]
    Create {
        /// Rule name
        #[arg(long)]
        name: String,
        /// Rule description
        #[arg(long)]
        description: Option<String>,
        /// Confirm this high-risk rule-policy mutation
        #[arg(long)]
        confirmed: bool,
    },

    /// Update rule metadata and/or flow logic
    #[command(about = "Update rule metadata and/or flow logic")]
    Update {
        /// Rule ID
        rule_id: i64,
        /// New rule name
        #[arg(long)]
        name: Option<String>,
        /// New description
        #[arg(long)]
        description: Option<String>,
        /// Enable or disable the rule
        #[arg(long)]
        enabled: Option<bool>,
        /// Rule priority (lower = higher priority)
        #[arg(long)]
        priority: Option<u32>,
        /// Cooldown between executions in milliseconds
        #[arg(long)]
        cooldown_ms: Option<u64>,
        /// Path to Vue Flow JSON file (use '-' for stdin)
        #[arg(long = "flow-json")]
        flow_json: Option<String>,
        /// Confirm this high-risk rule-policy mutation
        #[arg(long)]
        confirmed: bool,
    },

    /// Delete a rule
    #[command(about = "Delete a business rule")]
    Delete {
        /// Rule ID
        rule_id: i64,
        /// Skip confirmation prompt
        #[arg(short, long)]
        force: bool,
        /// Confirm this high-risk rule-policy mutation (`--force` only skips the prompt)
        #[arg(long)]
        confirmed: bool,
    },
}

pub async fn handle_command(cmd: RuleCommands, base_url: &str, json: bool) -> Result<()> {
    let client = RuleClient::new(base_url)?;

    match cmd {
        RuleCommands::List { enabled } => {
            let rules = client.list_rules().await?;

            let rules = if enabled {
                if let serde_json::Value::Array(arr) = rules {
                    let filtered = arr
                        .into_iter()
                        .filter(|r| r.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false))
                        .collect::<Vec<_>>();
                    serde_json::Value::from(filtered)
                } else {
                    rules
                }
            } else {
                rules
            };

            if json {
                crate::output::print_success(&rules);
            } else {
                println!("Rules: {}", serde_json::to_string_pretty(&rules)?);
            }
        },
        RuleCommands::Get { rule_id } => {
            let rule = client.get_rule(rule_id).await?;
            if json {
                crate::output::print_success(&rule);
            } else {
                println!(
                    "Rule '{}': {}",
                    rule_id,
                    serde_json::to_string_pretty(&rule)?
                );
            }
        },
        RuleCommands::Enable { rule_id, confirmed } => {
            client.enable_rule(rule_id, confirmed).await?;
            if json {
                crate::output::print_ok();
            } else {
                println!("Rule '{}' enabled", rule_id);
            }
        },
        RuleCommands::Disable { rule_id, confirmed } => {
            client.disable_rule(rule_id, confirmed).await?;
            if json {
                crate::output::print_ok();
            } else {
                println!("Rule '{}' disabled", rule_id);
            }
        },
        RuleCommands::Execute { rule_id, confirmed } => {
            let result = client.execute_rule(rule_id, confirmed).await?;
            if json {
                crate::output::print_success(&result);
            } else {
                println!(
                    "Rule '{}' evaluated; selected commands were accepted or rejected by the local command plane: {}",
                    rule_id,
                    serde_json::to_string_pretty(&result)?
                );
            }
        },
        RuleCommands::Create {
            name,
            description,
            confirmed,
        } => {
            let result = client
                .create_rule(&name, description.as_deref(), confirmed)
                .await?;
            if json {
                crate::output::print_success(&result);
            } else {
                println!("Rule created: {}", serde_json::to_string_pretty(&result)?);
            }
        },
        RuleCommands::Update {
            rule_id,
            name,
            description,
            enabled,
            priority,
            cooldown_ms,
            flow_json,
            confirmed,
        } => {
            let mut body = serde_json::Map::new();
            if let Some(n) = name {
                body.insert("name".into(), Value::String(n));
            }
            if let Some(d) = description {
                body.insert("description".into(), Value::String(d));
            }
            if let Some(e) = enabled {
                body.insert("enabled".into(), Value::Bool(e));
            }
            if let Some(p) = priority {
                body.insert("priority".into(), Value::from(p));
            }
            if let Some(c) = cooldown_ms {
                body.insert("cooldown_ms".into(), Value::from(c));
            }
            if let Some(path) = flow_json {
                let content = if path == "-" {
                    let mut buf = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
                    buf
                } else {
                    std::fs::read_to_string(&path)
                        .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", path, e))?
                };
                let flow: Value = serde_json::from_str(&content)
                    .map_err(|e| anyhow::anyhow!("Invalid JSON in flow file: {}", e))?;
                body.insert("flow_json".into(), flow);
            }
            if body.is_empty() {
                anyhow::bail!(
                    "No fields to update. Use --name, --description, --enabled, --priority, --cooldown-ms, or --flow-json"
                );
            }
            let result = client
                .update_rule(rule_id, Value::Object(body), confirmed)
                .await?;
            if json {
                crate::output::print_success(&result);
            } else {
                println!("Rule {} updated", rule_id);
            }
        },
        RuleCommands::Delete {
            rule_id,
            force,
            confirmed,
        } => {
            if !force && !json {
                println!("Delete rule {}? [y/N]", rule_id);
                let mut input = String::new();
                std::io::stdin().read_line(&mut input)?;
                if !input.trim().eq_ignore_ascii_case("y") {
                    println!("Cancelled");
                    return Ok(());
                }
            }
            client.delete_rule(rule_id, confirmed).await?;
            if json {
                crate::output::print_ok();
            } else {
                println!("Rule {} deleted", rule_id);
            }
        },
    }

    Ok(())
}

// HTTP client for rule management
pub(crate) struct RuleClient {
    client: Client,
    base_url: String,
    access_token: Option<String>,
}

impl RuleClient {
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

    pub(crate) async fn list_rules(&self) -> Result<Value> {
        let request = self.client.get(format!("{}/api/rules", self.base_url));
        let response = self.apply_auth(request)?.send().await?;

        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            Err(anyhow::anyhow!(
                "Failed to list rules: {} - ensure automation is running (aether services start)",
                response.status()
            ))
        }
    }

    pub(crate) async fn get_rule(&self, rule_id: i64) -> Result<Value> {
        let request = self
            .client
            .get(format!("{}/api/rules/{}", self.base_url, rule_id));
        let response = self.apply_auth(request)?.send().await?;

        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            Err(anyhow::anyhow!("Failed to get rule: {}", response.status()))
        }
    }

    pub(crate) async fn enable_rule(&self, rule_id: i64, confirmed: bool) -> Result<Value> {
        self.require_rule_management_auth(confirmed)?;
        let expected_revision = self.current_rules_revision().await?;
        let request = self
            .client
            .post(format!("{}/api/rules/{}/enable", self.base_url, rule_id))
            .header("x-request-id", uuid::Uuid::new_v4().to_string())
            .header("x-aether-confirmed", "true")
            .json(&serde_json::json!({
                "confirmed": true,
                "expected_revision": expected_revision
            }));
        let response = self.apply_auth(request)?.send().await?;

        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            Err(anyhow::anyhow!(
                "Failed to enable rule: {}",
                response.status()
            ))
        }
    }

    pub(crate) async fn disable_rule(&self, rule_id: i64, confirmed: bool) -> Result<Value> {
        self.require_rule_management_auth(confirmed)?;
        let expected_revision = self.current_rules_revision().await?;
        let request = self
            .client
            .post(format!("{}/api/rules/{}/disable", self.base_url, rule_id))
            .header("x-request-id", uuid::Uuid::new_v4().to_string())
            .header("x-aether-confirmed", "true")
            .json(&serde_json::json!({
                "confirmed": true,
                "expected_revision": expected_revision
            }));
        let response = self.apply_auth(request)?.send().await?;

        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            Err(anyhow::anyhow!(
                "Failed to disable rule: {}",
                response.status()
            ))
        }
    }

    #[allow(clippy::disallowed_methods)] // json! macro internally uses unwrap (safe for known valid JSON)
    pub(crate) async fn execute_rule(&self, rule_id: i64, confirmed: bool) -> Result<Value> {
        self.require_rule_execution_auth(confirmed)?;
        let request = self
            .client
            .post(format!("{}/api/rules/{}/execute", self.base_url, rule_id))
            .header("x-request-id", uuid::Uuid::new_v4().to_string())
            .header("x-aether-confirmed", "true")
            .json(&serde_json::json!({ "confirmed": confirmed }));
        let response = self.apply_auth(request)?.send().await?;

        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            Err(anyhow::anyhow!(
                "Failed to execute rule: {}",
                response.status()
            ))
        }
    }

    #[allow(clippy::disallowed_methods)]
    pub(crate) async fn create_rule(
        &self,
        name: &str,
        description: Option<&str>,
        confirmed: bool,
    ) -> Result<Value> {
        self.require_rule_management_auth(confirmed)?;
        let expected_revision = self.current_rules_revision().await?;
        let mut body = serde_json::Map::new();
        body.insert("name".into(), Value::String(name.to_string()));
        body.insert("confirmed".into(), Value::Bool(true));
        body.insert("expected_revision".into(), Value::from(expected_revision));
        if let Some(d) = description {
            body.insert("description".into(), Value::String(d.to_string()));
        }
        let request = self
            .client
            .post(format!("{}/api/rules", self.base_url))
            .header("x-request-id", uuid::Uuid::new_v4().to_string())
            .header("x-aether-confirmed", "true")
            .json(&Value::Object(body));
        let response = self.apply_auth(request)?.send().await?;

        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            Err(anyhow::anyhow!(
                "Failed to create rule: {}",
                response.status()
            ))
        }
    }

    pub(crate) async fn update_rule(
        &self,
        rule_id: i64,
        mut body: Value,
        confirmed: bool,
    ) -> Result<Value> {
        self.require_rule_management_auth(confirmed)?;
        let expected_revision = self.current_rules_revision().await?;
        let fields = body
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("rule update body must be a JSON object"))?;
        fields.insert("confirmed".to_string(), Value::Bool(true));
        fields.insert(
            "expected_revision".to_string(),
            Value::from(expected_revision),
        );
        let request = self
            .client
            .put(format!("{}/api/rules/{}", self.base_url, rule_id))
            .header("x-request-id", uuid::Uuid::new_v4().to_string())
            .header("x-aether-confirmed", "true")
            .json(&body);
        let response = self.apply_auth(request)?.send().await?;

        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            Err(anyhow::anyhow!(
                "Failed to update rule {}: {}",
                rule_id,
                response.status()
            ))
        }
    }

    pub(crate) async fn delete_rule(&self, rule_id: i64, confirmed: bool) -> Result<Value> {
        self.require_rule_management_auth(confirmed)?;
        let expected_revision = self.current_rules_revision().await?;
        let request = self
            .client
            .delete(format!("{}/api/rules/{}", self.base_url, rule_id))
            .header("x-request-id", uuid::Uuid::new_v4().to_string())
            .header("x-aether-confirmed", "true")
            .json(&serde_json::json!({
                "confirmed": true,
                "expected_revision": expected_revision
            }));
        let response = self.apply_auth(request)?.send().await?;

        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            Err(anyhow::anyhow!(
                "Failed to delete rule {}: {}",
                rule_id,
                response.status()
            ))
        }
    }

    fn require_rule_management_auth(&self, confirmed: bool) -> Result<()> {
        if !confirmed {
            return Err(anyhow::anyhow!(
                "rule management requires explicit --confirmed"
            ));
        }
        crate::transport_security::require_secure_bearer_transport(&self.base_url)?;
        if self.access_token.is_none() {
            return Err(anyhow::anyhow!(
                "rule management requires AETHER_ACCESS_TOKEN from an authenticated Admin or Engineer session"
            ));
        }
        Ok(())
    }

    async fn current_rules_revision(&self) -> Result<u64> {
        let request = self
            .client
            .get(format!("{}/api/rules?page=1&page_size=1", self.base_url));
        let response = self.apply_auth(request)?.send().await?;
        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Failed to read automation-rules revision: {}",
                response.status()
            ));
        }
        let value = response
            .headers()
            .get("x-aether-configuration-revision")
            .or_else(|| response.headers().get(reqwest::header::ETAG))
            .ok_or_else(|| anyhow::anyhow!("rules query did not return a revision header"))?
            .to_str()?
            .trim_matches('"')
            .parse::<u64>()?;
        if value == 0 || value >= i64::MAX as u64 {
            return Err(anyhow::anyhow!("invalid automation-rules revision {value}"));
        }
        Ok(value)
    }

    fn require_rule_execution_auth(&self, confirmed: bool) -> Result<()> {
        if !confirmed {
            return Err(anyhow::anyhow!(
                "manual rule execution requires explicit confirmation"
            ));
        }
        crate::transport_security::require_secure_bearer_transport(&self.base_url)?;
        if self.access_token.is_none() {
            return Err(anyhow::anyhow!(
                "manual rule execution requires AETHER_ACCESS_TOKEN from an authenticated Admin or Engineer session"
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::RuleClient;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn list_rules_attaches_bearer_when_access_token_is_present() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/rules"))
            .and(header("authorization", "Bearer signed-access-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            RuleClient::with_access_token(&server.uri(), "signed-access-token").expect("client");
        client.list_rules().await.expect("authenticated list");
    }

    #[tokio::test]
    async fn list_rules_stays_unauthenticated_without_access_token() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/rules"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .expect(1)
            .mount(&server)
            .await;

        let client = RuleClient {
            client: reqwest::Client::new(),
            base_url: server.uri(),
            access_token: None,
        };
        client.list_rules().await.expect("tokenless list");

        let requests = server.received_requests().await.expect("received requests");
        assert!(
            requests
                .iter()
                .all(|request| !request.headers.contains_key("authorization")),
            "tokenless reads must not carry an authorization header"
        );
    }

    #[tokio::test]
    async fn execute_rule_sends_the_gateway_confirmation_header() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/rules/7/execute"))
            .and(header("authorization", "Bearer signed-access-token"))
            .and(header("x-aether-confirmed", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            RuleClient::with_access_token(&server.uri(), "signed-access-token").expect("client");
        client
            .execute_rule(7, true)
            .await
            .expect("governed execute");
    }

    #[test]
    fn bearer_writes_reject_remote_plaintext_before_token_access() {
        let client = RuleClient {
            client: reqwest::Client::new(),
            base_url: "http://192.0.2.10:6002".to_string(),
            access_token: None,
        };

        for result in [
            client.require_rule_management_auth(true),
            client.require_rule_execution_auth(true),
        ] {
            let error = result.expect_err("remote plaintext must fail closed");
            assert!(error.to_string().contains("refusing to send"), "{error:#}");
        }
    }
}
