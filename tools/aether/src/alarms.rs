//! Alarm management module
//!
//! Provides access to alarm: active alerts, alarm rules (list/get/create/
//! update/delete/enable/disable), historical alert events, statistics, and
//! monitor status.

use anyhow::Result;
use clap::Subcommand;
use reqwest::Client;
use serde_json::Value;

use crate::output::{parse_error_body, print_action};

#[derive(Subcommand)]
pub enum AlarmCommands {
    /// List active alerts
    #[command(about = "List currently active alerts")]
    List {
        /// Filter by channel ID
        #[arg(long)]
        channel: Option<i64>,
        /// Filter by warning level (1=low, 2=medium, 3=high)
        #[arg(long)]
        level: Option<i64>,
        /// Keyword search (rule name, channel, point)
        #[arg(long)]
        keyword: Option<String>,
        /// Page number (1-based)
        #[arg(long, default_value = "1")]
        page: i64,
        /// Page size
        #[arg(long, default_value = "50")]
        size: i64,
    },

    /// Get a single active alert by ID
    #[command(about = "Get details of a specific active alert")]
    Get {
        /// Alert ID
        id: i64,
    },

    /// List alarm rules
    #[command(about = "List alarm rules")]
    Rules {
        /// Filter by channel ID
        #[arg(long)]
        channel: Option<i64>,
        /// Show only enabled rules
        #[arg(long)]
        enabled: bool,
        /// Filter by warning level (1=low, 2=medium, 3=high)
        #[arg(long)]
        level: Option<i64>,
        /// Keyword search
        #[arg(long)]
        keyword: Option<String>,
        /// Page number (1-based)
        #[arg(long, default_value = "1")]
        page: i64,
        /// Page size
        #[arg(long, default_value = "50")]
        size: i64,
    },

    /// Get a single alarm rule by ID
    #[command(about = "Get details of a specific alarm rule")]
    RuleGet {
        /// Rule ID
        id: i64,
    },

    /// List historical alert events (trigger + recovery)
    #[command(about = "List historical alert events")]
    Events {
        /// Filter by rule ID
        #[arg(long)]
        rule: Option<i64>,
        /// Filter by event type: trigger or recovery
        #[arg(long)]
        event_type: Option<String>,
        /// Filter by warning level (1=low, 2=medium, 3=high)
        #[arg(long)]
        level: Option<i64>,
        /// Keyword search
        #[arg(long)]
        keyword: Option<String>,
        /// Page number (1-based)
        #[arg(long, default_value = "1")]
        page: i64,
        /// Page size
        #[arg(long, default_value = "50")]
        size: i64,
    },

    /// Show alert statistics
    #[command(about = "Show alert count and rule statistics")]
    Stats,

    /// Show alarm monitor status
    #[command(about = "Show alarm monitor loop status")]
    Monitor,

    /// Create an alarm rule from a JSON file
    #[command(about = "Create an alarm rule from a JSON file")]
    RuleCreate {
        /// Path to a JSON file matching alarm's CreateRuleRequest
        #[arg(long)]
        file: String,
    },

    /// Update an alarm rule from a JSON file (partial update)
    #[command(about = "Update an alarm rule from a JSON file (only present fields change)")]
    RuleUpdate {
        /// Rule ID
        id: i64,
        /// Path to a JSON file matching alarm's UpdateRuleRequest
        #[arg(long)]
        file: String,
    },

    /// Delete an alarm rule
    #[command(about = "Delete an alarm rule")]
    RuleDelete {
        /// Rule ID
        id: i64,
    },

    /// Enable an alarm rule
    #[command(about = "Enable an alarm rule")]
    RuleEnable {
        /// Rule ID
        id: i64,
    },

    /// Disable an alarm rule
    #[command(about = "Disable an alarm rule")]
    RuleDisable {
        /// Rule ID
        id: i64,
    },
}

pub async fn handle_command(cmd: AlarmCommands, base_url: &str, json: bool) -> Result<()> {
    let client = AlarmClient::new(base_url)?;

    match cmd {
        AlarmCommands::List {
            channel,
            level,
            keyword,
            page,
            size,
        } => {
            let data = client
                .list_alerts(channel, level, keyword.as_deref(), page, size)
                .await?;
            if json {
                crate::output::print_success(&data);
            } else {
                print_alerts_table(&data);
            }
        },

        AlarmCommands::Get { id } => {
            let data = client.get_alert(id).await?;
            crate::output::print_value(&data, json);
        },

        AlarmCommands::Rules {
            channel,
            enabled,
            level,
            keyword,
            page,
            size,
        } => {
            let enabled_filter = if enabled { Some(true) } else { None };
            let data = client
                .list_rules(
                    channel,
                    enabled_filter,
                    level,
                    keyword.as_deref(),
                    page,
                    size,
                )
                .await?;
            if json {
                crate::output::print_success(&data);
            } else {
                print_rules_table(&data);
            }
        },

        AlarmCommands::RuleGet { id } => {
            let data = client.get_rule(id).await?;
            crate::output::print_value(&data, json);
        },

        AlarmCommands::Events {
            rule,
            event_type,
            level,
            keyword,
            page,
            size,
        } => {
            let data = client
                .list_events(
                    rule,
                    event_type.as_deref(),
                    level,
                    keyword.as_deref(),
                    page,
                    size,
                )
                .await?;
            if json {
                crate::output::print_success(&data);
            } else {
                print_events_table(&data);
            }
        },

        AlarmCommands::Stats => {
            let data = client.get_statistics().await?;
            crate::output::print_value(&data, json);
        },

        AlarmCommands::Monitor => {
            let data = client.get_monitor_status().await?;
            crate::output::print_value(&data, json);
        },

        AlarmCommands::RuleCreate { file } => {
            let raw = std::fs::read_to_string(&file)
                .map_err(|e| anyhow::anyhow!("Failed to read rule file {file}: {e}"))?;
            let body: Value = serde_json::from_str(&raw)
                .map_err(|e| anyhow::anyhow!("Invalid JSON in rule file {file}: {e}"))?;
            let data = client.create_rule(&body).await?;
            crate::output::print_value(&data, json);
        },

        // Unlike uplink (whose cert-delete returns genuinely different messages
        // for the same 200), alarm's action endpoints return one static
        // success message per endpoint via `ApiResponse::ok` ("Rule updated",
        // "Rule deleted", …) — mandatory (never omitted) but invariant. So
        // `action_message` reliably echoes that static string and the fallback
        // is only ever a defensive backstop. The fallbacks below deliberately
        // omit the id: the server's message carries none, so an id-bearing
        // fallback ("Rule 7 updated") promises a line the user never actually
        // sees — and the operator already typed the id on the command line.
        AlarmCommands::RuleUpdate { id, file } => {
            let raw = std::fs::read_to_string(&file)
                .map_err(|e| anyhow::anyhow!("Failed to read rule file {file}: {e}"))?;
            let body: Value = serde_json::from_str(&raw)
                .map_err(|e| anyhow::anyhow!("Invalid JSON in rule file {file}: {e}"))?;
            let data = client.update_rule(id, &body).await?;
            print_action(&data, "Rule updated", json);
        },

        AlarmCommands::RuleDelete { id } => {
            let data = client.delete_rule(id).await?;
            print_action(&data, "Rule deleted", json);
        },

        AlarmCommands::RuleEnable { id } => {
            let data = client.set_rule_enabled(id, true).await?;
            print_action(&data, "Rule enabled", json);
        },

        AlarmCommands::RuleDisable { id } => {
            let data = client.set_rule_enabled(id, false).await?;
            print_action(&data, "Rule disabled", json);
        },
    }

    Ok(())
}

// ── Human-readable table printers ────────────────────────────────────────────

fn print_alerts_table(data: &Value) {
    let list = data
        .get("data")
        .and_then(|d| d.get("list"))
        .and_then(|l| l.as_array());

    let total = data
        .get("data")
        .and_then(|d| d.get("total"))
        .and_then(|t| t.as_i64())
        .unwrap_or(0);

    match list {
        None => {
            println!("No active alerts.");
        },
        Some(items) if items.is_empty() => {
            println!("No active alerts.");
        },
        Some(items) => {
            println!(
                "{:<6} {:<20} {:<8} {:<10} {:<8} {:<8} Triggered",
                "ID", "Rule", "Level", "Channel", "Type", "Point"
            );
            println!("{}", "-".repeat(80));
            for item in items {
                let id = item.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
                let rule = item
                    .get("rule_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-");
                let level = item
                    .get("warning_level")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                let level_label = match level {
                    1 => "low",
                    2 => "medium",
                    3 => "high",
                    _ => "?",
                };
                let channel = item.get("channel_id").and_then(|v| v.as_i64()).unwrap_or(0);
                let dtype = item
                    .get("data_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-");
                let point = item.get("point_id").and_then(|v| v.as_i64()).unwrap_or(0);
                let triggered = item
                    .get("triggered_at")
                    .and_then(|v| v.as_i64())
                    .map(|ts| {
                        chrono::DateTime::from_timestamp(ts, 0)
                            .map(|dt| dt.format("%m-%d %H:%M:%S").to_string())
                            .unwrap_or_else(|| ts.to_string())
                    })
                    .unwrap_or_else(|| "-".to_string());

                println!(
                    "{:<6} {:<20} {:<8} {:<10} {:<8} {:<8} {}",
                    id,
                    truncate(rule, 19),
                    level_label,
                    channel,
                    dtype,
                    point,
                    triggered
                );
            }
            println!("\nTotal: {}", total);
        },
    }
}

fn print_rules_table(data: &Value) {
    let list = data
        .get("data")
        .and_then(|d| d.get("list"))
        .and_then(|l| l.as_array());

    let total = data
        .get("data")
        .and_then(|d| d.get("total"))
        .and_then(|t| t.as_i64())
        .unwrap_or(0);

    match list {
        None => {
            println!("No alarm rules found.");
        },
        Some(items) if items.is_empty() => {
            println!("No alarm rules found.");
        },
        Some(items) => {
            println!(
                "{:<6} {:<22} {:<8} {:<10} {:<8} {:<5} {:<5} Enabled",
                "ID", "Name", "Level", "Channel", "Type", "Op", "Thresh"
            );
            println!("{}", "-".repeat(80));
            for item in items {
                let id = item.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
                let name = item
                    .get("rule_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-");
                let level = item
                    .get("warning_level")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                let channel = item.get("channel_id").and_then(|v| v.as_i64()).unwrap_or(0);
                let dtype = item
                    .get("data_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-");
                let op = item.get("operator").and_then(|v| v.as_str()).unwrap_or("-");
                let value = item
                    .get("value")
                    .and_then(|v| v.as_f64())
                    .map(|f| format!("{:.2}", f))
                    .unwrap_or_else(|| "-".to_string());
                let enabled = item
                    .get("enabled")
                    .and_then(|v| v.as_bool())
                    .map(|b| if b { "yes" } else { "no" })
                    .unwrap_or("-");

                println!(
                    "{:<6} {:<22} {:<8} {:<10} {:<8} {:<5} {:<5} {}",
                    id,
                    truncate(name, 21),
                    level,
                    channel,
                    dtype,
                    op,
                    value,
                    enabled
                );
            }
            println!("\nTotal: {}", total);
        },
    }
}

fn print_events_table(data: &Value) {
    let list = data
        .get("data")
        .and_then(|d| d.get("list"))
        .and_then(|l| l.as_array());

    let total = data
        .get("data")
        .and_then(|d| d.get("total"))
        .and_then(|t| t.as_i64())
        .unwrap_or(0);

    match list {
        None => {
            println!("No alert events found.");
        },
        Some(items) if items.is_empty() => {
            println!("No alert events found.");
        },
        Some(items) => {
            println!(
                "{:<6} {:<20} {:<10} {:<8} {:<10} {:<12}",
                "ID", "Rule", "EventType", "Level", "Duration", "TriggeredAt"
            );
            println!("{}", "-".repeat(80));
            for item in items {
                let id = item.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
                let rule = item
                    .get("rule_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-");
                let etype = item
                    .get("event_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-");
                let level = item
                    .get("warning_level")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                let duration = item
                    .get("duration")
                    .and_then(|v| v.as_i64())
                    .map(|s| format!("{}s", s))
                    .unwrap_or_else(|| "-".to_string());
                let triggered = item
                    .get("triggered_at")
                    .and_then(|v| v.as_i64())
                    .map(|ts| {
                        chrono::DateTime::from_timestamp(ts, 0)
                            .map(|dt| dt.format("%m-%d %H:%M").to_string())
                            .unwrap_or_else(|| ts.to_string())
                    })
                    .unwrap_or_else(|| "-".to_string());

                println!(
                    "{:<6} {:<20} {:<10} {:<8} {:<10} {}",
                    id,
                    truncate(rule, 19),
                    etype,
                    level,
                    duration,
                    triggered
                );
            }
            println!("\nTotal: {}", total);
        },
    }
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max { s } else { &s[..max] }
}

// ── HTTP client ───────────────────────────────────────────────────────────────

pub(crate) struct AlarmClient {
    client: Client,
    base_url: String,
}

impl AlarmClient {
    pub(crate) fn new(base_url: &str) -> Result<Self> {
        Ok(Self {
            client: Client::new(),
            base_url: base_url.to_string(),
        })
    }

    pub(crate) async fn list_alerts(
        &self,
        channel: Option<i64>,
        level: Option<i64>,
        keyword: Option<&str>,
        page: i64,
        size: i64,
    ) -> Result<Value> {
        let mut params = vec![
            ("page".to_string(), page.to_string()),
            ("page_size".to_string(), size.to_string()),
        ];
        if let Some(ch) = channel {
            params.push(("channel_id".to_string(), ch.to_string()));
        }
        if let Some(lv) = level {
            params.push(("warning_level".to_string(), lv.to_string()));
        }
        if let Some(kw) = keyword {
            params.push(("keyword".to_string(), kw.to_string()));
        }

        let resp = self
            .client
            .get(format!("{}/alarmApi/alerts", self.base_url))
            .query(&params)
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(anyhow::anyhow!(
                "Failed to list alerts: {} — ensure alarm is running",
                resp.status()
            ))
        }
    }

    pub(crate) async fn get_alert(&self, id: i64) -> Result<Value> {
        let resp = self
            .client
            .get(format!("{}/alarmApi/alerts/{}", self.base_url, id))
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(anyhow::anyhow!(
                "Alert {} not found (HTTP {})",
                id,
                resp.status()
            ))
        }
    }

    pub(crate) async fn list_rules(
        &self,
        channel: Option<i64>,
        enabled: Option<bool>,
        level: Option<i64>,
        keyword: Option<&str>,
        page: i64,
        size: i64,
    ) -> Result<Value> {
        let mut params = vec![
            ("page".to_string(), page.to_string()),
            ("page_size".to_string(), size.to_string()),
        ];
        if let Some(ch) = channel {
            params.push(("channel_id".to_string(), ch.to_string()));
        }
        if let Some(en) = enabled {
            params.push(("enabled".to_string(), en.to_string()));
        }
        if let Some(lv) = level {
            params.push(("warning_level".to_string(), lv.to_string()));
        }
        if let Some(kw) = keyword {
            params.push(("keyword".to_string(), kw.to_string()));
        }

        let resp = self
            .client
            .get(format!("{}/alarmApi/rules", self.base_url))
            .query(&params)
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(anyhow::anyhow!(
                "Failed to list alarm rules: {} — ensure alarm is running",
                resp.status()
            ))
        }
    }

    pub(crate) async fn get_rule(&self, id: i64) -> Result<Value> {
        let resp = self
            .client
            .get(format!("{}/alarmApi/rules/{}", self.base_url, id))
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(anyhow::anyhow!(
                "Rule {} not found (HTTP {})",
                id,
                resp.status()
            ))
        }
    }

    pub(crate) async fn list_events(
        &self,
        rule: Option<i64>,
        event_type: Option<&str>,
        level: Option<i64>,
        keyword: Option<&str>,
        page: i64,
        size: i64,
    ) -> Result<Value> {
        let mut params = vec![
            ("page".to_string(), page.to_string()),
            ("page_size".to_string(), size.to_string()),
        ];
        if let Some(r) = rule {
            params.push(("rule_id".to_string(), r.to_string()));
        }
        if let Some(et) = event_type {
            params.push(("event_type".to_string(), et.to_string()));
        }
        if let Some(lv) = level {
            params.push(("warning_level".to_string(), lv.to_string()));
        }
        if let Some(kw) = keyword {
            params.push(("keyword".to_string(), kw.to_string()));
        }

        let resp = self
            .client
            .get(format!("{}/alarmApi/alert-events", self.base_url))
            .query(&params)
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(anyhow::anyhow!(
                "Failed to list alert events: {} — ensure alarm is running",
                resp.status()
            ))
        }
    }

    pub(crate) async fn get_statistics(&self) -> Result<Value> {
        let resp = self
            .client
            .get(format!("{}/alarmApi/alert-statistics", self.base_url))
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(anyhow::anyhow!(
                "Failed to get alarm statistics: {}",
                resp.status()
            ))
        }
    }

    async fn get_monitor_status(&self) -> Result<Value> {
        let resp = self
            .client
            .get(format!("{}/alarmApi/monitor/status", self.base_url))
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(anyhow::anyhow!(
                "Failed to get monitor status: {}",
                resp.status()
            ))
        }
    }

    pub(crate) async fn create_rule(&self, body: &Value) -> Result<Value> {
        let resp = self
            .client
            .post(format!("{}/alarmApi/rules", self.base_url))
            .json(body)
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(parse_error_body("Failed to create alarm rule", resp).await)
        }
    }

    pub(crate) async fn update_rule(&self, id: i64, body: &Value) -> Result<Value> {
        let resp = self
            .client
            .put(format!("{}/alarmApi/rules/{}", self.base_url, id))
            .json(body)
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(parse_error_body("Failed to update alarm rule", resp).await)
        }
    }

    pub(crate) async fn delete_rule(&self, id: i64) -> Result<Value> {
        let resp = self
            .client
            .delete(format!("{}/alarmApi/rules/{}", self.base_url, id))
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(parse_error_body("Failed to delete alarm rule", resp).await)
        }
    }

    /// alarm uses PATCH here; automation uses POST for the same semantics on its
    /// own business rules. Do not unify — the services genuinely differ.
    pub(crate) async fn set_rule_enabled(&self, id: i64, enabled: bool) -> Result<Value> {
        let action = if enabled { "enable" } else { "disable" };
        let resp = self
            .client
            .patch(format!(
                "{}/alarmApi/rules/{}/{}",
                self.base_url, id, action
            ))
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(parse_error_body(&format!("Failed to {action} alarm rule"), resp).await)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AlarmClient;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn create_rule_posts_full_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/alarmApi/rules"))
            .and(body_json(serde_json::json!({
                "service_type": "io",
                "channel_id": 1001,
                "data_type": "T",
                "point_id": 5,
                "rule_name": "over-temp",
                "warning_level": 3,
                "operator": ">",
                "value": 85.0,
                "enabled": true,
                "description": "cell temperature"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "id": 7 })))
            .expect(1)
            .mount(&server)
            .await;

        let client = AlarmClient::new(&server.uri()).unwrap();
        let body = serde_json::json!({
            "service_type": "io",
            "channel_id": 1001,
            "data_type": "T",
            "point_id": 5,
            "rule_name": "over-temp",
            "warning_level": 3,
            "operator": ">",
            "value": 85.0,
            "enabled": true,
            "description": "cell temperature"
        });
        let v = client.create_rule(&body).await.unwrap();

        assert_eq!(v["id"], 7);
    }

    #[tokio::test]
    async fn update_rule_uses_put_and_forwards_the_body() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/alarmApi/rules/7"))
            // Assert the body: without this, an implementation that never
            // forwards the request body would still pass.
            .and(body_json(serde_json::json!({ "value": 90.0 })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let client = AlarmClient::new(&server.uri()).unwrap();
        client
            .update_rule(7, &serde_json::json!({ "value": 90.0 }))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn delete_rule_uses_delete() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/alarmApi/rules/7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let client = AlarmClient::new(&server.uri()).unwrap();
        client.delete_rule(7).await.unwrap();
    }

    #[tokio::test]
    async fn enable_uses_patch_on_the_enable_path() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/alarmApi/rules/7/enable"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        // Only /enable is mounted. If set_rule_enabled(_, true) ever targets
        // /disable, wiremock 404s and this fails — which is the point.
        let client = AlarmClient::new(&server.uri()).unwrap();
        client.set_rule_enabled(7, true).await.unwrap();
    }

    #[tokio::test]
    async fn disable_uses_patch_on_the_disable_path() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/alarmApi/rules/7/disable"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let client = AlarmClient::new(&server.uri()).unwrap();
        client.set_rule_enabled(7, false).await.unwrap();
    }

    #[tokio::test]
    async fn delete_rule_surfaces_inline_error_message() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/alarmApi/rules/999"))
            .respond_with(ResponseTemplate::new(404).set_body_json(
                serde_json::json!({ "success": false, "message": "Rule 999 not found", "data": null }),
            ))
            .mount(&server)
            .await;

        let client = AlarmClient::new(&server.uri()).unwrap();
        let err = client.delete_rule(999).await.unwrap_err().to_string();

        assert!(err.contains("Rule 999 not found"), "{err}");
    }

    #[tokio::test]
    async fn create_rule_surfaces_inline_error_message() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/alarmApi/rules"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "success": false,
                "message": "Invalid operator. Allowed: >, <, >=, <=, ==, !=",
                "data": null,
            })))
            .mount(&server)
            .await;

        let client = AlarmClient::new(&server.uri()).unwrap();
        let err = client
            .create_rule(&serde_json::json!({ "operator": "?" }))
            .await
            .unwrap_err()
            .to_string();

        assert!(err.contains("Invalid operator"), "{err}");
    }

    #[tokio::test]
    async fn update_rule_surfaces_inline_error_message() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/alarmApi/rules/7"))
            .respond_with(ResponseTemplate::new(404).set_body_json(
                serde_json::json!({ "success": false, "message": "Rule 7 not found", "data": null }),
            ))
            .mount(&server)
            .await;

        let client = AlarmClient::new(&server.uri()).unwrap();
        let err = client
            .update_rule(7, &serde_json::json!({ "value": 90.0 }))
            .await
            .unwrap_err()
            .to_string();

        assert!(err.contains("Rule 7 not found"), "{err}");
    }

    #[tokio::test]
    async fn enable_error_names_the_enable_action() {
        // Only /enable is mounted, and its context string must say "enable" so a
        // script looping over both operations can tell which one failed (Fix 3).
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/alarmApi/rules/7/enable"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let client = AlarmClient::new(&server.uri()).unwrap();
        let err = client
            .set_rule_enabled(7, true)
            .await
            .unwrap_err()
            .to_string();

        assert!(err.contains("Failed to enable alarm rule"), "{err}");
    }

    #[tokio::test]
    async fn disable_error_names_the_disable_action() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/alarmApi/rules/7/disable"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let client = AlarmClient::new(&server.uri()).unwrap();
        let err = client
            .set_rule_enabled(7, false)
            .await
            .unwrap_err()
            .to_string();

        assert!(err.contains("Failed to disable alarm rule"), "{err}");
    }
}
