#![allow(clippy::disallowed_methods)]

//! Shared data structures for point CRUD and batch operations

/// Point CRUD operation result
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct PointCrudResult {
    #[schema(example = 1)]
    pub channel_id: u32,

    /// Point type: T (Telemetry), S (Signal), C (Control), A (Adjustment)
    #[schema(example = "T")]
    pub point_type: String,

    #[schema(example = 101)]
    pub point_id: u32,

    #[schema(example = "DC_Voltage")]
    pub signal_name: String,

    #[schema(example = "Point updated successfully")]
    pub message: String,
}

// ============================================================================
// Batch Point CRUD Data Structures
// ============================================================================

/// Batch point operations request
///
/// Supports creating, updating, and deleting multiple points in a single request.
/// All operations are optional - provide only the operations you need.
#[derive(Debug, serde::Deserialize, utoipa::ToSchema)]
pub struct PointBatchRequest {
    /// Points to create
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub create: Vec<PointBatchCreateItem>,

    /// Points to update
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub update: Vec<PointBatchUpdateItem>,

    /// Points to delete
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delete: Vec<PointBatchDeleteItem>,
}

/// Batch create operation item
#[derive(Debug, serde::Deserialize, utoipa::ToSchema)]
pub struct PointBatchCreateItem {
    /// Point type: T (Telemetry), S (Signal), C (Control), A (Adjustment)
    #[schema(example = "T")]
    pub point_type: String,

    /// Point identifier
    #[schema(example = 101)]
    pub point_id: u32,

    /// Force create mode: if true, use INSERT OR REPLACE (upsert), if false, fail on duplicate (default: false)
    #[serde(default)]
    #[schema(example = false)]
    pub force: bool,

    /// Point configuration data (structure varies by point type)
    pub data: serde_json::Value,
}

/// Batch update operation item
#[derive(Debug, serde::Deserialize, utoipa::ToSchema)]
pub struct PointBatchUpdateItem {
    /// Point type: T (Telemetry), S (Signal), C (Control), A (Adjustment)
    #[schema(example = "T")]
    pub point_type: String,

    /// Point identifier
    #[schema(example = 101)]
    pub point_id: u32,

    /// Fields to update (only provide fields you want to update)
    /// Same structure as PointUpdateRequest, wrapped in "data" for consistency with CREATE
    pub data: PointUpdateRequest,
}

/// Batch delete operation item
#[derive(Debug, serde::Deserialize, utoipa::ToSchema)]
pub struct PointBatchDeleteItem {
    /// Point type: T (Telemetry), S (Signal), C (Control), A (Adjustment)
    #[schema(example = "T")]
    pub point_type: String,

    /// Point identifier
    #[schema(example = 101)]
    pub point_id: u32,
}

/// Batch operation result
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct PointBatchResult {
    /// Total number of operations requested
    #[schema(example = 10)]
    pub total_operations: usize,

    /// Number of successful operations
    #[schema(example = 8)]
    pub succeeded: usize,

    /// Number of failed operations
    #[schema(example = 2)]
    pub failed: usize,

    /// Statistics per operation type
    pub operation_stats: OperationStats,

    /// Details of failed operations
    pub errors: Vec<PointBatchError>,

    /// Processing duration in milliseconds
    #[schema(example = 250)]
    pub duration_ms: u64,
}

/// Operation statistics grouped by type
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct OperationStats {
    pub create: OperationStat,
    pub update: OperationStat,
    pub delete: OperationStat,
}

/// Statistics for a single operation type
#[derive(Debug, Default, serde::Serialize, utoipa::ToSchema)]
pub struct OperationStat {
    #[schema(example = 5)]
    pub total: usize,
    #[schema(example = 4)]
    pub succeeded: usize,
    #[schema(example = 1)]
    pub failed: usize,
}

/// Error details for a failed batch operation
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct PointBatchError {
    /// Operation type: create, update, or delete
    #[schema(example = "create")]
    pub operation: String,

    /// Point type: T, S, C, or A
    #[schema(example = "T")]
    pub point_type: String,

    /// Point identifier
    #[schema(example = 101)]
    pub point_id: u32,

    /// Error message
    #[schema(example = "Point 101 already exists")]
    pub error: String,
}

/// Update request for point fields (supports partial updates)
///
/// Only provide fields you want to update. Fields are type-specific:
/// - **Common**: signal_name, description, unit, reverse
/// - **T/A/C**: scale, offset, data_type
/// - **A only**: min_value, max_value, step
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
pub struct PointUpdateRequest {
    /// Point signal name (all types)
    #[schema(example = "DC_Voltage")]
    pub signal_name: Option<String>,

    /// Point description (all types)
    #[schema(example = "DC bus voltage")]
    pub description: Option<String>,

    /// Measurement unit (all types)
    #[schema(example = "V")]
    pub unit: Option<String>,

    /// Scale factor for raw value conversion (T/A/C)
    #[schema(example = 0.1)]
    pub scale: Option<f64>,

    /// Offset for raw value conversion (T/A/C)
    #[schema(example = 0.0)]
    pub offset: Option<f64>,

    /// Data type: float32, int16, uint16, int32, uint32 (T/A/C)
    #[schema(example = "float32")]
    pub data_type: Option<String>,

    /// Reverse logic (false=normal, true=inverted) (all types)
    #[schema(example = false)]
    pub reverse: Option<bool>,

    /// Inclusive minimum command value (A only)
    pub min_value: Option<f64>,

    /// Inclusive maximum command value (A only)
    pub max_value: Option<f64>,

    /// Allowed command increment (A only, must be finite and positive)
    pub step: Option<f64>,
}
