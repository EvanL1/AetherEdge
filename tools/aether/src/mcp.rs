//! `aether mcp` — expose CLI capabilities as MCP tools.
//!
//! Read-only tools are always registered. Write tools (anything that changes
//! device state, channel/rule configuration, or persisted data) register only
//! when `--allow-write` is passed, via a separately-merged ToolRouter — they
//! are absent from `tools/list`, not merely hint-annotated, when the flag is
//! off. See docs/reference/mcp-tools.md.
//!
//! Every tool calls exactly one client method and passes the result through
//! `to_call_result`, which maps `Ok` onto `CallToolResult::structured` and
//! `Err` onto `CallToolResult::error` (the server failed, or is unreachable)
//! -- never `Err(ErrorData)`, which MCP clients render opaquely and would
//! hide the server's own diagnostic text.

use std::path::Path;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, ListResourcesResult, PaginatedRequestParams,
    ReadResourceRequestParams, ReadResourceResult, Resource, ResourceContents, ServerCapabilities,
    ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, tool, tool_handler, tool_router};
use serde::Deserialize;
use serde_json::Value;

use crate::alarms::AlarmClient;
use crate::channels::{ChannelClient, PointClient};
use crate::history::HistoryClient;
use crate::models::client::ModelClient;
use crate::net::NetClient;
use crate::routing::RoutingClient;
use crate::rules::RuleClient;
use crate::templates::TemplateClient;

/// Every tool body ends with this: `Ok` becomes structured content, `Err`
/// becomes visible error text -- never `Err(ErrorData)`, which MCP clients
/// render opaquely and would hide the server's own diagnostic text.
fn to_call_result(result: anyhow::Result<Value>) -> CallToolResult {
    match result {
        Ok(v) => CallToolResult::structured(v),
        Err(e) => CallToolResult::error(vec![ContentBlock::text(format!("{e:#}"))]),
    }
}

pub(crate) struct AetherMcp {
    channels: ChannelClient,
    points: PointClient,
    alarms: AlarmClient,
    rules: RuleClient,
    routing: RoutingClient,
    history: HistoryClient,
    models: ModelClient,
    templates: TemplateClient,
    net: NetClient,
    tool_router: ToolRouter<AetherMcp>,
}

pub(crate) struct BaseUrls {
    pub io: String,
    pub automation: String,
    pub alarm: String,
    pub uplink: String,
    pub history: String,
}

impl AetherMcp {
    pub(crate) fn new(urls: &BaseUrls, allow_write: bool) -> anyhow::Result<Self> {
        let mut tool_router = Self::read_only_router();
        if allow_write {
            tool_router += Self::write_router();
        }

        Ok(Self {
            channels: ChannelClient::new(&urls.io)?,
            points: PointClient::new(&urls.io)?,
            alarms: AlarmClient::new(&urls.alarm)?,
            rules: RuleClient::new(&urls.automation)?,
            routing: RoutingClient::new(&urls.automation)?,
            history: HistoryClient::new(&urls.history)?,
            models: ModelClient::new(&urls.automation)?,
            templates: TemplateClient::new(&urls.io)?,
            net: NetClient::new(&urls.uplink)?,
            tool_router,
        })
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
struct AlarmsListParams {
    /// Filter by channel ID
    channel: Option<i64>,
    /// Filter by warning level (1=low, 2=medium, 3=high)
    level: Option<i64>,
    /// Keyword search across rule name, channel, point
    keyword: Option<String>,
    /// Page number (1-based)
    #[serde(default = "default_page")]
    page: i64,
    /// Page size
    #[serde(default = "default_size")]
    size: i64,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct AlarmsGetParams {
    /// Alert ID
    id: i64,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct AlarmsRulesListParams {
    /// Filter by channel ID
    channel: Option<i64>,
    /// Filter by enabled/disabled state
    enabled: Option<bool>,
    /// Filter by warning level (1=low, 2=medium, 3=high)
    level: Option<i64>,
    /// Keyword search across rule name, channel, point
    keyword: Option<String>,
    /// Page number (1-based)
    #[serde(default = "default_page")]
    page: i64,
    /// Page size
    #[serde(default = "default_size")]
    size: i64,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct AlarmsRuleGetParams {
    /// Alarm rule ID
    id: i64,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct AlarmsEventsParams {
    /// Filter by alarm rule ID
    rule: Option<i64>,
    /// Filter by event type: "trigger" (alarm raised) or "recovery" (alarm cleared)
    event_type: Option<String>,
    /// Filter by warning level (1=low, 2=medium, 3=high)
    level: Option<i64>,
    /// Keyword search across rule name, channel, point
    keyword: Option<String>,
    /// Page number (1-based)
    #[serde(default = "default_page")]
    page: i64,
    /// Page size
    #[serde(default = "default_size")]
    size: i64,
}

fn default_page() -> i64 {
    1
}
fn default_size() -> i64 {
    50
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ChannelIdParams {
    /// Channel ID
    channel_id: u32,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ChannelsPointsParams {
    /// Channel ID
    channel_id: u32,
    /// Optional point-type filter: T | S | C | A
    point_type: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ChannelsPointsMappingParams {
    /// Channel ID
    channel_id: u32,
    /// Point type: T | S | C | A
    point_type: String,
    /// Point ID
    point_id: u32,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct RuleIdParams {
    /// Rule ID
    rule_id: i64,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct HistoryQueryParams {
    /// Logical key identifying the series, e.g. "io:1001:T"
    series_key: String,
    /// Point ID within that series
    point_id: String,
    /// Start of the time range (RFC3339); omit for no lower bound
    from: Option<String>,
    /// End of the time range (RFC3339); omit for "now"
    to: Option<String>,
    /// Page number (1-based)
    #[serde(default = "default_page")]
    page: i64,
    /// Page size
    #[serde(default = "default_size")]
    size: i64,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct HistoryLatestParams {
    /// Logical key identifying the series, e.g. "io:1001:T"
    series_key: String,
    /// Point ID within that series
    point_id: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ModelsInstancesParams {
    /// Filter by product type, e.g. "ESS", "Battery"
    product: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct TemplatesListParams {
    /// Filter by protocol, e.g. "modbus"
    protocol: Option<String>,
}

#[tool_router(router = read_only_router)]
impl AetherMcp {
    #[tool(description = "Show MQTT connection status (connected/disconnected, broker address)")]
    async fn net_mqtt_status(&self) -> CallToolResult {
        to_call_result(self.net.mqtt_status().await)
    }

    #[tool(description = "Show the current uplink configuration (MQTT broker, TLS settings)")]
    async fn net_mqtt_config_get(&self) -> CallToolResult {
        to_call_result(self.net.mqtt_config().await)
    }

    #[tool(
        description = "Show installed TLS certificate info (which of ca_cert/client_cert/client_key are present)"
    )]
    async fn net_cert_info(&self) -> CallToolResult {
        to_call_result(self.net.cert_info().await)
    }

    #[tool(description = "List active alarms, optionally filtered by channel/level/keyword")]
    async fn alarms_list(&self, Parameters(p): Parameters<AlarmsListParams>) -> CallToolResult {
        to_call_result(
            self.alarms
                .list_alerts(p.channel, p.level, p.keyword.as_deref(), p.page, p.size)
                .await,
        )
    }

    #[tool(description = "Get a specific active alert by ID")]
    async fn alarms_get(&self, Parameters(p): Parameters<AlarmsGetParams>) -> CallToolResult {
        to_call_result(self.alarms.get_alert(p.id).await)
    }

    #[tool(description = "List alarm rules, optionally filtered by channel/enabled/level/keyword")]
    async fn alarms_rules_list(
        &self,
        Parameters(p): Parameters<AlarmsRulesListParams>,
    ) -> CallToolResult {
        to_call_result(
            self.alarms
                .list_rules(
                    p.channel,
                    p.enabled,
                    p.level,
                    p.keyword.as_deref(),
                    p.page,
                    p.size,
                )
                .await,
        )
    }

    #[tool(description = "Get a specific alarm rule by ID")]
    async fn alarms_rule_get(
        &self,
        Parameters(p): Parameters<AlarmsRuleGetParams>,
    ) -> CallToolResult {
        to_call_result(self.alarms.get_rule(p.id).await)
    }

    #[tool(
        description = "List historical alarm events, optionally filtered by rule/type/level/keyword"
    )]
    async fn alarms_events(&self, Parameters(p): Parameters<AlarmsEventsParams>) -> CallToolResult {
        to_call_result(
            self.alarms
                .list_events(
                    p.rule,
                    p.event_type.as_deref(),
                    p.level,
                    p.keyword.as_deref(),
                    p.page,
                    p.size,
                )
                .await,
        )
    }

    #[tool(description = "Get aggregate alarm statistics")]
    async fn alarms_stats(&self) -> CallToolResult {
        to_call_result(self.alarms.get_statistics().await)
    }

    #[tool(description = "List all configured communication channels")]
    async fn channels_list(&self) -> CallToolResult {
        to_call_result(self.channels.list_channels().await)
    }

    #[tool(description = "Get the connection status of a specific channel")]
    async fn channels_status(&self, Parameters(p): Parameters<ChannelIdParams>) -> CallToolResult {
        to_call_result(self.channels.get_channel_status(p.channel_id).await)
    }

    #[tool(description = "Show a channel's point-to-instance mappings")]
    async fn channels_mappings(
        &self,
        Parameters(p): Parameters<ChannelIdParams>,
    ) -> CallToolResult {
        to_call_result(self.channels.mappings(p.channel_id).await)
    }

    #[tool(
        description = "List points on a channel that have no protocol address mapping (points not wired to a device register; instance routing is a separate concern)"
    )]
    async fn channels_unmapped_points(
        &self,
        Parameters(p): Parameters<ChannelIdParams>,
    ) -> CallToolResult {
        to_call_result(self.channels.unmapped_points(p.channel_id).await)
    }

    #[tool(description = "List points on a channel, optionally filtered by type (T/S/C/A)")]
    async fn channels_points(
        &self,
        Parameters(p): Parameters<ChannelsPointsParams>,
    ) -> CallToolResult {
        to_call_result(
            self.points
                .list_points(p.channel_id, p.point_type.as_deref())
                .await,
        )
    }

    #[tool(description = "Show the instance mapping for a single point")]
    async fn channels_points_mapping(
        &self,
        Parameters(p): Parameters<ChannelsPointsMappingParams>,
    ) -> CallToolResult {
        to_call_result(
            self.points
                .point_mapping(p.channel_id, &p.point_type, p.point_id)
                .await,
        )
    }

    #[tool(description = "List all business rules")]
    async fn rules_list(&self) -> CallToolResult {
        to_call_result(self.rules.list_rules().await)
    }

    #[tool(description = "Get a specific business rule by ID")]
    async fn rules_get(&self, Parameters(p): Parameters<RuleIdParams>) -> CallToolResult {
        to_call_result(self.rules.get_rule(p.rule_id).await)
    }

    #[tool(description = "List all M2C/C2M routing entries")]
    async fn routing_list(&self) -> CallToolResult {
        to_call_result(self.routing.list_all().await)
    }

    #[tool(description = "Query historical time-series data for a point over a time range")]
    async fn history_query(&self, Parameters(p): Parameters<HistoryQueryParams>) -> CallToolResult {
        to_call_result(
            self.history
                .query_range(
                    &p.series_key,
                    &p.point_id,
                    p.from.as_deref(),
                    p.to.as_deref(),
                    p.page,
                    p.size,
                )
                .await,
        )
    }

    #[tool(description = "Get the latest historical value for a point")]
    async fn history_latest(
        &self,
        Parameters(p): Parameters<HistoryLatestParams>,
    ) -> CallToolResult {
        to_call_result(self.history.get_latest(&p.series_key, &p.point_id).await)
    }

    #[tool(description = "List available product types")]
    async fn models_products(&self) -> CallToolResult {
        to_call_result(self.models.list_products().await)
    }

    #[tool(description = "List device instances, optionally filtered by product type")]
    async fn models_instances(
        &self,
        Parameters(p): Parameters<ModelsInstancesParams>,
    ) -> CallToolResult {
        to_call_result(self.models.list_instances(p.product.as_deref()).await)
    }

    #[tool(description = "List channel configuration templates, optionally filtered by protocol")]
    async fn templates_list(
        &self,
        Parameters(p): Parameters<TemplatesListParams>,
    ) -> CallToolResult {
        to_call_result(self.templates.list_templates(p.protocol.as_deref()).await)
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ChannelsWriteParams {
    /// Channel ID
    channel_id: u32,
    /// Simulation point type: T | S
    point_type: String,
    /// Point ID (numeric or semantic)
    id: String,
    /// Value to write
    value: f64,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ChannelsCreateParams {
    /// Channel name
    name: String,
    /// Protocol identifier, e.g. "modbus", "iec104"
    protocol: String,
    /// Protocol-specific connection parameters (shape depends on `protocol`)
    parameters: Value,
    /// Optional description
    description: Option<String>,
    /// Explicit channel ID; omit to auto-assign
    id: Option<u32>,
    /// Whether the channel starts enabled
    #[serde(default = "default_true")]
    enabled: bool,
}
fn default_true() -> bool {
    true
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ChannelsUpdateParams {
    /// Channel ID
    channel_id: u32,
    /// Partial update body -- only fields present are changed
    body: Value,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ChannelsPointsBatchParams {
    /// Channel ID
    channel_id: u32,
    /// {"create":[...],"update":[...],"delete":[...]} -- the JSON body
    /// verbatim, not a file path (unlike the CLI's --file flag: the MCP
    /// client has no access to the aether-mcp host's filesystem).
    body: Value,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct RulesCreateParams {
    /// Rule name
    name: String,
    /// Optional description
    description: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct RulesUpdateParams {
    /// Rule ID
    rule_id: i64,
    /// Partial update body -- only fields present are changed
    body: Value,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct RulesExecuteParams {
    /// Rule ID
    rule_id: i64,
    /// Currently ignored server-side (automation's execute handler takes no
    /// request body); the rule's actual conditions always decide which
    /// actions fire. Kept for forward compatibility.
    #[serde(default)]
    force: bool,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct AlarmsRuleCreateParams {
    /// Full CreateRuleRequest body: service_type, channel_id, data_type,
    /// point_id, rule_name, operator, value, and optionally warning_level,
    /// enabled, description
    body: Value,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct AlarmsRuleUpdateParams {
    /// Alarm rule ID
    id: i64,
    /// Partial UpdateRuleRequest body -- only fields present are changed
    body: Value,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ModelsInstancesActionParams {
    /// Instance ID
    instance_id: u32,
    /// Numeric action point ID encoded as a string (for example, "1")
    point_id: String,
    /// Value to write
    value: f64,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ModelsInstancesMeasurementParams {
    /// Instance ID
    instance_id: u32,
    /// Point ID: numeric ("1") or semantic ("power_setpoint")
    point_id: String,
    /// Measurement value to set
    value: f64,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct NetMqttConfigSetParams {
    /// Complete NetConfig object (partial updates are not supported by uplink)
    config: Value,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct NetCertUploadParams {
    /// Certificate role: ca_cert | client_cert | client_key
    cert_type: String,
    /// Path to the certificate file ON THE MACHINE RUNNING `aether mcp`
    /// (.pem/.crt/.key/.cer/.p12/.pfx, max 1 MB) -- not a path on the
    /// MCP client's machine.
    file_path: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct NetCertDeleteParams {
    /// Certificate role: ca_cert | client_cert | client_key
    cert_type: String,
}

#[tool_router(router = write_router)]
impl AetherMcp {
    #[tool(
        description = "Inject a simulated T/S value into the acquisition SHM plane. This does not command a device, but downstream rules and alarms treat it as telemetry.",
        annotations(read_only_hint = false)
    )]
    async fn channels_write(
        &self,
        Parameters(p): Parameters<ChannelsWriteParams>,
    ) -> CallToolResult {
        to_call_result(
            self.channels
                .write_point(p.channel_id, &p.point_type, &p.id, p.value)
                .await,
        )
    }

    #[tool(
        description = "Create a new communication channel",
        annotations(read_only_hint = false)
    )]
    async fn channels_create(
        &self,
        Parameters(p): Parameters<ChannelsCreateParams>,
    ) -> CallToolResult {
        to_call_result(
            self.channels
                .create_channel(
                    &p.name,
                    &p.protocol,
                    p.parameters,
                    p.description.as_deref(),
                    p.id,
                    p.enabled,
                )
                .await,
        )
    }

    #[tool(
        description = "Update an existing channel's configuration",
        annotations(read_only_hint = false)
    )]
    async fn channels_update(
        &self,
        Parameters(p): Parameters<ChannelsUpdateParams>,
    ) -> CallToolResult {
        to_call_result(self.channels.update_channel(p.channel_id, p.body).await)
    }

    #[tool(
        description = "Delete a channel and cascade-remove its points, mappings, and routing",
        annotations(read_only_hint = false)
    )]
    async fn channels_delete(&self, Parameters(p): Parameters<ChannelIdParams>) -> CallToolResult {
        to_call_result(self.channels.delete_channel(p.channel_id).await)
    }

    #[tool(description = "Enable a channel", annotations(read_only_hint = false))]
    async fn channels_enable(&self, Parameters(p): Parameters<ChannelIdParams>) -> CallToolResult {
        to_call_result(self.channels.set_enabled(p.channel_id, true).await)
    }

    #[tool(description = "Disable a channel", annotations(read_only_hint = false))]
    async fn channels_disable(&self, Parameters(p): Parameters<ChannelIdParams>) -> CallToolResult {
        to_call_result(self.channels.set_enabled(p.channel_id, false).await)
    }

    #[tool(
        description = "Batch create/update/delete points on a channel. `body` is {\"create\":[...],\"update\":[...],\"delete\":[...]}.",
        annotations(read_only_hint = false)
    )]
    async fn channels_points_batch(
        &self,
        Parameters(p): Parameters<ChannelsPointsBatchParams>,
    ) -> CallToolResult {
        to_call_result(self.points.points_batch(p.channel_id, &p.body).await)
    }

    #[tool(
        description = "Enable a business rule",
        annotations(read_only_hint = false)
    )]
    async fn rules_enable(&self, Parameters(p): Parameters<RuleIdParams>) -> CallToolResult {
        to_call_result(self.rules.enable_rule(p.rule_id).await)
    }

    #[tool(
        description = "Disable a business rule",
        annotations(read_only_hint = false)
    )]
    async fn rules_disable(&self, Parameters(p): Parameters<RuleIdParams>) -> CallToolResult {
        to_call_result(self.rules.disable_rule(p.rule_id).await)
    }

    #[tool(
        description = "Create a new business rule (name + optional description; add conditions/actions afterward via rules_update)",
        annotations(read_only_hint = false)
    )]
    async fn rules_create(&self, Parameters(p): Parameters<RulesCreateParams>) -> CallToolResult {
        to_call_result(
            self.rules
                .create_rule(&p.name, p.description.as_deref())
                .await,
        )
    }

    #[tool(
        description = "Update a business rule's configuration",
        annotations(read_only_hint = false)
    )]
    async fn rules_update(&self, Parameters(p): Parameters<RulesUpdateParams>) -> CallToolResult {
        to_call_result(self.rules.update_rule(p.rule_id, p.body).await)
    }

    #[tool(
        description = "Delete a business rule",
        annotations(read_only_hint = false)
    )]
    async fn rules_delete(&self, Parameters(p): Parameters<RuleIdParams>) -> CallToolResult {
        to_call_result(self.rules.delete_rule(p.rule_id).await)
    }

    #[tool(
        description = "Execute a rule now: evaluates its conditions and dispatches whichever actions they select to real devices. This is a real execution, not a dry run.",
        annotations(read_only_hint = false)
    )]
    async fn rules_execute(&self, Parameters(p): Parameters<RulesExecuteParams>) -> CallToolResult {
        to_call_result(self.rules.execute_rule(p.rule_id, p.force).await)
    }

    #[tool(
        description = "Create an alarm rule. `body` must match alarm's CreateRuleRequest.",
        annotations(read_only_hint = false)
    )]
    async fn alarms_rule_create(
        &self,
        Parameters(p): Parameters<AlarmsRuleCreateParams>,
    ) -> CallToolResult {
        to_call_result(self.alarms.create_rule(&p.body).await)
    }

    #[tool(
        description = "Update an alarm rule (partial update -- only fields present in `body` change)",
        annotations(read_only_hint = false)
    )]
    async fn alarms_rule_update(
        &self,
        Parameters(p): Parameters<AlarmsRuleUpdateParams>,
    ) -> CallToolResult {
        to_call_result(self.alarms.update_rule(p.id, &p.body).await)
    }

    #[tool(
        description = "Delete an alarm rule",
        annotations(read_only_hint = false)
    )]
    async fn alarms_rule_delete(
        &self,
        Parameters(p): Parameters<AlarmsRuleGetParams>,
    ) -> CallToolResult {
        to_call_result(self.alarms.delete_rule(p.id).await)
    }

    #[tool(
        description = "Enable an alarm rule",
        annotations(read_only_hint = false)
    )]
    async fn alarms_rule_enable(
        &self,
        Parameters(p): Parameters<AlarmsRuleGetParams>,
    ) -> CallToolResult {
        to_call_result(self.alarms.set_rule_enabled(p.id, true).await)
    }

    #[tool(
        description = "Disable an alarm rule",
        annotations(read_only_hint = false)
    )]
    async fn alarms_rule_disable(
        &self,
        Parameters(p): Parameters<AlarmsRuleGetParams>,
    ) -> CallToolResult {
        to_call_result(self.alarms.set_rule_enabled(p.id, false).await)
    }

    #[tool(
        description = "Execute a control action on an instance. This writes to a real device via SHM + io.",
        annotations(read_only_hint = false)
    )]
    async fn models_instances_action(
        &self,
        Parameters(p): Parameters<ModelsInstancesActionParams>,
    ) -> CallToolResult {
        to_call_result(
            self.models
                .execute_action(p.instance_id, &p.point_id, p.value)
                .await,
        )
    }

    #[tool(
        description = "Set a measurement value on an instance. This overwrites the live inst:{id}:M value -- the same field real device telemetry populates -- so rules, alarms, and dashboards will treat it as genuine until the next real update arrives.",
        annotations(read_only_hint = false)
    )]
    async fn models_instances_measurement(
        &self,
        Parameters(p): Parameters<ModelsInstancesMeasurementParams>,
    ) -> CallToolResult {
        to_call_result(
            self.models
                .set_measurement(p.instance_id, &p.point_id, p.value)
                .await,
        )
    }

    #[tool(
        description = "Replace uplink's configuration (full NetConfig object -- partial updates are not supported)",
        annotations(read_only_hint = false)
    )]
    async fn net_mqtt_config_set(
        &self,
        Parameters(p): Parameters<NetMqttConfigSetParams>,
    ) -> CallToolResult {
        to_call_result(self.net.mqtt_config_set(&p.config).await)
    }

    #[tool(
        description = "Reconnect the MQTT client",
        annotations(read_only_hint = false)
    )]
    async fn net_mqtt_reconnect(&self) -> CallToolResult {
        to_call_result(self.net.mqtt_reconnect().await)
    }

    #[tool(
        description = "Disconnect the MQTT client",
        annotations(read_only_hint = false)
    )]
    async fn net_mqtt_disconnect(&self) -> CallToolResult {
        to_call_result(self.net.mqtt_disconnect().await)
    }

    #[tool(
        description = "Upload a TLS certificate file (max 1 MB) from a path on the machine running aether mcp -- NOT a path on the MCP client's machine",
        annotations(read_only_hint = false)
    )]
    async fn net_cert_upload(
        &self,
        Parameters(p): Parameters<NetCertUploadParams>,
    ) -> CallToolResult {
        to_call_result(
            self.net
                .cert_upload(&p.cert_type, Path::new(&p.file_path))
                .await,
        )
    }

    #[tool(
        description = "Delete a TLS certificate by role",
        annotations(read_only_hint = false)
    )]
    async fn net_cert_delete(
        &self,
        Parameters(p): Parameters<NetCertDeleteParams>,
    ) -> CallToolResult {
        to_call_result(self.net.cert_delete(&p.cert_type).await)
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for AetherMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let resources = crate::mcp_docs::DOC_RESOURCES
            .iter()
            .map(|d| {
                let mut r = Resource::new(d.uri, crate::mcp_docs::resource_name(d.uri))
                    .with_mime_type("text/markdown");
                if let Some(title) = crate::mcp_docs::frontmatter_field(d.body, "title") {
                    r = r.with_title(title);
                }
                if let Some(desc) = crate::mcp_docs::frontmatter_field(d.body, "description") {
                    r = r.with_description(desc);
                }
                r
            })
            .collect();
        Ok(ListResourcesResult::with_all_items(resources))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, ErrorData> {
        let doc = crate::mcp_docs::DOC_RESOURCES
            .iter()
            .find(|d| d.uri == request.uri)
            .ok_or_else(|| {
                ErrorData::resource_not_found(format!("unknown resource: {}", request.uri), None)
            })?;
        Ok(ReadResourceResult::new(vec![
            ResourceContents::TextResourceContents {
                uri: doc.uri.to_string(),
                mime_type: Some("text/markdown".to_string()),
                text: doc.body.to_string(),
                meta: None,
            },
        ]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn resources_capability_is_advertised() {
        let server = AetherMcp::new(&test_urls("http://localhost:1"), false).unwrap();
        let info = server.get_info();
        assert!(
            info.capabilities.resources.is_some(),
            "resources capability missing from get_info"
        );
    }

    fn test_urls(base: &str) -> BaseUrls {
        BaseUrls {
            io: base.to_string(),
            automation: base.to_string(),
            alarm: base.to_string(),
            uplink: base.to_string(),
            history: base.to_string(),
        }
    }

    /// Shorthand for the common "construct an --allow-write server against
    /// this mock's base URL" step shared by every write-tool test below.
    fn write_mcp(base: &str) -> AetherMcp {
        let mut server = AetherMcp::new(&test_urls(base), true).unwrap();
        server.models = ModelClient::with_access_token(base, "signed-access-token").unwrap();
        server
    }

    /// The full set of tool names that only exist when --allow-write is
    /// passed. Kept in one place so the gating tests can assert none of
    /// them leak into the read-only router, not just the one or two named
    /// in each individual assertion.
    const WRITE_TOOL_NAMES: &[&str] = &[
        "channels_write",
        "channels_create",
        "channels_update",
        "channels_delete",
        "channels_enable",
        "channels_disable",
        "channels_points_batch",
        "rules_enable",
        "rules_disable",
        "rules_create",
        "rules_update",
        "rules_delete",
        "rules_execute",
        "alarms_rule_create",
        "alarms_rule_update",
        "alarms_rule_delete",
        "alarms_rule_enable",
        "alarms_rule_disable",
        "models_instances_action",
        "models_instances_measurement",
        "net_mqtt_config_set",
        "net_mqtt_reconnect",
        "net_mqtt_disconnect",
        "net_cert_upload",
        "net_cert_delete",
    ];

    #[tokio::test]
    async fn read_only_router_has_no_write_tools() {
        let server = MockServer::start().await;
        let mcp = AetherMcp::new(&test_urls(&server.uri()), false).unwrap();
        let names: Vec<_> = mcp
            .tool_router
            .list_all()
            .iter()
            .map(|t| t.name.to_string())
            .collect();

        assert!(names.contains(&"net_mqtt_status".to_string()), "{names:?}");
        assert!(
            !names.contains(&"net_mqtt_config_set".to_string()),
            "{names:?}"
        );
    }

    #[tokio::test]
    async fn net_mqtt_status_calls_the_right_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/netApi/mqtt/status"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "connected": true })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mcp = AetherMcp::new(&test_urls(&server.uri()), false).unwrap();
        let result = mcp.net_mqtt_status().await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
        let structured = result
            .structured_content
            .expect("expected structured content");
        assert_eq!(structured["connected"], true);
    }

    #[tokio::test]
    async fn net_mqtt_status_surfaces_server_error_as_visible_content() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/netApi/mqtt/status"))
            .respond_with(ResponseTemplate::new(500).set_body_json(
                serde_json::json!({ "success": false, "message": "broker unreachable" }),
            ))
            .mount(&server)
            .await;

        let mcp = AetherMcp::new(&test_urls(&server.uri()), false).unwrap();
        let result = mcp.net_mqtt_status().await;

        assert_eq!(result.is_error, Some(true));
        let text = result
            .content
            .iter()
            .find_map(|c| c.as_text().map(|t| t.text.clone()))
            .expect("expected text content");
        assert!(text.contains("broker unreachable"), "{text}");
    }

    #[tokio::test]
    async fn net_mqtt_config_get_calls_the_config_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/netApi/mqtt/config"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "host": "10.0.0.1" })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mcp = AetherMcp::new(&test_urls(&server.uri()), false).unwrap();
        let result = mcp.net_mqtt_config_get().await;

        let structured = result
            .structured_content
            .expect("expected structured content");
        assert_eq!(structured["host"], "10.0.0.1");
    }

    #[tokio::test]
    async fn net_cert_info_calls_the_certificate_info_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/netApi/certificate/info"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "ca_cert": "present" })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mcp = AetherMcp::new(&test_urls(&server.uri()), false).unwrap();
        let result = mcp.net_cert_info().await;

        let structured = result
            .structured_content
            .expect("expected structured content");
        assert_eq!(structured["ca_cert"], "present");
    }

    // NOTE: `AlarmClient::list_alerts`/`list_rules`/`list_events` build their query
    // string from server-side param names (`channel_id`, `warning_level`,
    // `page_size`, `rule_id`, ...), not the CLI-facing arg names (`channel`,
    // `level`, `size`, `rule`). The matchers below assert the real wire shape.

    #[tokio::test]
    async fn alarms_list_forwards_all_filters() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/alarmApi/alerts"))
            .and(query_param("channel_id", "1001"))
            .and(query_param("warning_level", "3"))
            .and(query_param("keyword", "temp"))
            .and(query_param("page", "1"))
            .and(query_param("page_size", "50"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "alerts": [] })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mcp = AetherMcp::new(&test_urls(&server.uri()), false).unwrap();
        let result = mcp
            .alarms_list(Parameters(AlarmsListParams {
                channel: Some(1001),
                level: Some(3),
                keyword: Some("temp".to_string()),
                page: 1,
                size: 50,
            }))
            .await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    #[tokio::test]
    async fn alarms_get_uses_the_id_in_the_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/alarmApi/alerts/7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "id": 7 })))
            .expect(1)
            .mount(&server)
            .await;

        let mcp = AetherMcp::new(&test_urls(&server.uri()), false).unwrap();
        let result = mcp.alarms_get(Parameters(AlarmsGetParams { id: 7 })).await;

        let structured = result
            .structured_content
            .expect("expected structured content");
        assert_eq!(structured["id"], 7);
    }

    #[tokio::test]
    async fn alarms_rules_list_forwards_the_enabled_filter() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/alarmApi/rules"))
            .and(query_param("enabled", "true"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "rules": [] })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mcp = AetherMcp::new(&test_urls(&server.uri()), false).unwrap();
        let result = mcp
            .alarms_rules_list(Parameters(AlarmsRulesListParams {
                channel: None,
                enabled: Some(true),
                level: None,
                keyword: None,
                page: 1,
                size: 50,
            }))
            .await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    #[tokio::test]
    async fn alarms_rule_get_uses_the_id_in_the_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/alarmApi/rules/12"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "id": 12 })))
            .expect(1)
            .mount(&server)
            .await;

        let mcp = AetherMcp::new(&test_urls(&server.uri()), false).unwrap();
        let result = mcp
            .alarms_rule_get(Parameters(AlarmsRuleGetParams { id: 12 }))
            .await;

        let structured = result
            .structured_content
            .expect("expected structured content");
        assert_eq!(structured["id"], 12);
    }

    #[tokio::test]
    async fn alarms_events_forwards_the_event_type_filter() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/alarmApi/alert-events"))
            .and(query_param("event_type", "recovery"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "events": [] })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mcp = AetherMcp::new(&test_urls(&server.uri()), false).unwrap();
        let result = mcp
            .alarms_events(Parameters(AlarmsEventsParams {
                rule: None,
                event_type: Some("recovery".to_string()),
                level: None,
                keyword: None,
                page: 1,
                size: 50,
            }))
            .await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    #[tokio::test]
    async fn alarms_stats_calls_the_statistics_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/alarmApi/alert-statistics"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "total": 3 })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mcp = AetherMcp::new(&test_urls(&server.uri()), false).unwrap();
        let result = mcp.alarms_stats().await;

        let structured = result
            .structured_content
            .expect("expected structured content");
        assert_eq!(structured["total"], 3);
    }

    // NOTE: `ChannelClient::list_channels` requests bare `/api/channels` (no
    // `/list` suffix). io separately registers `/api/channels/list` for a
    // handler literally named `list_channels` -- a name collision with this
    // client method, but not the same route: the CLI client never calls it.
    #[tokio::test]
    async fn channels_list_calls_the_bare_channels_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/channels"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "channels": [] })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mcp = AetherMcp::new(&test_urls(&server.uri()), false).unwrap();
        let result = mcp.channels_list().await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    #[tokio::test]
    async fn channels_status_uses_the_channel_id_in_the_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/channels/1001/status"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "online": true })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mcp = AetherMcp::new(&test_urls(&server.uri()), false).unwrap();
        let result = mcp
            .channels_status(Parameters(ChannelIdParams { channel_id: 1001 }))
            .await;

        let structured = result
            .structured_content
            .expect("expected structured content");
        assert_eq!(structured["online"], true);
    }

    #[tokio::test]
    async fn channels_mappings_uses_the_channel_id_in_the_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/channels/1001/mappings"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "mappings": [] })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mcp = AetherMcp::new(&test_urls(&server.uri()), false).unwrap();
        let result = mcp
            .channels_mappings(Parameters(ChannelIdParams { channel_id: 1001 }))
            .await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    #[tokio::test]
    async fn channels_unmapped_points_uses_the_channel_id_in_the_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/channels/1001/unmapped-points"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "points": [] })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mcp = AetherMcp::new(&test_urls(&server.uri()), false).unwrap();
        let result = mcp
            .channels_unmapped_points(Parameters(ChannelIdParams { channel_id: 1001 }))
            .await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    #[tokio::test]
    async fn channels_points_forwards_the_type_filter() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/channels/1001/points"))
            .and(query_param("type", "T"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "points": [] })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mcp = AetherMcp::new(&test_urls(&server.uri()), false).unwrap();
        let result = mcp
            .channels_points(Parameters(ChannelsPointsParams {
                channel_id: 1001,
                point_type: Some("T".to_string()),
            }))
            .await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    // Mounted only on "C" -- every other point-type test in this suite uses
    // "T", so this proves the type segment is the actual parameter, not a
    // hardcoded "T" (mirrors channels.rs's own
    // point_mapping_uses_a_different_type_segment test for the same reason).
    #[tokio::test]
    async fn channels_points_mapping_uses_the_type_segment_in_the_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/channels/1001/C/points/5/mapping"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "instance_id": 3 })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mcp = AetherMcp::new(&test_urls(&server.uri()), false).unwrap();
        let result = mcp
            .channels_points_mapping(Parameters(ChannelsPointsMappingParams {
                channel_id: 1001,
                point_type: "C".to_string(),
                point_id: 5,
            }))
            .await;

        let structured = result
            .structured_content
            .expect("expected structured content");
        assert_eq!(structured["instance_id"], 3);
    }

    #[tokio::test]
    async fn rules_list_calls_the_rules_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/rules"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "rules": [] })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mcp = AetherMcp::new(&test_urls(&server.uri()), false).unwrap();
        let result = mcp.rules_list().await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    #[tokio::test]
    async fn rules_get_uses_the_rule_id_in_the_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/rules/9"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "id": 9 })))
            .expect(1)
            .mount(&server)
            .await;

        let mcp = AetherMcp::new(&test_urls(&server.uri()), false).unwrap();
        let result = mcp.rules_get(Parameters(RuleIdParams { rule_id: 9 })).await;

        let structured = result
            .structured_content
            .expect("expected structured content");
        assert_eq!(structured["id"], 9);
    }

    #[tokio::test]
    async fn routing_list_calls_the_routing_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/routing"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "routes": [] })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mcp = AetherMcp::new(&test_urls(&server.uri()), false).unwrap();
        let result = mcp.routing_list().await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    // NOTE: `HistoryClient::query_range` GETs `/hisApi/data/query` (not
    // `/hisApi/query` -- there's an extra `/data` segment shared with
    // `get_latest` below).
    #[tokio::test]
    async fn history_query_forwards_the_time_range() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/hisApi/data/query"))
            .and(query_param("series_key", "io:1001:T"))
            .and(query_param("point_id", "5"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "points": [] })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mcp = AetherMcp::new(&test_urls(&server.uri()), false).unwrap();
        let result = mcp
            .history_query(Parameters(HistoryQueryParams {
                series_key: "io:1001:T".to_string(),
                point_id: "5".to_string(),
                from: None,
                to: None,
                page: 1,
                size: 50,
            }))
            .await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    // NOTE: `HistoryClient::get_latest` GETs `/hisApi/data/latest` (not
    // `/hisApi/latest`).
    #[tokio::test]
    async fn history_latest_uses_the_point_id() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/hisApi/data/latest"))
            .and(query_param("series_key", "io:1001:T"))
            .and(query_param("point_id", "5"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "value": 42.0 })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mcp = AetherMcp::new(&test_urls(&server.uri()), false).unwrap();
        let result = mcp
            .history_latest(Parameters(HistoryLatestParams {
                series_key: "io:1001:T".to_string(),
                point_id: "5".to_string(),
            }))
            .await;

        let structured = result
            .structured_content
            .expect("expected structured content");
        assert_eq!(structured["value"], 42.0);
    }

    #[tokio::test]
    async fn models_products_calls_the_products_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/products"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "products": [] })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mcp = AetherMcp::new(&test_urls(&server.uri()), false).unwrap();
        let result = mcp.models_products().await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    // NOTE: `ModelClient::list_instances` GETs bare `/api/instances` with an
    // optional `?product=` query string -- there's no `/list` suffix (unlike
    // `channels_list`'s neighboring io route, this one really doesn't
    // have it).
    #[tokio::test]
    async fn models_instances_forwards_the_product_filter() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/instances"))
            .and(query_param("product", "ESS"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "instances": [] })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mcp = AetherMcp::new(&test_urls(&server.uri()), false).unwrap();
        let result = mcp
            .models_instances(Parameters(ModelsInstancesParams {
                product: Some("ESS".to_string()),
            }))
            .await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    #[tokio::test]
    async fn templates_list_forwards_the_protocol_filter() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/templates"))
            .and(query_param("protocol", "modbus"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "templates": [] })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mcp = AetherMcp::new(&test_urls(&server.uri()), false).unwrap();
        let result = mcp
            .templates_list(Parameters(TemplatesListParams {
                protocol: Some("modbus".to_string()),
            }))
            .await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    #[tokio::test]
    async fn write_router_is_empty_without_allow_write() {
        let server = MockServer::start().await;
        let mcp = AetherMcp::new(&test_urls(&server.uri()), false).unwrap();
        let names: Vec<_> = mcp
            .tool_router
            .list_all()
            .iter()
            .map(|t| t.name.to_string())
            .collect();

        // Exhaustive: none of the write tools added so far are reachable
        // without --allow-write -- not just the two spot-checked below.
        for write_tool in WRITE_TOOL_NAMES {
            assert!(!names.contains(&write_tool.to_string()), "{names:?}");
        }
        assert!(!names.contains(&"channels_write".to_string()), "{names:?}");
        // Route-count safety net: catches a future write tool landing in the
        // wrong impl block (and so never getting added to WRITE_TOOL_NAMES),
        // or a name collision silently overwriting a read-only route.
        assert_eq!(names.len(), 23, "{names:?}");
    }

    #[tokio::test]
    async fn write_router_is_present_with_allow_write() {
        let server = MockServer::start().await;
        let mcp = AetherMcp::new(&test_urls(&server.uri()), true).unwrap();
        let names: Vec<_> = mcp
            .tool_router
            .list_all()
            .iter()
            .map(|t| t.name.to_string())
            .collect();

        for write_tool in WRITE_TOOL_NAMES {
            assert!(names.contains(&write_tool.to_string()), "{names:?}");
        }
        assert!(names.contains(&"channels_write".to_string()), "{names:?}");
        // Read-only tools are still present too -- --allow-write ADDS, doesn't replace.
        assert!(names.contains(&"channels_list".to_string()), "{names:?}");
        // Route-count safety net: 23 read-only + 25 write, no collisions/overwrites.
        assert_eq!(names.len(), 48, "{names:?}");
    }

    #[tokio::test]
    async fn channels_write_posts_the_flattened_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/channels/1001/write"))
            .and(body_json(
                serde_json::json!({ "type": "T", "id": "5", "value": 50.0 }),
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "success": true })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mcp = write_mcp(&server.uri());
        let result = mcp
            .channels_write(Parameters(ChannelsWriteParams {
                channel_id: 1001,
                point_type: "T".to_string(),
                id: "5".to_string(),
                value: 50.0,
            }))
            .await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    #[tokio::test]
    async fn channels_create_posts_the_new_channel_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/channels"))
            .and(body_json(serde_json::json!({
                "name": "new-channel",
                "protocol": "modbus",
                "parameters": { "host": "10.0.0.5", "port": 502 },
                "enabled": true,
            })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "id": 1002 })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mcp = write_mcp(&server.uri());
        let result = mcp
            .channels_create(Parameters(ChannelsCreateParams {
                name: "new-channel".to_string(),
                protocol: "modbus".to_string(),
                parameters: serde_json::json!({ "host": "10.0.0.5", "port": 502 }),
                description: None,
                id: None,
                enabled: true,
            }))
            .await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    #[tokio::test]
    async fn channels_update_uses_put_on_the_channel_id() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/channels/1001"))
            .and(body_json(serde_json::json!({ "description": "updated" })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "id": 1001 })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mcp = write_mcp(&server.uri());
        let result = mcp
            .channels_update(Parameters(ChannelsUpdateParams {
                channel_id: 1001,
                body: serde_json::json!({ "description": "updated" }),
            }))
            .await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    #[tokio::test]
    async fn channels_delete_uses_delete_on_the_channel_id() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/api/channels/1001"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "success": true })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mcp = write_mcp(&server.uri());
        let result = mcp
            .channels_delete(Parameters(ChannelIdParams { channel_id: 1001 }))
            .await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    #[tokio::test]
    async fn channels_enable_puts_enabled_true_and_channels_disable_puts_enabled_false() {
        // Two separate servers -- one mock per path, so swapping the two
        // tools' request bodies would be caught (mounting both on one server
        // with .expect(1) each cannot detect a swap).
        let enable_server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/channels/1001/enabled"))
            .and(body_json(serde_json::json!({ "enabled": true })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&enable_server)
            .await;
        let mcp = write_mcp(&enable_server.uri());
        let result = mcp
            .channels_enable(Parameters(ChannelIdParams { channel_id: 1001 }))
            .await;
        assert_ne!(result.is_error, Some(true), "{result:?}");

        let disable_server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/channels/1001/enabled"))
            .and(body_json(serde_json::json!({ "enabled": false })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&disable_server)
            .await;
        let mcp = write_mcp(&disable_server.uri());
        let result = mcp
            .channels_disable(Parameters(ChannelIdParams { channel_id: 1001 }))
            .await;
        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    #[tokio::test]
    async fn channels_points_batch_posts_the_body_verbatim() {
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

        let mcp = write_mcp(&server.uri());
        let result = mcp
            .channels_points_batch(Parameters(ChannelsPointsBatchParams {
                channel_id: 1001,
                body: serde_json::json!({ "delete": [{ "point_id": 3 }] }),
            }))
            .await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    #[tokio::test]
    async fn rules_enable_and_disable_hit_their_own_paths() {
        let enable_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/rules/9/enable"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&enable_server)
            .await;
        let mcp = write_mcp(&enable_server.uri());
        let result = mcp
            .rules_enable(Parameters(RuleIdParams { rule_id: 9 }))
            .await;
        assert_ne!(result.is_error, Some(true), "{result:?}");

        let disable_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/rules/9/disable"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&disable_server)
            .await;
        let mcp = write_mcp(&disable_server.uri());
        let result = mcp
            .rules_disable(Parameters(RuleIdParams { rule_id: 9 }))
            .await;
        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    // Body assertion added beyond the plan's draft: rules_create only ever
    // sends "name" here (description is None), so this also proves an
    // Option::None doesn't leak a `"description": null` field into the body.
    #[tokio::test]
    async fn rules_create_posts_the_name_and_description() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/rules"))
            .and(body_json(serde_json::json!({ "name": "new-rule" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "id": 10 })))
            .expect(1)
            .mount(&server)
            .await;

        let mcp = write_mcp(&server.uri());
        let result = mcp
            .rules_create(Parameters(RulesCreateParams {
                name: "new-rule".to_string(),
                description: None,
            }))
            .await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    // Body assertion added beyond the plan's draft (Task 8's review flagged
    // this gap for channels_create/channels_update): without it, a swapped
    // rule_id/body argument order would still pass.
    #[tokio::test]
    async fn rules_update_uses_put_on_the_rule_id() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/rules/9"))
            .and(body_json(serde_json::json!({ "name": "renamed" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "id": 9 })))
            .expect(1)
            .mount(&server)
            .await;

        let mcp = write_mcp(&server.uri());
        let result = mcp
            .rules_update(Parameters(RulesUpdateParams {
                rule_id: 9,
                body: serde_json::json!({ "name": "renamed" }),
            }))
            .await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    #[tokio::test]
    async fn rules_delete_uses_delete_on_the_rule_id() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/api/rules/9"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "success": true })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mcp = write_mcp(&server.uri());
        let result = mcp
            .rules_delete(Parameters(RuleIdParams { rule_id: 9 }))
            .await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    #[tokio::test]
    async fn rules_execute_forwards_the_force_flag() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/rules/9/execute"))
            .and(body_json(serde_json::json!({ "force": true })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "executed": true })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mcp = write_mcp(&server.uri());
        let result = mcp
            .rules_execute(Parameters(RulesExecuteParams {
                rule_id: 9,
                force: true,
            }))
            .await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    // Body assertion added beyond the plan's draft (name says "full_body" --
    // make the test actually check that).
    #[tokio::test]
    async fn alarms_rule_create_posts_the_full_body() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "service_type": "io", "channel_id": 1001, "data_type": "T",
            "point_id": 5, "rule_name": "over-temp", "operator": ">", "value": 85.0
        });
        Mock::given(method("POST"))
            .and(path("/alarmApi/rules"))
            .and(body_json(body.clone()))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "id": 7 })))
            .expect(1)
            .mount(&server)
            .await;

        let mcp = write_mcp(&server.uri());
        let result = mcp
            .alarms_rule_create(Parameters(AlarmsRuleCreateParams { body }))
            .await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    // Body assertion added beyond the plan's draft, mirroring
    // AlarmClient::update_rule's own `update_rule_uses_put_and_forwards_the_body`
    // test in alarms.rs.
    #[tokio::test]
    async fn alarms_rule_update_uses_put_on_the_id() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/alarmApi/rules/7"))
            .and(body_json(serde_json::json!({ "value": 90.0 })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let mcp = write_mcp(&server.uri());
        let result = mcp
            .alarms_rule_update(Parameters(AlarmsRuleUpdateParams {
                id: 7,
                body: serde_json::json!({ "value": 90.0 }),
            }))
            .await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    #[tokio::test]
    async fn alarms_rule_delete_uses_delete_on_the_id() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/alarmApi/rules/7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let mcp = write_mcp(&server.uri());
        let result = mcp
            .alarms_rule_delete(Parameters(AlarmsRuleGetParams { id: 7 }))
            .await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    // Confirmed against AlarmClient::set_rule_enabled in alarms.rs: alarm
    // genuinely uses PATCH here (a documented, deliberate divergence from
    // automation's rules_enable/disable, which use POST) -- not a plan-drafting
    // error.
    #[tokio::test]
    async fn alarms_rule_enable_and_disable_use_patch_on_their_own_paths() {
        let enable_server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/alarmApi/rules/7/enable"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&enable_server)
            .await;
        let mcp = write_mcp(&enable_server.uri());
        let result = mcp
            .alarms_rule_enable(Parameters(AlarmsRuleGetParams { id: 7 }))
            .await;
        assert_ne!(result.is_error, Some(true), "{result:?}");

        let disable_server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/alarmApi/rules/7/disable"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&disable_server)
            .await;
        let mcp = write_mcp(&disable_server.uri());
        let result = mcp
            .alarms_rule_disable(Parameters(AlarmsRuleGetParams { id: 7 }))
            .await;
        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    #[tokio::test]
    async fn models_instances_action_posts_numeric_point_id_as_a_string() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/instances/3/action"))
            .and(header("authorization", "Bearer signed-access-token"))
            .and(body_json(serde_json::json!({
                "point_id": "1",
                "value": 4500.0,
                "confirmed": true
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let mcp = write_mcp(&server.uri());
        let result = mcp
            .models_instances_action(Parameters(ModelsInstancesActionParams {
                instance_id: 3,
                point_id: "1".to_string(),
                value: 4500.0,
            }))
            .await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    #[tokio::test]
    async fn models_instances_measurement_posts_to_the_measurement_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/instances/3/measurement"))
            .and(body_json(
                serde_json::json!({ "point_id": "101", "value": 650.5 }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let mcp = write_mcp(&server.uri());
        let result = mcp
            .models_instances_measurement(Parameters(ModelsInstancesMeasurementParams {
                instance_id: 3,
                point_id: "101".to_string(),
                value: 650.5,
            }))
            .await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    #[tokio::test]
    async fn net_mqtt_config_set_posts_the_body_verbatim() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/netApi/mqtt/config"))
            .and(body_json(
                serde_json::json!({ "host": "new", "port": 1883 }),
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "success": true })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mcp = write_mcp(&server.uri());
        let result = mcp
            .net_mqtt_config_set(Parameters(NetMqttConfigSetParams {
                config: serde_json::json!({ "host": "new", "port": 1883 }),
            }))
            .await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    #[tokio::test]
    async fn net_mqtt_reconnect_and_disconnect_hit_their_own_paths() {
        let reconnect_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/netApi/mqtt/reconnect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&reconnect_server)
            .await;
        let mcp = write_mcp(&reconnect_server.uri());
        let result = mcp.net_mqtt_reconnect().await;
        assert_ne!(result.is_error, Some(true), "{result:?}");

        let disconnect_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/netApi/mqtt/disconnect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&disconnect_server)
            .await;
        let mcp = write_mcp(&disconnect_server.uri());
        let result = mcp.net_mqtt_disconnect().await;
        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    #[tokio::test]
    async fn net_cert_upload_reads_the_file_and_posts_multipart() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/netApi/certificate/upload"))
            .and(wiremock::matchers::header_regex(
                "content-type",
                "^multipart/form-data; boundary=",
            ))
            .and(wiremock::matchers::body_string_contains(
                "name=\"cert_type\"",
            ))
            .and(wiremock::matchers::body_string_contains("client_key"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "success": true })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let cert_path = dir.path().join("ca.pem");
        std::fs::write(&cert_path, b"-----BEGIN CERTIFICATE-----\n").unwrap();

        let mcp = write_mcp(&server.uri());
        let result = mcp
            .net_cert_upload(Parameters(NetCertUploadParams {
                cert_type: "client_key".to_string(),
                file_path: cert_path.to_string_lossy().to_string(),
            }))
            .await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    #[tokio::test]
    async fn net_cert_upload_reports_a_missing_file_as_a_visible_tool_error() {
        let mcp = write_mcp("http://127.0.0.1:1");
        let result = mcp
            .net_cert_upload(Parameters(NetCertUploadParams {
                cert_type: "ca_cert".to_string(),
                file_path: "/nonexistent/ca.pem".to_string(),
            }))
            .await;

        assert_eq!(result.is_error, Some(true));
        let text = result
            .content
            .iter()
            .find_map(|c| c.as_text().map(|t| t.text.clone()))
            .expect("expected text content");
        assert!(text.contains("/nonexistent/ca.pem"), "{text}");
    }

    #[tokio::test]
    async fn net_cert_delete_uses_the_cert_type_in_the_path() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/netApi/certificate/client_key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let mcp = write_mcp(&server.uri());
        let result = mcp
            .net_cert_delete(Parameters(NetCertDeleteParams {
                cert_type: "client_key".to_string(),
            }))
            .await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
    }
}
