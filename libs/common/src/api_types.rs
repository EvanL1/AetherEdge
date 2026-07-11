//! Shared API models for AetherEMS services
//!
//! This module provides unified API request/response models and HTTP utilities
//! to ensure consistency across all service endpoints.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[cfg(feature = "openapi")]
use utoipa::ToSchema;

// ============================================================================
// Standard API Response Models
// ============================================================================

/// Standard success response
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct SuccessResponse<T> {
    /// Success indicator (always true)
    #[serde(default = "crate::serde_helpers::bool_true")]
    pub success: bool,
    /// Response data
    pub data: T,
    /// Additional metadata
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, serde_json::Value>,
}

impl<T> SuccessResponse<T> {
    /// Create a new success response
    pub fn new(data: T) -> Self {
        Self {
            success: true,
            data,
            metadata: HashMap::new(),
        }
    }

    /// Add metadata to the response
    pub fn with_metadata(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }
}

/// Standard error response
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct ErrorResponse {
    /// Success indicator (always false for errors)
    #[serde(default = "crate::serde_helpers::bool_false")]
    pub success: bool,
    /// Error information
    pub error: ErrorInfo,
}

/// Standard error information
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct ErrorInfo {
    /// Error code (HTTP status or custom)
    pub code: u16,
    /// Error message
    pub message: String,
    /// Detailed error description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
    /// Suggested action to fix the error
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
    /// Field-specific errors for validation
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub field_errors: HashMap<String, Vec<String>>,
}

impl ErrorInfo {
    /// Create a new error
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            code: 500,
            message: message.into(),
            details: None,
            suggestion: None,
            field_errors: HashMap::new(),
        }
    }

    /// Create with specific code
    pub fn with_code(mut self, code: u16) -> Self {
        self.code = code;
        self
    }

    /// Add details
    pub fn with_details(mut self, details: impl Into<String>) -> Self {
        self.details = Some(details.into());
        self
    }

    /// Add a suggestion for how to fix the error
    pub fn with_suggestion(mut self, suggestion: impl Into<String>) -> Self {
        self.suggestion = Some(suggestion.into());
        self
    }

    /// Add field error
    pub fn add_field_error(mut self, field: impl Into<String>, error: impl Into<String>) -> Self {
        self.field_errors
            .entry(field.into())
            .or_default()
            .push(error.into());
        self
    }
}

// ============================================================================
// AppError - HTTP Error with proper status codes (requires axum feature)
// ============================================================================

#[cfg(feature = "axum")]
use axum::{
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};

/// Application error with HTTP status code
/// This type implements IntoResponse for seamless integration with axum handlers
#[cfg(feature = "axum")]
#[derive(Debug, Clone)]
pub struct AppError {
    /// HTTP status code
    pub status: StatusCode,
    /// Error information
    pub error: ErrorInfo,
}

#[cfg(feature = "axum")]
impl AppError {
    /// Create a new error
    pub fn new(status: StatusCode, error: ErrorInfo) -> Self {
        Self { status, error }
    }

    /// Create a 400 Bad Request error
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            error: ErrorInfo::new(message).with_code(400),
        }
    }

    /// Create a 400 Bad Request error with validation details
    pub fn validation_error(field_errors: HashMap<String, Vec<String>>) -> Self {
        let mut error = ErrorInfo::new("Validation failed").with_code(400);
        error.field_errors = field_errors;
        Self {
            status: StatusCode::BAD_REQUEST,
            error,
        }
    }

    /// Create a 404 Not Found error
    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            error: ErrorInfo::new(message).with_code(404),
        }
    }

    /// Create a 409 Conflict error
    pub fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            error: ErrorInfo::new(message).with_code(409),
        }
    }

    /// Create a 500 Internal Server Error
    pub fn internal_error(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            error: ErrorInfo::new(message).with_code(500),
        }
    }

    /// Create a 503 Service Unavailable error
    pub fn service_unavailable(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            error: ErrorInfo::new(message).with_code(503),
        }
    }

    /// Add details to the error
    pub fn with_details(mut self, details: impl Into<String>) -> Self {
        self.error = self.error.with_details(details);
        self
    }
}

#[cfg(feature = "axum")]
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorResponse {
                success: false,
                error: self.error,
            }),
        )
            .into_response()
    }
}

#[cfg(feature = "axum")]
impl From<anyhow::Error> for AppError {
    fn from(err: anyhow::Error) -> Self {
        Self::internal_error(err.to_string())
    }
}

// ============================================================================
// Pagination Models
// ============================================================================

/// Paginated response wrapper
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct PaginatedResponse<T> {
    /// List of items
    #[serde(rename = "list", alias = "items")]
    pub items: Vec<T>,
    /// Total number of items
    pub total: usize,
    /// Current page (0-indexed)
    pub page: usize,
    /// Items per page
    pub page_size: usize,
    /// Total number of pages
    pub total_pages: usize,
    /// Whether there are more pages
    pub has_next: bool,
    /// Whether there are previous pages
    pub has_previous: bool,
}

impl<T> PaginatedResponse<T> {
    /// Create a new paginated response
    pub fn new(items: Vec<T>, total: usize, page: usize, page_size: usize) -> Self {
        let total_pages = total.div_ceil(page_size);
        Self {
            items,
            total,
            page,
            page_size,
            total_pages,
            has_next: page + 1 < total_pages,
            has_previous: page > 0,
        }
    }

    /// Create paginated response from a slice with 1-indexed page number
    ///
    /// This is a convenience method that handles the common pagination pattern:
    /// - Normalizes page to be at least 1
    /// - Clamps page_size between 1 and 100
    /// - Calculates correct slice boundaries
    /// - Returns empty list if page is out of bounds
    ///
    /// Optimization: Uses into_iter().skip().take() to consume ownership without cloning
    pub fn from_slice(all_items: Vec<T>, page: usize, page_size: usize) -> Self {
        let total = all_items.len();
        let page = page.max(1);
        let page_size = page_size.clamp(1, 100);

        let start_index = (page - 1) * page_size;

        // Optimization: consume ownership instead of cloning (removes T: Clone requirement)
        let items: Vec<T> = if start_index < total {
            all_items
                .into_iter()
                .skip(start_index)
                .take(page_size)
                .collect()
        } else {
            Vec::new()
        };

        // Convert to 0-indexed for internal storage
        Self::new(items, total, page - 1, page_size)
    }
}

/// Pagination request parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct PaginationParams {
    /// Page number (0-indexed)
    #[serde(default)]
    pub page: usize,
    /// Items per page
    #[serde(default = "crate::serde_helpers::page_size")]
    pub page_size: usize,
    /// Sort field
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_by: Option<String>,
    /// Sort order
    #[serde(default)]
    pub sort_order: SortOrder,
}

/// Sort order
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum SortOrder {
    #[default]
    Asc,
    Desc,
}

// ============================================================================
// Time Range Filter
// ============================================================================

/// Time range filter
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeRange {
    /// Start time (ISO 8601)
    pub start: Option<chrono::DateTime<chrono::Utc>>,
    /// End time (ISO 8601)
    pub end: Option<chrono::DateTime<chrono::Utc>>,
}

impl TimeRange {
    /// Create a time range for the last N hours
    pub fn last_hours(hours: i64) -> Self {
        let end = chrono::Utc::now();
        let start = end - chrono::Duration::hours(hours);
        Self {
            start: Some(start),
            end: Some(end),
        }
    }

    /// Create a time range for today
    pub fn today() -> Self {
        let now = chrono::Utc::now();
        let start = now
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .map(|dt| chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(dt, chrono::Utc));
        Self {
            start,
            end: Some(now),
        }
    }
}

// ============================================================================
// Service Health & Status Models
// ============================================================================

/// Service health status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthStatus {
    /// Overall health status
    pub status: ServiceStatus,
    /// Service name
    pub service: String,
    /// Service version
    pub version: String,
    /// Uptime in seconds
    pub uptime_seconds: u64,
    /// Timestamp of this check
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Individual component checks
    #[serde(default)]
    pub checks: HashMap<String, ComponentHealth>,
    /// System resource metrics (CPU, memory)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<serde_json::Value>,
}

/// Service status enum
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum ServiceStatus {
    Healthy,
    Degraded,
    Unhealthy,
    Unknown,
}

/// Component health check result
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct ComponentHealth {
    /// Component status
    pub status: ServiceStatus,
    /// Optional message
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Check duration in milliseconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;

    #[test]
    fn test_success_response_creation() {
        let response = SuccessResponse::new("test data");
        assert_eq!(response.data, "test data");
        assert!(response.metadata.is_empty());

        let response_with_metadata =
            SuccessResponse::new("test").with_metadata("key", serde_json::json!("value"));
        assert_eq!(response_with_metadata.metadata.len(), 1);
    }

    #[test]
    fn test_error_response_creation() {
        let error = ErrorInfo::new("Something went wrong").with_code(500);
        let response = ErrorResponse {
            success: false,
            error,
        };
        assert_eq!(response.error.message, "Something went wrong");
        assert_eq!(response.error.code, 500);
        assert!(!response.success);
    }

    #[test]
    fn test_pagination() {
        let items = vec![1, 2, 3, 4, 5];
        let paginated = PaginatedResponse::new(items, 100, 0, 5);
        assert_eq!(paginated.total_pages, 20);
        assert!(paginated.has_next);
        assert!(!paginated.has_previous);
    }

    #[test]
    fn test_time_range() {
        let range = TimeRange::last_hours(24);
        assert!(range.start.is_some());
        assert!(range.end.is_some());

        if let (Some(start), Some(end)) = (range.start, range.end) {
            assert!(start < end);
        }
    }
}
