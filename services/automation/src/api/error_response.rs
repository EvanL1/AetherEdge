//! HTTP status and response projection for [`AutomationError`].

use axum::response::IntoResponse;
use errors::AetherErrorTrait;

use crate::error::AutomationError;

impl AetherErrorTrait for AutomationError {
    fn error_code(&self) -> &'static str {
        match self {
            AutomationError::ConfigError(_) => "AUTOMATION_CONFIG_ERROR",
            AutomationError::InvalidConfig(_) => "AUTOMATION_INVALID_CONFIG",
            AutomationError::MissingConfig(_) => "AUTOMATION_MISSING_CONFIG",
            AutomationError::DatabaseError(_) => "AUTOMATION_DATABASE_ERROR",
            AutomationError::InstanceNotFound(_) => "AUTOMATION_INSTANCE_NOT_FOUND",
            AutomationError::InstanceExists(_) => "AUTOMATION_INSTANCE_EXISTS",
            AutomationError::RuleNotFound(_) => "AUTOMATION_RULE_NOT_FOUND",
            AutomationError::RuleExists(_) => "AUTOMATION_RULE_EXISTS",
            AutomationError::InvalidRule(_) => "AUTOMATION_INVALID_RULE",
            AutomationError::ParseError(_) => "AUTOMATION_PARSE_ERROR",
            AutomationError::ExecutionError(_) => "AUTOMATION_EXECUTION_ERROR",
            AutomationError::SchedulerError(_) => "AUTOMATION_SCHEDULER_ERROR",
            AutomationError::InvalidData(_) => "AUTOMATION_INVALID_DATA",
            AutomationError::InvalidRouting(_) => "AUTOMATION_INVALID_ROUTING",
            AutomationError::RoutingConflict(_) => "AUTOMATION_ROUTING_CONFLICT",
            AutomationError::ConfigurationConflict(_) => "AUTOMATION_CONFIGURATION_CONFLICT",
            AutomationError::AuthorizationDenied(_) => "AUTOMATION_AUTHORIZATION_DENIED",
            AutomationError::AuditUnavailable(_) => "AUTOMATION_AUDIT_UNAVAILABLE",
            AutomationError::SerializationError(_) => "AUTOMATION_SERIALIZATION_ERROR",
            AutomationError::DispatchDegraded(_) => "AUTOMATION_DISPATCH_DEGRADED",
            AutomationError::ChannelUnreachable { .. } => "AUTOMATION_CHANNEL_UNREACHABLE",
            AutomationError::InternalError(_) => "AUTOMATION_INTERNAL_ERROR",
        }
    }

    fn category(&self) -> errors::ErrorCategory {
        use errors::ErrorCategory;

        match self {
            AutomationError::ConfigError(_)
            | AutomationError::InvalidConfig(_)
            | AutomationError::MissingConfig(_) => ErrorCategory::Configuration,
            AutomationError::DatabaseError(_) => ErrorCategory::Database,
            AutomationError::InstanceNotFound(_) | AutomationError::RuleNotFound(_) => {
                ErrorCategory::NotFound
            },
            AutomationError::InstanceExists(_)
            | AutomationError::RuleExists(_)
            | AutomationError::RoutingConflict(_)
            | AutomationError::ConfigurationConflict(_) => ErrorCategory::Conflict,
            AutomationError::InvalidData(_)
            | AutomationError::InvalidRouting(_)
            | AutomationError::InvalidRule(_)
            | AutomationError::ParseError(_) => ErrorCategory::Validation,
            AutomationError::AuthorizationDenied(_) => ErrorCategory::Permission,
            AutomationError::AuditUnavailable(_) | AutomationError::ChannelUnreachable { .. } => {
                ErrorCategory::ResourceBusy
            },
            AutomationError::DispatchDegraded(_) => ErrorCategory::Network,
            AutomationError::ExecutionError(_)
            | AutomationError::SchedulerError(_)
            | AutomationError::SerializationError(_)
            | AutomationError::InternalError(_) => ErrorCategory::Internal,
        }
    }

    fn suggestion(&self) -> Option<String> {
        match self {
            AutomationError::ConfigError(_) | AutomationError::InvalidConfig(_) => Some(
                "Check aether-automation configuration in config/automation/ and run 'aether sync'"
                    .to_string(),
            ),
            AutomationError::MissingConfig(_) => Some(
                "Add the missing configuration to config/automation/ and run 'aether sync'"
                    .to_string(),
            ),
            AutomationError::DatabaseError(_) => Some(
                "Run 'aether doctor' to check database status. Try 'aether init' if database is missing"
                    .to_string(),
            ),
            AutomationError::InstanceNotFound(_) => Some(
                "Use GET /api/instances to list available instances, or create a new one with POST /api/instances"
                    .to_string(),
            ),
            AutomationError::InstanceExists(_) => Some(
                "Instance already exists. Use PUT /api/instances/{id} to update, or choose a different ID"
                    .to_string(),
            ),
            AutomationError::RuleNotFound(_) => Some(
                "Use GET /api/rules to list available rules, or create a new one with POST /api/rules"
                    .to_string(),
            ),
            AutomationError::RuleExists(_) => Some(
                "Rule already exists. Use PUT /api/rules/{id} to update, or choose a different ID"
                    .to_string(),
            ),
            AutomationError::InvalidRule(_) | AutomationError::ParseError(_) => Some(
                "Check rule syntax. See docs/API_REFERENCE.md for rule format documentation"
                    .to_string(),
            ),
            AutomationError::InvalidRouting(_) => Some(
                "Verify routing configuration. Check that source and target channels/instances exist"
                    .to_string(),
            ),
            AutomationError::RoutingConflict(_) => Some(
                "Reload the current logical-routing revision and review the newer topology before submitting a fresh command"
                    .to_string(),
            ),
            AutomationError::ConfigurationConflict(_) => Some(
                "Reload the current instances revision and review routed descendants before submitting a fresh command"
                    .to_string(),
            ),
            AutomationError::AuthorizationDenied(_) => Some(
                "Use a signed Admin/Engineer session or the local aether CLI to issue device commands"
                    .to_string(),
            ),
            AutomationError::AuditUnavailable(_) => Some(
                "Check the local automation SQLite database; this request was not executed and may be submitted after audit persistence recovers"
                    .to_string(),
            ),
            AutomationError::ExecutionError(_) => Some(
                "Check rule conditions and actions. Use 'aether rules execute <id>' and inspect \
                 the local rule_history table for execution_path and error details"
                    .to_string(),
            ),
            AutomationError::SchedulerError(_) => {
                Some("Check scheduler status with GET /api/scheduler/status".to_string())
            },
            _ => None,
        }
    }
}

impl From<AutomationError> for common::AppError {
    fn from(error: AutomationError) -> Self {
        common::app_error_from(&error)
    }
}

impl IntoResponse for AutomationError {
    fn into_response(self) -> axum::response::Response {
        common::AppError::from(self).into_response()
    }
}
