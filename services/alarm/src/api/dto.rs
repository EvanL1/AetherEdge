//! HTTP/OpenAPI DTOs for the alarm service.

use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::models;

fn decoded_snapshot(raw: &str) -> serde_json::Value {
    serde_json::from_str(raw).unwrap_or_else(|_| serde_json::Value::String(raw.to_string()))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AlertRule {
    pub id: i64,
    pub service_type: String,
    pub channel_id: i64,
    pub data_type: String,
    pub point_id: i64,
    pub rule_name: String,
    pub warning_level: i64,
    pub operator: String,
    pub value: f64,
    pub enabled: bool,
    pub description: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<models::AlertRule> for AlertRule {
    fn from(value: models::AlertRule) -> Self {
        Self {
            id: value.id,
            service_type: value.service_type,
            channel_id: value.channel_id,
            data_type: value.data_type,
            point_id: value.point_id,
            rule_name: value.rule_name,
            warning_level: value.warning_level,
            operator: value.operator,
            value: value.value,
            enabled: value.enabled,
            description: value.description,
            created_at: value.created_at,
            updated_at: value.updated_at,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Alert {
    pub id: i64,
    pub rule_id: i64,
    pub rule_snapshot: serde_json::Value,
    pub service_type: String,
    pub channel_id: i64,
    pub data_type: String,
    pub point_id: i64,
    pub rule_name: String,
    pub warning_level: i64,
    pub operator: String,
    pub threshold_value: f64,
    pub current_value: f64,
    pub status: String,
    pub triggered_at: i64,
}

impl From<models::Alert> for Alert {
    fn from(value: models::Alert) -> Self {
        Self {
            id: value.id,
            rule_id: value.rule_id,
            rule_snapshot: decoded_snapshot(&value.rule_snapshot),
            service_type: value.service_type,
            channel_id: value.channel_id,
            data_type: value.data_type,
            point_id: value.point_id,
            rule_name: value.rule_name,
            warning_level: value.warning_level,
            operator: value.operator,
            threshold_value: value.threshold_value,
            current_value: value.current_value,
            status: value.status,
            triggered_at: value.triggered_at,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AlertEvent {
    pub id: i64,
    pub rule_id: i64,
    pub rule_snapshot: serde_json::Value,
    pub service_type: String,
    pub channel_id: i64,
    pub data_type: String,
    pub point_id: i64,
    pub rule_name: String,
    pub warning_level: i64,
    pub operator: String,
    pub threshold_value: f64,
    pub trigger_value: Option<f64>,
    pub recovery_value: Option<f64>,
    pub event_type: String,
    pub triggered_at: Option<i64>,
    pub recovered_at: Option<i64>,
    pub duration: Option<i64>,
}

impl From<models::AlertEvent> for AlertEvent {
    fn from(value: models::AlertEvent) -> Self {
        Self {
            id: value.id,
            rule_id: value.rule_id,
            rule_snapshot: decoded_snapshot(&value.rule_snapshot),
            service_type: value.service_type,
            channel_id: value.channel_id,
            data_type: value.data_type,
            point_id: value.point_id,
            rule_name: value.rule_name,
            warning_level: value.warning_level,
            operator: value.operator,
            threshold_value: value.threshold_value,
            trigger_value: value.trigger_value,
            recovery_value: value.recovery_value,
            event_type: value.event_type,
            triggered_at: value.triggered_at,
            recovered_at: value.recovered_at,
            duration: value.duration,
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateRuleRequest {
    pub service_type: String,
    pub channel_id: i64,
    pub data_type: String,
    pub point_id: i64,
    pub rule_name: String,
    #[serde(default = "default_warning_level")]
    pub warning_level: i64,
    pub operator: String,
    pub value: f64,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateRuleRequest {
    pub service_type: Option<String>,
    pub channel_id: Option<i64>,
    pub data_type: Option<String>,
    pub point_id: Option<i64>,
    pub rule_name: Option<String>,
    pub warning_level: Option<i64>,
    pub operator: Option<String>,
    pub value: Option<f64>,
    pub enabled: Option<bool>,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize, Default, IntoParams)]
pub struct RuleQueryParams {
    pub keyword: Option<String>,
    pub service_type: Option<String>,
    pub channel_id: Option<i64>,
    pub data_type: Option<String>,
    pub enabled: Option<bool>,
    pub warning_level: Option<i64>,
    pub page: Option<i64>,
    pub page_size: Option<i64>,
    #[serde(default)]
    pub skip: i64,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

impl From<RuleQueryParams> for models::RuleFilter {
    fn from(value: RuleQueryParams) -> Self {
        Self {
            keyword: value.keyword,
            service_type: value.service_type,
            channel_id: value.channel_id,
            data_type: value.data_type,
            enabled: value.enabled,
            warning_level: value.warning_level,
            page: models::PageRequest::resolve(
                value.page,
                value.page_size,
                value.skip,
                value.limit,
            ),
        }
    }
}

#[derive(Debug, Deserialize, Default, IntoParams)]
pub struct AlertQueryParams {
    pub warning_level: Option<i64>,
    pub service_type: Option<String>,
    pub channel_id: Option<i64>,
    pub keyword: Option<String>,
    pub page: Option<i64>,
    pub page_size: Option<i64>,
    #[serde(default)]
    pub skip: i64,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

impl From<AlertQueryParams> for models::AlertFilter {
    fn from(value: AlertQueryParams) -> Self {
        Self {
            warning_level: value.warning_level,
            service_type: value.service_type,
            channel_id: value.channel_id,
            keyword: value.keyword,
            page: models::PageRequest::resolve(
                value.page,
                value.page_size,
                value.skip,
                value.limit,
            ),
        }
    }
}

#[derive(Debug, Deserialize, Default, IntoParams)]
pub struct EventQueryParams {
    pub keyword: Option<String>,
    pub rule_id: Option<i64>,
    pub event_type: Option<String>,
    pub service_type: Option<String>,
    pub warning_level: Option<i64>,
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
    pub page: Option<i64>,
    pub page_size: Option<i64>,
    #[serde(default)]
    pub skip: i64,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

impl From<EventQueryParams> for models::EventFilter {
    fn from(value: EventQueryParams) -> Self {
        Self {
            keyword: value.keyword,
            rule_id: value.rule_id,
            event_type: value.event_type,
            service_type: value.service_type,
            warning_level: value.warning_level,
            start_time: value.start_time,
            end_time: value.end_time,
            page: models::PageRequest::resolve(
                value.page,
                value.page_size,
                value.skip,
                value.limit,
            ),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ApiResponse<T: Serialize> {
    pub success: bool,
    pub message: String,
    pub data: T,
}

impl<T: Serialize> ApiResponse<T> {
    pub fn ok(message: impl Into<String>, data: T) -> Self {
        Self {
            success: true,
            message: message.into(),
            data,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CreateRuleData {
    pub rule_id: u64,
    pub rule_name: String,
    pub logical_key: Option<String>,
    pub point_id: i64,
    pub monitoring: bool,
    pub rule: Option<AlertRule>,
    pub request_id: String,
    pub audit: CompletionAuditData,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SingleItemData<T: Serialize> {
    pub total: i64,
    pub list: Vec<T>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RuleIdData {
    pub rule_id: u64,
    pub request_id: String,
    pub audit: CompletionAuditData,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AlertResolutionData {
    pub alert_id: u64,
    pub rule_id: u64,
    pub resolved_at_ms: u64,
    pub request_id: String,
    pub audit: CompletionAuditData,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CompletionAuditData {
    pub status: String,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PagedData<T: Serialize> {
    pub total: i64,
    pub list: Vec<T>,
    pub page: i64,
    pub page_size: i64,
}

impl<T: Serialize> PagedData<T> {
    pub fn from_page<U>(value: models::Page<U>) -> Self
    where
        T: From<U>,
    {
        Self {
            total: value.total,
            list: value.list.into_iter().map(Into::into).collect(),
            page: value.page,
            page_size: value.page_size,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MonitorStatus {
    pub running: bool,
    pub last_check_time: Option<i64>,
    pub check_interval: u64,
}

impl From<&models::MonitorStatus> for MonitorStatus {
    fn from(value: &models::MonitorStatus) -> Self {
        Self {
            running: value.running,
            last_check_time: value.last_check_time,
            check_interval: value.check_interval,
        }
    }
}

fn default_warning_level() -> i64 {
    2
}

fn default_true() -> bool {
    true
}

fn default_limit() -> i64 {
    20
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_pagination_is_normalized_before_database_access() {
        let filter = models::RuleFilter::from(RuleQueryParams {
            skip: 40,
            limit: 20,
            ..RuleQueryParams::default()
        });
        assert_eq!(filter.page.offset, 40);
        assert_eq!(filter.page.limit, 20);
        assert_eq!(filter.page.page, 3);
    }
}
