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
    },

    /// Disable a rule
    #[command(about = "Disable a business rule")]
    Disable {
        /// Rule ID
        rule_id: i64,
    },

    /// Execute a rule
    #[command(about = "Execute a rule (evaluate and execute if conditions met)")]
    Execute {
        /// Rule ID
        rule_id: i64,
        /// Force execution even if conditions not met
        #[arg(short, long)]
        force: bool,
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
    },

    /// Delete a rule
    #[command(about = "Delete a business rule")]
    Delete {
        /// Rule ID
        rule_id: i64,
        /// Skip confirmation prompt
        #[arg(short, long)]
        force: bool,
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
        RuleCommands::Enable { rule_id } => {
            client.enable_rule(rule_id).await?;
            if json {
                crate::output::print_ok();
            } else {
                println!("Rule '{}' enabled", rule_id);
            }
        },
        RuleCommands::Disable { rule_id } => {
            client.disable_rule(rule_id).await?;
            if json {
                crate::output::print_ok();
            } else {
                println!("Rule '{}' disabled", rule_id);
            }
        },
        RuleCommands::Execute { rule_id, force } => {
            let result = client.execute_rule(rule_id, force).await?;
            if json {
                crate::output::print_success(&result);
            } else {
                println!(
                    "Execution result for rule '{}': {}",
                    rule_id,
                    serde_json::to_string_pretty(&result)?
                );
            }
        },
        RuleCommands::Create { name, description } => {
            let result = client.create_rule(&name, description.as_deref()).await?;
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
            let result = client.update_rule(rule_id, Value::Object(body)).await?;
            if json {
                crate::output::print_success(&result);
            } else {
                println!("Rule {} updated", rule_id);
            }
        },
        RuleCommands::Delete { rule_id, force } => {
            if !force && !json {
                println!("Delete rule {}? [y/N]", rule_id);
                let mut input = String::new();
                std::io::stdin().read_line(&mut input)?;
                if !input.trim().eq_ignore_ascii_case("y") {
                    println!("Cancelled");
                    return Ok(());
                }
            }
            client.delete_rule(rule_id).await?;
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
}

impl RuleClient {
    pub(crate) fn new(base_url: &str) -> Result<Self> {
        Ok(Self {
            client: Client::new(),
            base_url: base_url.to_string(),
        })
    }

    pub(crate) async fn list_rules(&self) -> Result<Value> {
        let response = self
            .client
            .get(format!("{}/api/rules", self.base_url))
            .send()
            .await?;

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
        let response = self
            .client
            .get(format!("{}/api/rules/{}", self.base_url, rule_id))
            .send()
            .await?;

        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            Err(anyhow::anyhow!("Failed to get rule: {}", response.status()))
        }
    }

    pub(crate) async fn enable_rule(&self, rule_id: i64) -> Result<Value> {
        let response = self
            .client
            .post(format!("{}/api/rules/{}/enable", self.base_url, rule_id))
            .send()
            .await?;

        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            Err(anyhow::anyhow!(
                "Failed to enable rule: {}",
                response.status()
            ))
        }
    }

    pub(crate) async fn disable_rule(&self, rule_id: i64) -> Result<Value> {
        let response = self
            .client
            .post(format!("{}/api/rules/{}/disable", self.base_url, rule_id))
            .send()
            .await?;

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
    pub(crate) async fn execute_rule(&self, rule_id: i64, force: bool) -> Result<Value> {
        let response = self
            .client
            .post(format!("{}/api/rules/{}/execute", self.base_url, rule_id))
            .json(&serde_json::json!({ "force": force }))
            .send()
            .await?;

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
    pub(crate) async fn create_rule(&self, name: &str, description: Option<&str>) -> Result<Value> {
        let mut body = serde_json::Map::new();
        body.insert("name".into(), Value::String(name.to_string()));
        if let Some(d) = description {
            body.insert("description".into(), Value::String(d.to_string()));
        }
        let response = self
            .client
            .post(format!("{}/api/rules", self.base_url))
            .json(&Value::Object(body))
            .send()
            .await?;

        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            Err(anyhow::anyhow!(
                "Failed to create rule: {}",
                response.status()
            ))
        }
    }

    pub(crate) async fn update_rule(&self, rule_id: i64, body: Value) -> Result<Value> {
        let response = self
            .client
            .put(format!("{}/api/rules/{}", self.base_url, rule_id))
            .json(&body)
            .send()
            .await?;

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

    pub(crate) async fn delete_rule(&self, rule_id: i64) -> Result<Value> {
        let response = self
            .client
            .delete(format!("{}/api/rules/{}", self.base_url, rule_id))
            .send()
            .await?;

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
}
