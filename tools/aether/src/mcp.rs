//! `aether mcp` — expose CLI capabilities as MCP tools.
//!
//! Read-only tools are always registered. `--allow-write` adds only high-risk
//! commands that already pass through transport-neutral application
//! capability, authorization, confirmation, and audit policy. Ungoverned
//! management mutations remain available through their compatibility CLI/HTTP
//! surfaces but are deliberately absent from MCP `tools/list`.
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

use aether_application::{
    CapabilityDescriptor, EXECUTE_RULE_CAPABILITY, MANAGE_ALARM_RULE_CAPABILITY,
    MANAGE_CHANNEL_CAPABILITY, MANAGE_ROUTING_CAPABILITY, MANAGE_RULE_CAPABILITY,
    RECONCILE_CHANNELS_CAPABILITY, RESOLVE_ALERT_CAPABILITY, WRITE_POINT_CAPABILITY,
};
use aether_pack::{ActivePackSet, load_active_packs};

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

/// The complete MCP write surface and its transport-neutral application
/// capability. Keep this mapping exact: `--allow-write` is only a registration
/// gate and never substitutes for per-invocation confirmation.
const MCP_WRITE_CAPABILITY_MAPPING: [(&str, CapabilityDescriptor); 22] = [
    ("channels_create", MANAGE_CHANNEL_CAPABILITY),
    ("channels_update", MANAGE_CHANNEL_CAPABILITY),
    ("channels_delete", MANAGE_CHANNEL_CAPABILITY),
    ("channels_enable", MANAGE_CHANNEL_CAPABILITY),
    ("channels_disable", MANAGE_CHANNEL_CAPABILITY),
    ("channels_reconcile", RECONCILE_CHANNELS_CAPABILITY),
    ("models_instances_action", WRITE_POINT_CAPABILITY),
    ("rules_execute", EXECUTE_RULE_CAPABILITY),
    ("rules_enable", MANAGE_RULE_CAPABILITY),
    ("rules_disable", MANAGE_RULE_CAPABILITY),
    ("rules_create", MANAGE_RULE_CAPABILITY),
    ("rules_update", MANAGE_RULE_CAPABILITY),
    ("rules_delete", MANAGE_RULE_CAPABILITY),
    ("alarms_rule_create", MANAGE_ALARM_RULE_CAPABILITY),
    ("alarms_rule_update", MANAGE_ALARM_RULE_CAPABILITY),
    ("alarms_rule_delete", MANAGE_ALARM_RULE_CAPABILITY),
    ("alarms_rule_enable", MANAGE_ALARM_RULE_CAPABILITY),
    ("alarms_rule_disable", MANAGE_ALARM_RULE_CAPABILITY),
    ("alarms_resolve", RESOLVE_ALERT_CAPABILITY),
    ("routing_action_upsert", MANAGE_ROUTING_CAPABILITY),
    ("routing_action_delete", MANAGE_ROUTING_CAPABILITY),
    ("routing_action_set_enabled", MANAGE_ROUTING_CAPABILITY),
];

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
    doc_resources: Vec<crate::mcp_docs::DocResource>,
    tool_router: ToolRouter<AetherMcp>,
}

pub(crate) struct BaseUrls {
    pub io: String,
    pub automation: String,
    pub alarm: String,
    pub uplink: String,
    pub history: String,
}

impl BaseUrls {
    /// Derives every domain base from the single API gateway base URL.
    /// The gateway proxies each capability domain under `/api/v1/{domain}`
    /// (ADR-0021); internal service ports are never addressed directly.
    pub(crate) fn from_api_base(api_base: &str) -> Self {
        let api = api_base.trim_end_matches('/');
        Self {
            io: format!("{api}/api/v1/io"),
            automation: format!("{api}/api/v1/automation"),
            alarm: format!("{api}/api/v1/alarm"),
            uplink: format!("{api}/api/v1/uplink"),
            history: format!("{api}/api/v1/history"),
        }
    }
}

impl AetherMcp {
    /// Constructs the fail-safe generic MCP surface with no domain Pack active.
    #[cfg(test)]
    pub(crate) fn new(urls: &BaseUrls, allow_write: bool) -> anyhow::Result<Self> {
        Self::with_active_packs(urls, allow_write, &ActivePackSet::empty())
    }

    /// Constructs MCP from the single shared `<config>/global.yaml` Pack entry.
    pub(crate) fn from_active_pack_config(
        urls: &BaseUrls,
        allow_write: bool,
        config_directory: &Path,
    ) -> anyhow::Result<Self> {
        let runtime_manifest = aether_runtime_catalog::load_runtime_manifest_for_current_process(
            config_directory,
            env!("CARGO_PKG_VERSION"),
        )?;
        let pack_runtime = runtime_manifest.pack_runtime()?;
        let active_packs = load_active_packs(config_directory, &pack_runtime)?;
        Self::with_active_packs(urls, allow_write, &active_packs)
    }

    fn with_active_packs(
        urls: &BaseUrls,
        allow_write: bool,
        active_packs: &ActivePackSet,
    ) -> anyhow::Result<Self> {
        let mut tool_router = Self::read_only_router();
        if allow_write {
            let write_router = Self::write_router();
            debug_assert_eq!(
                write_router.list_all().len(),
                MCP_WRITE_CAPABILITY_MAPPING.len()
            );
            tool_router += write_router;
        }

        #[cfg(test)]
        let alarms = AlarmClient::with_access_token(&urls.alarm, "mcp-test-access-token")?;
        #[cfg(not(test))]
        let alarms = AlarmClient::new(&urls.alarm)?;

        Ok(Self {
            channels: ChannelClient::new(&urls.io)?,
            points: PointClient::new(&urls.io)?,
            alarms,
            rules: RuleClient::new(&urls.automation)?,
            routing: RoutingClient::new(&urls.automation)?,
            history: HistoryClient::new(&urls.history)?,
            models: ModelClient::new(&urls.automation)?,
            templates: TemplateClient::new(&urls.io)?,
            net: NetClient::new(&urls.uplink)?,
            doc_resources: crate::mcp_docs::doc_resources(active_packs)?,
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
struct RuleMutationIdParams {
    /// Rule ID
    rule_id: i64,
    /// Explicitly confirms this high-risk rule-policy mutation.
    confirmed: bool,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct AlarmRuleMutationIdParams {
    /// Alarm rule ID
    id: i64,
    /// Explicitly confirms this high-risk alarm-policy mutation.
    confirmed: bool,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct AlertResolveParams {
    /// Active alert ID
    id: i64,
    /// Explicitly confirms that the active alert indication may be cleared.
    confirmed: bool,
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

#[cfg(test)]
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
    /// Whether the channel starts enabled. Defaults to false so creation is inert.
    #[serde(default)]
    #[schemars(default)]
    enabled: bool,
    /// Explicitly confirms this high-risk channel commissioning mutation.
    confirmed: bool,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ChannelsUpdateParams {
    /// Channel ID
    channel_id: u32,
    /// Partial update body -- only fields present are changed
    body: Value,
    /// Required desired-state compare-and-set revision from the latest channel read (minimum 1)
    #[schemars(range(min = 1))]
    expected_revision: u64,
    /// Explicitly confirms this high-risk channel commissioning mutation.
    confirmed: bool,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ChannelMutationIdParams {
    /// Channel ID
    channel_id: u32,
    /// Required desired-state compare-and-set revision from the latest channel read (minimum 1)
    #[schemars(range(min = 1))]
    expected_revision: u64,
    /// Explicitly confirms this high-risk channel commissioning mutation.
    confirmed: bool,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ChannelsReconcileParams {
    /// Explicitly confirms this high-risk, non-idempotent runtime reconciliation.
    confirmed: bool,
}

#[cfg(test)]
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
    /// Explicitly confirms this high-risk rule-policy mutation.
    confirmed: bool,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct RulesUpdateParams {
    /// Rule ID
    rule_id: i64,
    /// Partial update body -- only fields present are changed
    body: Value,
    /// Explicitly confirms this high-risk rule-policy mutation.
    confirmed: bool,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct RulesExecuteParams {
    /// Rule ID
    rule_id: i64,
    /// Explicitly confirms that the rule may dispatch real device commands.
    confirmed: bool,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct AlarmsRuleCreateParams {
    /// Full CreateRuleRequest body: service_type, channel_id, data_type,
    /// point_id, rule_name, operator, value, and optionally warning_level,
    /// enabled, description
    body: Value,
    /// Explicitly confirms this high-risk alarm-policy mutation.
    confirmed: bool,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct AlarmsRuleUpdateParams {
    /// Alarm rule ID
    id: i64,
    /// Partial UpdateRuleRequest body -- only fields present are changed
    body: Value,
    /// Explicitly confirms this high-risk alarm-policy mutation.
    confirmed: bool,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ModelsInstancesActionParams {
    /// Instance ID
    instance_id: u32,
    /// Numeric action point ID encoded as a string (for example, "1")
    point_id: String,
    /// Value to write
    value: f64,
    /// Explicitly confirms this high-risk device command.
    confirmed: bool,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct RoutingActionUpsertParams {
    /// Instance that owns the logical action point.
    instance_id: u32,
    /// Logical action-point ID within the instance model.
    action_point_id: u32,
    /// Physical destination channel.
    channel_id: u32,
    /// Physical command-owned point type: C or A.
    channel_type: String,
    /// Physical destination point ID within the channel.
    channel_point_id: u32,
    /// Whether the new route participates in command dispatch.
    #[serde(default = "default_routing_enabled")]
    enabled: bool,
    /// Explicitly confirms this high-risk physical topology change.
    confirmed: bool,
}

const fn default_routing_enabled() -> bool {
    true
}

#[derive(Deserialize, schemars::JsonSchema)]
struct RoutingActionDeleteParams {
    /// Instance that owns the logical action point.
    instance_id: u32,
    /// Logical action-point ID within the instance model.
    action_point_id: u32,
    /// Explicitly confirms this high-risk physical topology change.
    confirmed: bool,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct RoutingActionSetEnabledParams {
    /// Instance that owns the logical action point.
    instance_id: u32,
    /// Logical action-point ID within the instance model.
    action_point_id: u32,
    /// Whether the route participates in command dispatch.
    enabled: bool,
    /// Explicitly confirms this high-risk physical topology change.
    confirmed: bool,
}

#[cfg(test)]
#[derive(Deserialize, schemars::JsonSchema)]
struct NetMqttConfigSetParams {
    /// Complete NetConfig object (partial updates are not supported by uplink)
    config: Value,
}

#[cfg(test)]
#[derive(Deserialize, schemars::JsonSchema)]
struct NetCertUploadParams {
    /// Certificate role: ca_cert | client_cert | client_key
    cert_type: String,
    /// Path to the certificate file ON THE MACHINE RUNNING `aether mcp`
    /// (.pem/.crt/.key/.cer/.p12/.pfx, max 1 MB) -- not a path on the
    /// MCP client's machine.
    file_path: String,
}

#[cfg(test)]
#[derive(Deserialize, schemars::JsonSchema)]
struct NetCertDeleteParams {
    /// Certificate role: ca_cert | client_cert | client_key
    cert_type: String,
}

#[tool_router(router = write_router)]
impl AetherMcp {
    #[tool(
        description = "Create a communication channel through the authenticated, explicitly confirmed, and audited io.channel.manage application command. This is a high-risk, non-idempotent commissioning mutation; enabled defaults to false. Success may report a degraded runtime projection or incomplete completion audit: inspect request_id, resulting_revision, and reconciliation_required. Clients must not automatically retry.",
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
                    p.confirmed,
                )
                .await,
        )
    }

    #[tool(
        description = "Update a communication channel through the authenticated, explicitly confirmed, and audited io.channel.manage application command. This is a high-risk, non-idempotent commissioning mutation; expected_revision from the latest channel read is required as a compare-and-set guard. Success may report a degraded runtime projection or incomplete completion audit: inspect request_id, resulting_revision, and reconciliation_required. Clients must not automatically retry.",
        annotations(read_only_hint = false)
    )]
    async fn channels_update(
        &self,
        Parameters(p): Parameters<ChannelsUpdateParams>,
    ) -> CallToolResult {
        to_call_result(
            self.channels
                .update_channel(p.channel_id, p.body, p.confirmed, Some(p.expected_revision))
                .await,
        )
    }

    #[tool(
        description = "Delete a communication channel through the authenticated, explicitly confirmed, and audited io.channel.manage application command. Action-route references cause a conflict and are never silently cascaded. This is a high-risk, non-idempotent commissioning mutation; success may report a degraded runtime projection or incomplete completion audit. Inspect request_id, resulting_revision, and reconciliation_required. Clients must not automatically retry.",
        annotations(read_only_hint = false)
    )]
    async fn channels_delete(
        &self,
        Parameters(p): Parameters<ChannelMutationIdParams>,
    ) -> CallToolResult {
        to_call_result(
            self.channels
                .delete_channel(p.channel_id, p.confirmed, Some(p.expected_revision))
                .await,
        )
    }

    #[tool(
        description = "Enable a communication channel through the authenticated, explicitly confirmed, and audited io.channel.manage application command. This is a high-risk, non-idempotent lifecycle mutation; activation-pending or degraded is accepted and requires reconciliation, not proof of connectivity. Inspect request_id, resulting_revision, and reconciliation_required. Clients must not automatically retry, including when completion audit is incomplete.",
        annotations(read_only_hint = false)
    )]
    async fn channels_enable(
        &self,
        Parameters(p): Parameters<ChannelMutationIdParams>,
    ) -> CallToolResult {
        to_call_result(
            self.channels
                .set_enabled(p.channel_id, true, p.confirmed, Some(p.expected_revision))
                .await,
        )
    }

    #[tool(
        description = "Disable a communication channel through the authenticated, explicitly confirmed, and audited io.channel.manage application command. This is a high-risk, non-idempotent lifecycle mutation; a degraded runtime projection is accepted and requires reconciliation. Inspect request_id, resulting_revision, and reconciliation_required. Clients must not automatically retry, including when completion audit is incomplete.",
        annotations(read_only_hint = false)
    )]
    async fn channels_disable(
        &self,
        Parameters(p): Parameters<ChannelMutationIdParams>,
    ) -> CallToolResult {
        to_call_result(
            self.channels
                .set_enabled(p.channel_id, false, p.confirmed, Some(p.expected_revision))
                .await,
        )
    }

    #[tool(
        description = "Reconcile every channel runtime from authoritative desired state through the authenticated, explicitly confirmed, and audited io.channel.reconcile application command. This is a high-risk, non-idempotent operation that can reconnect protocol sessions. Inspect request_id, each sanitized item, degraded_count, reconciliation_required, and completion_audit. Clients must not automatically retry, including when runtime convergence or terminal audit remains incomplete.",
        annotations(read_only_hint = false)
    )]
    async fn channels_reconcile(
        &self,
        Parameters(p): Parameters<ChannelsReconcileParams>,
    ) -> CallToolResult {
        to_call_result(self.channels.reconcile_channels(p.confirmed).await)
    }

    #[tool(
        description = "Execute a rule now through the authenticated, explicitly confirmed, and audited application command. Selected device actions are accepted by the local command plane; success does not prove physical-device completion.",
        annotations(read_only_hint = false)
    )]
    async fn rules_execute(&self, Parameters(p): Parameters<RulesExecuteParams>) -> CallToolResult {
        to_call_result(self.rules.execute_rule(p.rule_id, p.confirmed).await)
    }

    #[tool(
        description = "Enable a business rule through the authenticated, explicitly confirmed, and audited rule-management application command. This is a high-risk rule-policy mutation; clients must not automatically retry an incomplete audit result.",
        annotations(read_only_hint = false)
    )]
    async fn rules_enable(
        &self,
        Parameters(p): Parameters<RuleMutationIdParams>,
    ) -> CallToolResult {
        to_call_result(self.rules.enable_rule(p.rule_id, p.confirmed).await)
    }

    #[tool(
        description = "Disable a business rule through the authenticated, explicitly confirmed, and audited rule-management application command. This is a high-risk rule-policy mutation; clients must not automatically retry an incomplete audit result.",
        annotations(read_only_hint = false)
    )]
    async fn rules_disable(
        &self,
        Parameters(p): Parameters<RuleMutationIdParams>,
    ) -> CallToolResult {
        to_call_result(self.rules.disable_rule(p.rule_id, p.confirmed).await)
    }

    #[tool(
        description = "Create a disabled business-rule shell through the authenticated, explicitly confirmed, and audited rule-management application command. This is a high-risk rule-policy mutation; clients must not automatically retry an incomplete audit result.",
        annotations(read_only_hint = false)
    )]
    async fn rules_create(&self, Parameters(p): Parameters<RulesCreateParams>) -> CallToolResult {
        to_call_result(
            self.rules
                .create_rule(&p.name, p.description.as_deref(), p.confirmed)
                .await,
        )
    }

    #[tool(
        description = "Update a business rule through the authenticated, explicitly confirmed, and audited rule-management application command. `body` is the partial rule update object. This is a high-risk rule-policy mutation; clients must not automatically retry an incomplete audit result.",
        annotations(read_only_hint = false)
    )]
    async fn rules_update(&self, Parameters(p): Parameters<RulesUpdateParams>) -> CallToolResult {
        to_call_result(self.rules.update_rule(p.rule_id, p.body, p.confirmed).await)
    }

    #[tool(
        description = "Delete a business rule through the authenticated, explicitly confirmed, and audited rule-management application command. This is a high-risk rule-policy mutation; clients must not automatically retry an incomplete audit result.",
        annotations(read_only_hint = false)
    )]
    async fn rules_delete(
        &self,
        Parameters(p): Parameters<RuleMutationIdParams>,
    ) -> CallToolResult {
        to_call_result(self.rules.delete_rule(p.rule_id, p.confirmed).await)
    }

    #[tool(
        description = "Create an alarm rule through the authenticated, explicitly confirmed, and audited alarm-policy application command. `body` must match the alarm CreateRuleRequest. This is a high-risk alarm-policy mutation; clients must not automatically retry an incomplete audit result.",
        annotations(read_only_hint = false)
    )]
    async fn alarms_rule_create(
        &self,
        Parameters(p): Parameters<AlarmsRuleCreateParams>,
    ) -> CallToolResult {
        to_call_result(self.alarms.create_rule(&p.body, p.confirmed).await)
    }

    #[tool(
        description = "Update an alarm rule through the authenticated, explicitly confirmed, and audited alarm-policy application command. `body` is a partial UpdateRuleRequest. This is a high-risk alarm-policy mutation; clients must not automatically retry an incomplete audit result.",
        annotations(read_only_hint = false)
    )]
    async fn alarms_rule_update(
        &self,
        Parameters(p): Parameters<AlarmsRuleUpdateParams>,
    ) -> CallToolResult {
        to_call_result(self.alarms.update_rule(p.id, &p.body, p.confirmed).await)
    }

    #[tool(
        description = "Delete an alarm rule through the authenticated, explicitly confirmed, and audited alarm-policy application command. This is a high-risk alarm-policy mutation; clients must not automatically retry an incomplete audit result.",
        annotations(read_only_hint = false)
    )]
    async fn alarms_rule_delete(
        &self,
        Parameters(p): Parameters<AlarmRuleMutationIdParams>,
    ) -> CallToolResult {
        to_call_result(self.alarms.delete_rule(p.id, p.confirmed).await)
    }

    #[tool(
        description = "Enable an alarm rule through the authenticated, explicitly confirmed, and audited alarm-policy application command. This is a high-risk alarm-policy mutation; clients must not automatically retry an incomplete audit result.",
        annotations(read_only_hint = false)
    )]
    async fn alarms_rule_enable(
        &self,
        Parameters(p): Parameters<AlarmRuleMutationIdParams>,
    ) -> CallToolResult {
        to_call_result(self.alarms.set_rule_enabled(p.id, true, p.confirmed).await)
    }

    #[tool(
        description = "Disable an alarm rule through the authenticated, explicitly confirmed, and audited alarm-policy application command. This is a high-risk alarm-policy mutation; clients must not automatically retry an incomplete audit result.",
        annotations(read_only_hint = false)
    )]
    async fn alarms_rule_disable(
        &self,
        Parameters(p): Parameters<AlarmRuleMutationIdParams>,
    ) -> CallToolResult {
        to_call_result(self.alarms.set_rule_enabled(p.id, false, p.confirmed).await)
    }

    #[tool(
        description = "Resolve an active alert through the authenticated, explicitly confirmed, and audited alert-resolution application command. This high-risk command clears an operator-visible alert indication; clients must not automatically retry an incomplete audit result.",
        annotations(read_only_hint = false)
    )]
    async fn alarms_resolve(
        &self,
        Parameters(p): Parameters<AlertResolveParams>,
    ) -> CallToolResult {
        to_call_result(self.alarms.resolve_alert(p.id, p.confirmed).await)
    }

    #[tool(
        description = "Submit a control action through the authenticated, explicitly confirmed, and audited application command. Success means the local command plane accepted it, not that the physical device executed it.",
        annotations(read_only_hint = false)
    )]
    async fn models_instances_action(
        &self,
        Parameters(p): Parameters<ModelsInstancesActionParams>,
    ) -> CallToolResult {
        to_call_result(
            self.models
                .execute_action(p.instance_id, &p.point_id, p.value, p.confirmed)
                .await,
        )
    }

    #[tool(
        description = "Change the physical C/A destination of an action route through the authenticated, explicitly confirmed, and audited application command. This is a high-risk topology mutation; success does not execute a device command, and clients must not automatically retry an incomplete audit or publication result.",
        annotations(read_only_hint = false)
    )]
    async fn routing_action_upsert(
        &self,
        Parameters(p): Parameters<RoutingActionUpsertParams>,
    ) -> CallToolResult {
        to_call_result(
            self.routing
                .upsert_action_route(
                    p.instance_id,
                    p.action_point_id,
                    p.channel_id,
                    &p.channel_type,
                    p.channel_point_id,
                    p.enabled,
                    p.confirmed,
                )
                .await,
        )
    }

    #[tool(
        description = "Delete an action route through the authenticated, explicitly confirmed, and audited application command. This is a high-risk physical-topology mutation; success does not execute a device command, and clients must not automatically retry an incomplete audit or publication result.",
        annotations(read_only_hint = false)
    )]
    async fn routing_action_delete(
        &self,
        Parameters(p): Parameters<RoutingActionDeleteParams>,
    ) -> CallToolResult {
        to_call_result(
            self.routing
                .delete_action_route(p.instance_id, p.action_point_id, p.confirmed)
                .await,
        )
    }

    #[tool(
        description = "Enable or disable an action route through the authenticated, explicitly confirmed, and audited application command. This is a high-risk physical-topology mutation; success does not execute a device command, and clients must not automatically retry an incomplete audit or publication result.",
        annotations(read_only_hint = false)
    )]
    async fn routing_action_set_enabled(
        &self,
        Parameters(p): Parameters<RoutingActionSetEnabledParams>,
    ) -> CallToolResult {
        to_call_result(
            self.routing
                .set_action_route_enabled(p.instance_id, p.action_point_id, p.enabled, p.confirmed)
                .await,
        )
    }
}

// Preserve direct wrapper coverage for management mutations that are not yet
// explicitly mapped into the production MCP write catalog. Some already have
// governed HTTP boundaries; none is registered merely because a wrapper
// exists.
#[cfg(test)]
#[tool_router(router = legacy_write_test_router)]
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
        let resources = self
            .doc_resources
            .iter()
            .map(|d| {
                let mut r = Resource::new(&d.uri, crate::mcp_docs::resource_name(&d.uri))
                    .with_mime_type("text/markdown");
                if let Some(title) = crate::mcp_docs::frontmatter_field(&d.body, "title") {
                    r = r.with_title(title);
                }
                if let Some(desc) = crate::mcp_docs::frontmatter_field(&d.body, "description") {
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
        let doc = self
            .doc_resources
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
    use wiremock::matchers::{body_json, header, header_exists, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn configured_pack_knowledge_requires_the_runtime_manifest() {
        let config = tempfile::tempdir().expect("temporary MCP config");
        std::fs::write(config.path().join("global.yaml"), "packs: []\n")
            .expect("empty Pack selection");

        let error = match AetherMcp::from_active_pack_config(
            &test_urls("http://localhost:1"),
            false,
            config.path(),
        ) {
            Ok(_) => panic!("production MCP startup must not use a static runtime fallback"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("runtime-manifest.json"));
    }

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

    #[test]
    fn base_urls_derive_every_domain_from_the_gateway_base() {
        let urls = BaseUrls::from_api_base("http://edge.example.test:6005/");
        assert_eq!(urls.io, "http://edge.example.test:6005/api/v1/io");
        assert_eq!(
            urls.automation,
            "http://edge.example.test:6005/api/v1/automation"
        );
        assert_eq!(urls.alarm, "http://edge.example.test:6005/api/v1/alarm");
        assert_eq!(urls.uplink, "http://edge.example.test:6005/api/v1/uplink");
        assert_eq!(urls.history, "http://edge.example.test:6005/api/v1/history");
    }

    /// Shorthand for the common "construct an --allow-write server against
    /// this mock's base URL" step shared by every write-tool test below.
    fn write_mcp(base: &str) -> AetherMcp {
        let mut server = AetherMcp::new(&test_urls(base), true).unwrap();
        server.channels = ChannelClient::with_access_token(base, "signed-access-token").unwrap();
        server.models = ModelClient::with_access_token(base, "signed-access-token").unwrap();
        server.alarms = AlarmClient::with_access_token(base, "signed-access-token").unwrap();
        server.rules =
            crate::rules::RuleClient::with_access_token(base, "signed-access-token").unwrap();
        server.routing = RoutingClient::with_access_token(base, "signed-access-token").unwrap();
        server
    }

    async fn mock_rules_revision(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/api/rules"))
            .and(query_param("page", "1"))
            .and(query_param("page_size", "1"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-aether-configuration-revision", "7")
                    .set_body_json(serde_json::json!({"data": {"list": []}})),
            )
            .expect(1)
            .mount(server)
            .await;
    }

    /// Mutations deliberately absent from the production MCP catalog. A name
    /// stays here until its application capability and exact MCP mapping are
    /// both reviewed; direct wrappers exist only for unit coverage.
    const UNEXPOSED_WRITE_TOOL_NAMES: &[&str] = &[
        "channels_write",
        "channels_points_batch",
        "net_mqtt_config_set",
        "net_mqtt_reconnect",
        "net_mqtt_disconnect",
        "net_cert_upload",
        "net_cert_delete",
    ];

    #[test]
    fn retired_instance_measurement_write_is_absent_from_capability_surfaces() {
        let mcp = AetherMcp::new(&test_urls("http://localhost:1"), true).unwrap();
        let names = mcp
            .tool_router
            .list_all()
            .iter()
            .map(|tool| tool.name.to_string())
            .collect::<Vec<_>>();
        assert!(
            !names.contains(&"models_instances_measurement".to_string()),
            "retired live-state write must not be registered as an MCP tool"
        );
    }

    #[test]
    fn every_exposed_mcp_write_maps_to_a_governed_application_capability() {
        use aether_application::{AuditPolicy, ConfirmationPolicy, OperationKind};

        let catalog = aether_application::capability_catalog();
        for (tool_name, mapped_capability) in MCP_WRITE_CAPABILITY_MAPPING {
            let catalog_capability = catalog
                .iter()
                .copied()
                .find(|capability| capability.name() == mapped_capability.name())
                .unwrap_or_else(|| panic!("{tool_name} maps to a capability absent from catalog"));

            assert_eq!(catalog_capability, mapped_capability, "{tool_name}");
            assert_eq!(
                mapped_capability.kind(),
                OperationKind::Command,
                "{tool_name}"
            );
            assert_eq!(
                mapped_capability.confirmation(),
                ConfirmationPolicy::Always,
                "{tool_name}"
            );
            assert_eq!(
                mapped_capability.audit_policy(),
                AuditPolicy::Required,
                "{tool_name}"
            );
        }

        let read_only = AetherMcp::new(&test_urls("http://localhost:1"), false).unwrap();
        let write_enabled = AetherMcp::new(&test_urls("http://localhost:1"), true).unwrap();
        let read_only_names = read_only
            .tool_router
            .list_all()
            .iter()
            .map(|tool| tool.name.to_string())
            .collect::<std::collections::BTreeSet<_>>();
        let exposed_writes = write_enabled
            .tool_router
            .list_all()
            .iter()
            .map(|tool| tool.name.to_string())
            .filter(|tool_name| !read_only_names.contains(tool_name))
            .collect::<std::collections::BTreeSet<_>>();
        let mapped_tools = MCP_WRITE_CAPABILITY_MAPPING
            .iter()
            .map(|(tool_name, _)| (*tool_name).to_string())
            .collect::<std::collections::BTreeSet<_>>();

        assert_eq!(exposed_writes, mapped_tools);
    }

    #[test]
    fn governed_policy_mutations_have_exact_capability_mappings_and_confirmation_schema() {
        use aether_application::{
            MANAGE_ALARM_RULE_CAPABILITY, MANAGE_RULE_CAPABILITY, RESOLVE_ALERT_CAPABILITY,
        };

        let expected = [
            ("rules_enable", MANAGE_RULE_CAPABILITY),
            ("rules_disable", MANAGE_RULE_CAPABILITY),
            ("rules_create", MANAGE_RULE_CAPABILITY),
            ("rules_update", MANAGE_RULE_CAPABILITY),
            ("rules_delete", MANAGE_RULE_CAPABILITY),
            ("alarms_rule_create", MANAGE_ALARM_RULE_CAPABILITY),
            ("alarms_rule_update", MANAGE_ALARM_RULE_CAPABILITY),
            ("alarms_rule_delete", MANAGE_ALARM_RULE_CAPABILITY),
            ("alarms_rule_enable", MANAGE_ALARM_RULE_CAPABILITY),
            ("alarms_rule_disable", MANAGE_ALARM_RULE_CAPABILITY),
            ("alarms_resolve", RESOLVE_ALERT_CAPABILITY),
        ];

        for (tool_name, capability) in expected {
            assert!(
                MCP_WRITE_CAPABILITY_MAPPING.contains(&(tool_name, capability)),
                "missing exact capability mapping for {tool_name}"
            );
        }

        let mcp = AetherMcp::new(&test_urls("http://localhost:1"), true).unwrap();
        let tools = mcp.tool_router.list_all();
        for (tool_name, _) in expected {
            let tool = tools
                .iter()
                .find(|tool| tool.name == tool_name)
                .unwrap_or_else(|| panic!("production write tool is absent: {tool_name}"));
            let required = tool
                .input_schema
                .get("required")
                .and_then(serde_json::Value::as_array)
                .expect("write tool schema has required properties");
            assert!(
                required.iter().any(|name| name == "confirmed"),
                "{tool_name} must require confirmed"
            );
            let description = tool.description.as_deref().unwrap_or_default();
            assert!(
                description.contains("high-risk"),
                "{tool_name} must disclose its high-risk classification"
            );
            assert!(
                description
                    .contains("clients must not automatically retry an incomplete audit result"),
                "{tool_name} must disclose terminal-audit retry semantics"
            );
        }

        let alert_description = tools
            .iter()
            .find(|tool| tool.name == "alarms_resolve")
            .and_then(|tool| tool.description.as_deref())
            .expect("alert-resolution description");
        assert!(
            alert_description.contains("clears an operator-visible alert indication"),
            "alert resolution must disclose its indication-clearing risk"
        );
    }

    #[test]
    fn governed_channel_commands_have_exact_capability_mapping_and_safe_schema() {
        use aether_application::{MANAGE_CHANNEL_CAPABILITY, RECONCILE_CHANNELS_CAPABILITY};

        let expected = [
            ("channels_create", MANAGE_CHANNEL_CAPABILITY),
            ("channels_update", MANAGE_CHANNEL_CAPABILITY),
            ("channels_delete", MANAGE_CHANNEL_CAPABILITY),
            ("channels_enable", MANAGE_CHANNEL_CAPABILITY),
            ("channels_disable", MANAGE_CHANNEL_CAPABILITY),
            ("channels_reconcile", RECONCILE_CHANNELS_CAPABILITY),
        ];
        for (tool_name, capability) in expected {
            assert!(
                MCP_WRITE_CAPABILITY_MAPPING.contains(&(tool_name, capability)),
                "missing exact capability mapping for {tool_name}"
            );
        }

        let mcp = AetherMcp::new(&test_urls("http://localhost:1"), true).unwrap();
        let tools = mcp.tool_router.list_all();
        for (tool_name, _) in expected {
            let tool = tools
                .iter()
                .find(|tool| tool.name == tool_name)
                .unwrap_or_else(|| panic!("production write tool is absent: {tool_name}"));
            let required = tool
                .input_schema
                .get("required")
                .and_then(serde_json::Value::as_array)
                .expect("channel write schema has required properties");
            assert!(
                required.iter().any(|name| name == "confirmed"),
                "{tool_name} must require confirmed"
            );
            let description = tool.description.as_deref().unwrap_or_default();
            assert!(description.contains("high-risk"), "{tool_name}");
            assert!(description.contains("degraded"), "{tool_name}");
            assert!(
                description.contains("must not automatically retry"),
                "{tool_name}"
            );
        }

        let create = tools
            .iter()
            .find(|tool| tool.name == "channels_create")
            .expect("create tool");
        assert!(
            !create
                .input_schema
                .get("required")
                .and_then(serde_json::Value::as_array)
                .expect("create required fields")
                .iter()
                .any(|name| name == "enabled"),
            "enabled must be optional and default false"
        );
        assert_eq!(
            create
                .input_schema
                .get("properties")
                .and_then(|properties| properties.get("enabled"))
                .and_then(|enabled| enabled.get("default")),
            Some(&serde_json::Value::Bool(false)),
            "enabled schema must advertise the inert false default"
        );
        assert!(
            create
                .input_schema
                .get("properties")
                .and_then(|properties| properties.get("expected_revision"))
                .is_none(),
            "create has no prior revision and must not expose expected_revision"
        );

        for tool_name in [
            "channels_update",
            "channels_delete",
            "channels_enable",
            "channels_disable",
        ] {
            let tool = tools
                .iter()
                .find(|tool| tool.name == tool_name)
                .unwrap_or_else(|| panic!("missing {tool_name}"));
            let revision = tool
                .input_schema
                .get("properties")
                .and_then(|properties| properties.get("expected_revision"))
                .unwrap_or_else(|| panic!("{tool_name} must expose expected_revision"));
            assert_eq!(revision.get("minimum").and_then(Value::as_u64), Some(1));
        }
    }

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
            .and(path("/mqtt/status"))
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
            .and(path("/mqtt/status"))
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
            .and(path("/mqtt/config"))
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
            .and(path("/certificate/info"))
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
            .and(path("/alerts"))
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
            .and(path("/alerts/7"))
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
            .and(path("/rules"))
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
            .and(path("/rules/12"))
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
            .and(path("/alert-events"))
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
            .and(path("/alert-statistics"))
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

    // NOTE: `HistoryClient::query_range` GETs `/data/query` (not
    // `/query` -- there's an extra `/data` segment shared with
    // `get_latest` below).
    #[tokio::test]
    async fn history_query_forwards_the_time_range() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/data/query"))
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

    // NOTE: `HistoryClient::get_latest` GETs `/data/latest` (not
    // `/latest`).
    #[tokio::test]
    async fn history_latest_uses_the_point_id() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/data/latest"))
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

        for (write_tool, _) in MCP_WRITE_CAPABILITY_MAPPING {
            assert!(!names.contains(&write_tool.to_string()), "{names:?}");
        }
        for unexposed_tool in UNEXPOSED_WRITE_TOOL_NAMES {
            assert!(!names.contains(&unexposed_tool.to_string()), "{names:?}");
        }
        // Route-count safety net catches a future write tool landing in the
        // wrong impl block or a name collision overwriting a read-only route.
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

        for (write_tool, _) in MCP_WRITE_CAPABILITY_MAPPING {
            assert!(names.contains(&write_tool.to_string()), "{names:?}");
        }
        for unexposed_tool in UNEXPOSED_WRITE_TOOL_NAMES {
            assert!(!names.contains(&unexposed_tool.to_string()), "{names:?}");
        }
        // Read-only tools are still present too -- --allow-write ADDS, doesn't replace.
        assert!(names.contains(&"channels_list".to_string()), "{names:?}");
        // Route-count safety net: 23 read-only + 22 governed writes.
        assert_eq!(names.len(), 45, "{names:?}");
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
            .and(header("authorization", "Bearer signed-access-token"))
            .and(header_exists("x-request-id"))
            .and(header("x-aether-confirmed", "true"))
            .and(body_json(serde_json::json!({
                "name": "new-channel",
                "protocol": "modbus",
                "parameters": { "host": "10.0.0.5", "port": 502 },
                "enabled": false,
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
                enabled: false,
                confirmed: true,
            }))
            .await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    #[tokio::test]
    async fn channels_update_uses_put_on_the_channel_id() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/channels/1001"))
            .and(header("authorization", "Bearer signed-access-token"))
            .and(header_exists("x-request-id"))
            .and(header("x-aether-confirmed", "true"))
            .and(header("x-aether-expected-revision", "7"))
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
                expected_revision: 7,
                confirmed: true,
            }))
            .await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    #[tokio::test]
    async fn channels_delete_uses_delete_on_the_channel_id() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/api/channels/1001"))
            .and(header("authorization", "Bearer signed-access-token"))
            .and(header_exists("x-request-id"))
            .and(header("x-aether-confirmed", "true"))
            .and(header("x-aether-expected-revision", "7"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "success": true })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mcp = write_mcp(&server.uri());
        let result = mcp
            .channels_delete(Parameters(ChannelMutationIdParams {
                channel_id: 1001,
                expected_revision: 7,
                confirmed: true,
            }))
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
            .and(header("authorization", "Bearer signed-access-token"))
            .and(header_exists("x-request-id"))
            .and(header("x-aether-confirmed", "true"))
            .and(header("x-aether-expected-revision", "7"))
            .and(body_json(serde_json::json!({ "enabled": true })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&enable_server)
            .await;
        let mcp = write_mcp(&enable_server.uri());
        let result = mcp
            .channels_enable(Parameters(ChannelMutationIdParams {
                channel_id: 1001,
                expected_revision: 7,
                confirmed: true,
            }))
            .await;
        assert_ne!(result.is_error, Some(true), "{result:?}");

        let disable_server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/channels/1001/enabled"))
            .and(header("authorization", "Bearer signed-access-token"))
            .and(header_exists("x-request-id"))
            .and(header("x-aether-confirmed", "true"))
            .and(header("x-aether-expected-revision", "8"))
            .and(body_json(serde_json::json!({ "enabled": false })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&disable_server)
            .await;
        let mcp = write_mcp(&disable_server.uri());
        let result = mcp
            .channels_disable(Parameters(ChannelMutationIdParams {
                channel_id: 1001,
                expected_revision: 8,
                confirmed: true,
            }))
            .await;
        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    fn channel_reconciliation_response() -> Value {
        serde_json::json!({
            "success": true,
            "data": {
                "request_id": "018f2a74-5700-7f42-9da4-73b247c9c003",
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
    async fn channels_reconcile_uses_the_canonical_governed_endpoint_and_typed_receipt() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/channels/reconcile"))
            .and(header("authorization", "Bearer signed-access-token"))
            .and(header_exists("x-request-id"))
            .and(header("x-aether-confirmed", "true"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(channel_reconciliation_response()),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mcp = write_mcp(&server.uri());
        let result = mcp
            .channels_reconcile(Parameters(ChannelsReconcileParams { confirmed: true }))
            .await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
        let structured = result
            .structured_content
            .expect("structured reconciliation receipt");
        assert_eq!(structured["data"]["scope"], "all");
        assert_eq!(structured["data"]["reconciliation_required"], true);
        assert_eq!(
            structured["data"]["completion_audit"]["status"],
            "incomplete"
        );
        assert_eq!(structured["data"]["retryable"], false);

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
    async fn channels_reconcile_rejects_remote_plaintext_before_http() {
        let mcp = write_mcp("http://192.0.2.10:6001");
        let result = mcp
            .channels_reconcile(Parameters(ChannelsReconcileParams { confirmed: true }))
            .await;

        assert_eq!(result.is_error, Some(true), "{result:?}");
        let rendered = serde_json::to_string(&result).expect("serialize MCP result");
        assert!(rendered.contains("non-loopback plaintext"), "{rendered}");
    }

    #[tokio::test]
    async fn every_unconfirmed_channel_tool_stops_before_http() {
        let server = MockServer::start().await;
        let mcp = write_mcp(&server.uri());

        let results = [
            mcp.channels_create(Parameters(ChannelsCreateParams {
                name: "blocked".to_string(),
                protocol: "virtual".to_string(),
                parameters: serde_json::json!({}),
                description: None,
                id: None,
                enabled: false,
                confirmed: false,
            }))
            .await,
            mcp.channels_update(Parameters(ChannelsUpdateParams {
                channel_id: 1001,
                body: serde_json::json!({"name": "blocked"}),
                expected_revision: 7,
                confirmed: false,
            }))
            .await,
            mcp.channels_delete(Parameters(ChannelMutationIdParams {
                channel_id: 1001,
                expected_revision: 7,
                confirmed: false,
            }))
            .await,
            mcp.channels_enable(Parameters(ChannelMutationIdParams {
                channel_id: 1001,
                expected_revision: 7,
                confirmed: false,
            }))
            .await,
            mcp.channels_disable(Parameters(ChannelMutationIdParams {
                channel_id: 1001,
                expected_revision: 7,
                confirmed: false,
            }))
            .await,
            mcp.channels_reconcile(Parameters(ChannelsReconcileParams { confirmed: false }))
                .await,
        ];

        assert!(
            results.iter().all(|result| result.is_error == Some(true)),
            "{results:?}"
        );
        assert!(server.received_requests().await.unwrap().is_empty());
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
        mock_rules_revision(&enable_server).await;
        Mock::given(method("POST"))
            .and(path("/api/rules/9/enable"))
            .and(header("authorization", "Bearer signed-access-token"))
            .and(header_exists("x-request-id"))
            .and(body_json(serde_json::json!({
                "confirmed": true,
                "expected_revision": 7
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&enable_server)
            .await;
        let mcp = write_mcp(&enable_server.uri());
        let result = mcp
            .rules_enable(Parameters(RuleMutationIdParams {
                rule_id: 9,
                confirmed: true,
            }))
            .await;
        assert_ne!(result.is_error, Some(true), "{result:?}");

        let disable_server = MockServer::start().await;
        mock_rules_revision(&disable_server).await;
        Mock::given(method("POST"))
            .and(path("/api/rules/9/disable"))
            .and(header("authorization", "Bearer signed-access-token"))
            .and(header_exists("x-request-id"))
            .and(body_json(serde_json::json!({
                "confirmed": true,
                "expected_revision": 7
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&disable_server)
            .await;
        let mcp = write_mcp(&disable_server.uri());
        let result = mcp
            .rules_disable(Parameters(RuleMutationIdParams {
                rule_id: 9,
                confirmed: true,
            }))
            .await;
        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    // Body assertion added beyond the plan's draft: rules_create only ever
    // sends "name" here (description is None), so this also proves an
    // Option::None doesn't leak a `"description": null` field into the body.
    #[tokio::test]
    async fn rules_create_posts_the_name_and_description() {
        let server = MockServer::start().await;
        mock_rules_revision(&server).await;
        Mock::given(method("POST"))
            .and(path("/api/rules"))
            .and(header("authorization", "Bearer signed-access-token"))
            .and(header_exists("x-request-id"))
            .and(body_json(serde_json::json!({
                "name": "new-rule",
                "confirmed": true,
                "expected_revision": 7
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "id": 10 })))
            .expect(1)
            .mount(&server)
            .await;

        let mcp = write_mcp(&server.uri());
        let result = mcp
            .rules_create(Parameters(RulesCreateParams {
                name: "new-rule".to_string(),
                description: None,
                confirmed: true,
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
        mock_rules_revision(&server).await;
        Mock::given(method("PUT"))
            .and(path("/api/rules/9"))
            .and(header("authorization", "Bearer signed-access-token"))
            .and(header_exists("x-request-id"))
            .and(body_json(serde_json::json!({
                "name": "renamed",
                "confirmed": true,
                "expected_revision": 7
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "id": 9 })))
            .expect(1)
            .mount(&server)
            .await;

        let mcp = write_mcp(&server.uri());
        let result = mcp
            .rules_update(Parameters(RulesUpdateParams {
                rule_id: 9,
                body: serde_json::json!({ "name": "renamed" }),
                confirmed: true,
            }))
            .await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    #[tokio::test]
    async fn rules_delete_uses_delete_on_the_rule_id() {
        let server = MockServer::start().await;
        mock_rules_revision(&server).await;
        Mock::given(method("DELETE"))
            .and(path("/api/rules/9"))
            .and(header("authorization", "Bearer signed-access-token"))
            .and(header_exists("x-request-id"))
            .and(body_json(serde_json::json!({
                "confirmed": true,
                "expected_revision": 7
            })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "success": true })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mcp = write_mcp(&server.uri());
        let result = mcp
            .rules_delete(Parameters(RuleMutationIdParams {
                rule_id: 9,
                confirmed: true,
            }))
            .await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    #[tokio::test]
    async fn rules_execute_forwards_authenticated_confirmation() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/rules/9/execute"))
            .and(header("authorization", "Bearer signed-access-token"))
            .and(body_json(serde_json::json!({ "confirmed": true })))
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
                confirmed: true,
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
            .and(path("/rules"))
            .and(header("authorization", "Bearer signed-access-token"))
            .and(header_exists("x-request-id"))
            .and(header("x-aether-confirmed", "true"))
            .and(body_json(body.clone()))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "id": 7 })))
            .expect(1)
            .mount(&server)
            .await;

        let mcp = write_mcp(&server.uri());
        let result = mcp
            .alarms_rule_create(Parameters(AlarmsRuleCreateParams {
                body,
                confirmed: true,
            }))
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
            .and(path("/rules/7"))
            .and(header("authorization", "Bearer signed-access-token"))
            .and(header_exists("x-request-id"))
            .and(header("x-aether-confirmed", "true"))
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
                confirmed: true,
            }))
            .await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    #[tokio::test]
    async fn alarms_rule_delete_uses_delete_on_the_id() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/rules/7"))
            .and(header("authorization", "Bearer signed-access-token"))
            .and(header_exists("x-request-id"))
            .and(header("x-aether-confirmed", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let mcp = write_mcp(&server.uri());
        let result = mcp
            .alarms_rule_delete(Parameters(AlarmRuleMutationIdParams {
                id: 7,
                confirmed: true,
            }))
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
            .and(path("/rules/7/enable"))
            .and(header("authorization", "Bearer signed-access-token"))
            .and(header_exists("x-request-id"))
            .and(header("x-aether-confirmed", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&enable_server)
            .await;
        let mcp = write_mcp(&enable_server.uri());
        let result = mcp
            .alarms_rule_enable(Parameters(AlarmRuleMutationIdParams {
                id: 7,
                confirmed: true,
            }))
            .await;
        assert_ne!(result.is_error, Some(true), "{result:?}");

        let disable_server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/rules/7/disable"))
            .and(header("authorization", "Bearer signed-access-token"))
            .and(header_exists("x-request-id"))
            .and(header("x-aether-confirmed", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&disable_server)
            .await;
        let mcp = write_mcp(&disable_server.uri());
        let result = mcp
            .alarms_rule_disable(Parameters(AlarmRuleMutationIdParams {
                id: 7,
                confirmed: true,
            }))
            .await;
        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    #[tokio::test]
    async fn alarms_resolve_forwards_authenticated_confirmation() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/alerts/12/resolve"))
            .and(header("authorization", "Bearer signed-access-token"))
            .and(header_exists("x-request-id"))
            .and(header("x-aether-confirmed", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let mcp = write_mcp(&server.uri());
        let result = mcp
            .alarms_resolve(Parameters(AlertResolveParams {
                id: 12,
                confirmed: true,
            }))
            .await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    #[tokio::test]
    async fn policy_mutations_reject_unconfirmed_before_any_http_request() {
        let server = MockServer::start().await;
        let mcp = write_mcp(&server.uri());

        let results = [
            mcp.rules_enable(Parameters(RuleMutationIdParams {
                rule_id: 9,
                confirmed: false,
            }))
            .await,
            mcp.rules_disable(Parameters(RuleMutationIdParams {
                rule_id: 9,
                confirmed: false,
            }))
            .await,
            mcp.rules_create(Parameters(RulesCreateParams {
                name: "blocked".to_string(),
                description: None,
                confirmed: false,
            }))
            .await,
            mcp.rules_update(Parameters(RulesUpdateParams {
                rule_id: 9,
                body: serde_json::json!({ "name": "blocked" }),
                confirmed: false,
            }))
            .await,
            mcp.rules_delete(Parameters(RuleMutationIdParams {
                rule_id: 9,
                confirmed: false,
            }))
            .await,
            mcp.alarms_rule_create(Parameters(AlarmsRuleCreateParams {
                body: serde_json::json!({}),
                confirmed: false,
            }))
            .await,
            mcp.alarms_rule_update(Parameters(AlarmsRuleUpdateParams {
                id: 7,
                body: serde_json::json!({}),
                confirmed: false,
            }))
            .await,
            mcp.alarms_rule_delete(Parameters(AlarmRuleMutationIdParams {
                id: 7,
                confirmed: false,
            }))
            .await,
            mcp.alarms_rule_enable(Parameters(AlarmRuleMutationIdParams {
                id: 7,
                confirmed: false,
            }))
            .await,
            mcp.alarms_rule_disable(Parameters(AlarmRuleMutationIdParams {
                id: 7,
                confirmed: false,
            }))
            .await,
            mcp.alarms_resolve(Parameters(AlertResolveParams {
                id: 12,
                confirmed: false,
            }))
            .await,
        ];

        assert!(
            results.iter().all(|result| result.is_error == Some(true)),
            "every unconfirmed mutation must fail: {results:?}"
        );
        assert!(
            server.received_requests().await.unwrap().is_empty(),
            "unconfirmed MCP policy mutations must perform zero HTTP requests"
        );
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
                confirmed: true,
            }))
            .await;

        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    #[tokio::test]
    async fn models_instances_action_rejects_unconfirmed_before_http() {
        let server = MockServer::start().await;
        let mcp = write_mcp(&server.uri());

        let result = mcp
            .models_instances_action(Parameters(ModelsInstancesActionParams {
                instance_id: 3,
                point_id: "1".to_string(),
                value: 4500.0,
                confirmed: false,
            }))
            .await;

        assert_eq!(result.is_error, Some(true), "{result:?}");
        assert!(
            server.received_requests().await.unwrap().is_empty(),
            "--allow-write must not substitute for per-call confirmation"
        );
    }

    #[tokio::test]
    async fn routing_action_upsert_forwards_the_governed_topology_command() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/instances/7/actions/1/routing"))
            .and(header("authorization", "Bearer signed-access-token"))
            .and(header_exists("x-request-id"))
            .and(body_json(serde_json::json!({
                "channel_id": 3,
                "four_remote": "A",
                "channel_point_id": 5,
                "enabled": true,
                "confirmed": true
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let mcp = write_mcp(&server.uri());
        let result = mcp
            .routing_action_upsert(Parameters(RoutingActionUpsertParams {
                instance_id: 7,
                action_point_id: 1,
                channel_id: 3,
                channel_type: "A".to_string(),
                channel_point_id: 5,
                enabled: true,
                confirmed: true,
            }))
            .await;
        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    #[tokio::test]
    async fn routing_action_delete_rejects_unconfirmed_before_http() {
        let server = MockServer::start().await;
        let mcp = write_mcp(&server.uri());

        let result = mcp
            .routing_action_delete(Parameters(RoutingActionDeleteParams {
                instance_id: 7,
                action_point_id: 1,
                confirmed: false,
            }))
            .await;
        assert_eq!(result.is_error, Some(true), "{result:?}");
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn routing_action_set_enabled_forwards_the_governed_topology_command() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/instances/7/actions/1/routing"))
            .and(header("authorization", "Bearer signed-access-token"))
            .and(header_exists("x-request-id"))
            .and(body_json(serde_json::json!({
                "enabled": false,
                "confirmed": true
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let mcp = write_mcp(&server.uri());
        let result = mcp
            .routing_action_set_enabled(Parameters(RoutingActionSetEnabledParams {
                instance_id: 7,
                action_point_id: 1,
                enabled: false,
                confirmed: true,
            }))
            .await;
        assert_ne!(result.is_error, Some(true), "{result:?}");
    }

    #[tokio::test]
    async fn net_mqtt_config_set_posts_the_body_verbatim() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/mqtt/config"))
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
            .and(path("/mqtt/reconnect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&reconnect_server)
            .await;
        let mcp = write_mcp(&reconnect_server.uri());
        let result = mcp.net_mqtt_reconnect().await;
        assert_ne!(result.is_error, Some(true), "{result:?}");

        let disconnect_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/mqtt/disconnect"))
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
            .and(path("/certificate/upload"))
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
            .and(path("/certificate/client_key"))
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

    #[test]
    fn channel_mutation_mcp_schemas_require_expected_revision() {
        for schema in [
            serde_json::to_value(schemars::schema_for!(ChannelsUpdateParams)).unwrap(),
            serde_json::to_value(schemars::schema_for!(ChannelMutationIdParams)).unwrap(),
        ] {
            let required = schema["required"]
                .as_array()
                .expect("object schema must declare required properties");
            assert!(
                required
                    .iter()
                    .any(|property| property == "expected_revision"),
                "expected_revision must be mandatory in MCP mutation schema: {schema}"
            );
        }
    }
}
