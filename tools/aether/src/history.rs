//! History data module
//!
//! Provides read-only access to history: latest values, range queries,
//! channel metadata, and storage metrics.

use anyhow::Result;
use clap::Subcommand;
use reqwest::Client;
use serde_json::Value;

#[derive(Subcommand)]
pub enum HistoryCommands {
    /// Get the latest stored value for a point
    #[command(about = "Get the latest historical value for a point")]
    Latest {
        /// Logical series key (e.g. inst:9:M or io:1001:T)
        series_key: String,
        /// Point ID (field name inside the hash)
        point_id: String,
    },

    /// Query historical data for a point within a time range
    #[command(about = "Query historical data for a point")]
    Query {
        /// Logical series key (e.g. inst:9:M)
        series_key: String,
        /// Point ID
        point_id: String,
        /// Start time (ISO 8601, e.g. 2026-05-12T00:00:00Z or relative like -1h)
        #[arg(long)]
        from: Option<String>,
        /// End time (ISO 8601, defaults to now)
        #[arg(long)]
        to: Option<String>,
        /// Page number (1-based)
        #[arg(long, default_value = "1")]
        page: i64,
        /// Page size (max rows per page)
        #[arg(long, default_value = "100")]
        size: i64,
    },

    /// List channels that have historical data
    #[command(about = "List channels known to history")]
    Channels,

    /// Show history storage metrics
    #[command(about = "Show historical storage metrics (row counts, data range, etc.)")]
    Metrics,

    /// Check history health
    #[command(about = "Check history service health")]
    Health,

    /// Batch query latest values or range data for multiple points at once
    #[command(
        about = "Batch query historical data for multiple (series_key, point_id) pairs",
        long_about = "Batch query historical data for multiple points in one request (max 20).\n\
            Each --series value must be in the format  series_key,point_id\n\
            Examples:\n  \
            aether history batch --series inst:9:M,101 --series inst:9:M,102\n  \
            aether history batch --series inst:9:M,101 --from 2026-05-01T00:00:00Z --limit 500"
    )]
    Batch {
        /// Series to query, format: series_key,point_id  (repeatable, max 20)
        #[arg(long = "series", value_name = "KEY,POINT_ID")]
        series: Vec<String>,

        /// Start time (ISO 8601, e.g. 2026-05-01T00:00:00Z)
        #[arg(long)]
        from: String,

        /// End time (ISO 8601, defaults to now)
        #[arg(long)]
        to: Option<String>,

        /// Max data points returned per series (default 1000, max 5000)
        #[arg(long, default_value = "1000")]
        limit: i64,
    },
}

pub async fn handle_command(cmd: HistoryCommands, base_url: &str, json: bool) -> Result<()> {
    let client = HistoryClient::new(base_url)?;

    match cmd {
        HistoryCommands::Latest {
            series_key,
            point_id,
        } => {
            let data = client.get_latest(&series_key, &point_id).await?;
            if json {
                crate::output::print_success(&data);
            } else {
                print_latest(&data, &series_key, &point_id);
            }
        },

        HistoryCommands::Query {
            series_key,
            point_id,
            from,
            to,
            page,
            size,
        } => {
            let data = client
                .query_range(
                    &series_key,
                    &point_id,
                    from.as_deref(),
                    to.as_deref(),
                    page,
                    size,
                )
                .await?;
            if json {
                crate::output::print_success(&data);
            } else {
                print_query_result(&data);
            }
        },

        HistoryCommands::Channels => {
            let data = client.list_channels().await?;
            if json {
                crate::output::print_success(&data);
            } else {
                print_channels(&data);
            }
        },

        HistoryCommands::Metrics => {
            let data = client.get_metrics().await?;
            if json {
                crate::output::print_success(&data);
            } else {
                println!("{}", serde_json::to_string_pretty(&data)?);
            }
        },

        HistoryCommands::Health => {
            let data = client.health().await?;
            if json {
                crate::output::print_success(&data);
            } else {
                println!("{}", serde_json::to_string_pretty(&data)?);
            }
        },

        HistoryCommands::Batch {
            series,
            from,
            to,
            limit,
        } => {
            // Parse "series_key,point_id" pairs
            let mut parsed = Vec::new();
            for s in &series {
                let parts: Vec<&str> = s.splitn(2, ',').collect();
                if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
                    anyhow::bail!(
                        "Invalid --series value '{}': expected format series_key,point_id (e.g. inst:9:M,101)",
                        s
                    );
                }
                parsed.push((parts[0].to_string(), parts[1].to_string()));
            }
            if parsed.is_empty() {
                anyhow::bail!("At least one --series value is required");
            }
            if parsed.len() > 20 {
                anyhow::bail!("Maximum 20 --series values allowed (got {})", parsed.len());
            }
            let data = client
                .batch_query(&parsed, &from, to.as_deref(), limit)
                .await?;
            if json {
                crate::output::print_success(&data);
            } else {
                print_batch_result(&data);
            }
        },
    }

    Ok(())
}

// ── Human-readable printers ───────────────────────────────────────────────────

fn print_latest(data: &Value, series_key: &str, point_id: &str) {
    let record = data.get("data");
    match record {
        None => println!("No data for {}:{}", series_key, point_id),
        Some(r) => {
            let value = r.get("value").and_then(|v| v.as_f64());
            let ts = r.get("timestamp").and_then(|v| v.as_str()).unwrap_or("-");
            match value {
                Some(v) => println!("{}:{} = {} @ {}", series_key, point_id, v, ts),
                None => println!("{}:{} = (no numeric value) @ {}", series_key, point_id, ts),
            }
        },
    }
}

fn print_query_result(data: &Value) {
    let records = data.get("data").and_then(|d| d.as_array()).or_else(|| {
        data.get("data")
            .and_then(|d| d.get("data"))
            .and_then(|d| d.as_array())
    });

    let total = data
        .get("total")
        .or_else(|| data.get("data").and_then(|d| d.get("total")))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    match records {
        None => {
            println!("No records found.");
        },
        Some(items) if items.is_empty() => {
            println!("No records found.");
        },
        Some(items) => {
            println!("{:<28} {:<16} Key:Point", "Timestamp", "Value");
            println!("{}", "-".repeat(65));
            for item in items {
                let ts = item
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-");
                let val = item
                    .get("value")
                    .and_then(|v| v.as_f64())
                    .map(|f| format!("{:.4}", f))
                    .unwrap_or_else(|| "-".to_string());
                let key = item
                    .get("series_key")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-");
                let pid = item.get("point_id").and_then(|v| v.as_str()).unwrap_or("-");
                println!("{:<28} {:<16} {}:{}", ts, val, key, pid);
            }
            println!("\nShowing {} of {} records", items.len(), total);
        },
    }
}

fn print_channels(data: &Value) {
    let channels = data
        .get("data")
        .and_then(|d| d.as_array())
        .or_else(|| data.as_array());

    match channels {
        None => println!("No channels found."),
        Some(items) if items.is_empty() => println!("No channels found."),
        Some(items) => {
            println!("{:<20} Point Count", "Series Key");
            println!("{}", "-".repeat(40));
            for item in items {
                let key = item
                    .get("series_key")
                    .and_then(|v| v.as_str())
                    .unwrap_or_else(|| item.as_str().unwrap_or("-"));
                let count = item
                    .get("point_count")
                    .and_then(|v| v.as_i64())
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "-".to_string());
                println!("{:<20} {}", key, count);
            }
            println!("\nTotal: {} channel(s)", items.len());
        },
    }
}

// ── HTTP client ───────────────────────────────────────────────────────────────

fn print_batch_result(data: &Value) {
    let series = data
        .get("data")
        .and_then(|d| d.get("data"))
        .and_then(|d| d.get("series"))
        .or_else(|| data.get("data").and_then(|d| d.get("series")))
        .and_then(|s| s.as_array());

    match series {
        None => println!("No batch result returned."),
        Some(items) if items.is_empty() => println!("No series returned."),
        Some(items) => {
            for series_item in items {
                let key = series_item
                    .get("series_key")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-");
                let pid = series_item
                    .get("point_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-");
                let count = series_item
                    .get("count")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);

                println!("\n── {}:{} ({} points) ──", key, pid, count);

                if let Some(pts) = series_item.get("data").and_then(|d| d.as_array()) {
                    if pts.is_empty() {
                        println!("  (no data in range)");
                    } else {
                        println!("  {:<28} Value", "Timestamp");
                        println!("  {}", "-".repeat(45));
                        for pt in pts {
                            let ts = pt.get("time").and_then(|v| v.as_str()).unwrap_or("-");
                            let val = pt
                                .get("value")
                                .and_then(|v| v.as_f64())
                                .map(|f| format!("{:.4}", f))
                                .unwrap_or_else(|| "-".to_string());
                            println!("  {:<28} {}", ts, val);
                        }
                    }
                }
            }
            println!();
        },
    }
}

pub(crate) struct HistoryClient {
    client: Client,
    base_url: String,
}

impl HistoryClient {
    pub(crate) fn new(base_url: &str) -> Result<Self> {
        Ok(Self {
            client: Client::new(),
            base_url: base_url.to_string(),
        })
    }

    pub(crate) async fn get_latest(&self, series_key: &str, point_id: &str) -> Result<Value> {
        let resp = self
            .client
            .get(format!("{}/hisApi/data/latest", self.base_url))
            .query(&[("series_key", series_key), ("point_id", point_id)])
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else if resp.status() == reqwest::StatusCode::NOT_FOUND {
            Err(anyhow::anyhow!(
                "No data found for {}:{} — point may not have been recorded yet",
                series_key,
                point_id
            ))
        } else {
            Err(anyhow::anyhow!(
                "Failed to get latest value: {} — ensure history is running",
                resp.status()
            ))
        }
    }

    pub(crate) async fn query_range(
        &self,
        series_key: &str,
        point_id: &str,
        from: Option<&str>,
        to: Option<&str>,
        page: i64,
        size: i64,
    ) -> Result<Value> {
        let mut params = vec![
            ("series_key".to_string(), series_key.to_string()),
            ("point_id".to_string(), point_id.to_string()),
            ("page".to_string(), page.to_string()),
            ("page_size".to_string(), size.to_string()),
        ];
        if let Some(f) = from {
            params.push(("start_time".to_string(), f.to_string()));
        }
        if let Some(t) = to {
            params.push(("end_time".to_string(), t.to_string()));
        }

        let resp = self
            .client
            .get(format!("{}/hisApi/data/query", self.base_url))
            .query(&params)
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(anyhow::anyhow!(
                "History query failed: {} — ensure history is running",
                resp.status()
            ))
        }
    }

    async fn list_channels(&self) -> Result<Value> {
        let resp = self
            .client
            .get(format!("{}/hisApi/channels", self.base_url))
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(anyhow::anyhow!(
                "Failed to list channels: {} — ensure history is running",
                resp.status()
            ))
        }
    }

    async fn get_metrics(&self) -> Result<Value> {
        let resp = self
            .client
            .get(format!("{}/hisApi/metrics", self.base_url))
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(anyhow::anyhow!("Failed to get metrics: {}", resp.status()))
        }
    }

    async fn health(&self) -> Result<Value> {
        let resp = self
            .client
            .get(format!("{}/hisApi/health", self.base_url))
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(anyhow::anyhow!(
                "history health check failed: {}",
                resp.status()
            ))
        }
    }

    async fn batch_query(
        &self,
        series: &[(String, String)],
        from: &str,
        to: Option<&str>,
        limit: i64,
    ) -> Result<Value> {
        let to_val = match to {
            Some(t) => t.to_string(),
            None => chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        };
        let body = serde_json::json!({
            "start_time": from,
            "end_time": to_val,
            "series": series.iter().map(|(k, p)| serde_json::json!({
                "series_key": k,
                "point_id": p,
            })).collect::<Vec<_>>(),
            "limit_per_series": limit,
        });

        let resp = self
            .client
            .post(format!("{}/hisApi/data/batch-query", self.base_url))
            .json(&body)
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            let status = resp.status();
            let msg = resp.text().await.unwrap_or_default();
            Err(anyhow::anyhow!(
                "Batch query failed ({}): {} — ensure history is running",
                status,
                msg
            ))
        }
    }
}
