//! Governed batch point operations.

use std::time::Instant;

use axum::{
    Extension,
    extract::{Path, Query, State},
    http::HeaderMap,
    response::Json,
};

use crate::api::routes::AppState;
use crate::dto::{AppError, SuccessResponse};
use crate::point_topology::{
    PointDefinitionMutation, PointKind, PointMutation, PointPatchMutation, PointTopologyMutation,
    PointTopologyMutationResult,
};

use super::point_governance::{PointTopologyHttpBoundary, completion_audit};
use super::point_helpers::trigger_channel_reload_if_needed;
use super::point_types::*;

/// Applies a mixed point batch under one channel revision CAS and transaction.
#[utoipa::path(
    post,
    path = "/api/channels/{channel_id}/points/batch",
    params(
        ("channel_id" = u32, Path, description = "Channel identifier"),
        ("auto_reload" = bool, Query, description = "Reconcile after commit")
    ),
    request_body(content = PointBatchRequest),
    responses(
        (status = 200, description = "Batch accepted", body = PointBatchResult),
        (status = 400, description = "Invalid batch"),
        (status = 409, description = "Stale expected revision")
    ),
    tag = "io"
)]
pub async fn batch_point_operations_handler(
    Path(channel_id): Path<u32>,
    State(state): State<AppState>,
    Query(reload_query): Query<crate::dto::AutoReloadQuery>,
    Extension(boundary): Extension<PointTopologyHttpBoundary>,
    headers: HeaderMap,
    Json(request): Json<PointBatchRequest>,
) -> Result<Json<SuccessResponse<PointBatchResult>>, AppError> {
    let started = Instant::now();
    if request.create.is_empty() && request.update.is_empty() && request.delete.is_empty() {
        return Err(AppError::bad_request(
            "At least one operation (create/update/delete) must be provided",
        ));
    }

    let mut create_stat = OperationStat {
        total: request.create.len(),
        ..OperationStat::default()
    };
    let mut update_stat = OperationStat {
        total: request.update.len(),
        ..OperationStat::default()
    };
    let mut delete_stat = OperationStat {
        total: request.delete.len(),
        ..OperationStat::default()
    };

    let mut mutations =
        Vec::with_capacity(create_stat.total + update_stat.total + delete_stat.total);
    for item in request.delete {
        mutations.push(delete_mutation(item)?);
    }
    for item in request.create {
        mutations.push(create_mutation(item)?);
    }
    for item in request.update {
        mutations.push(update_mutation(item)?);
    }

    let acceptance = boundary
        .mutate(
            &headers,
            PointTopologyMutation::Batch {
                channel_id,
                mutations,
            },
        )
        .await?;
    let request_id = acceptance.request_id().to_string();
    let resulting_revision = acceptance.resulting_revision().get();
    let audit = completion_audit(acceptance.completion_audit());
    let outcomes = match acceptance.into_result() {
        PointTopologyMutationResult::Batch { outcomes } => outcomes,
        PointTopologyMutationResult::Single { .. }
        | PointTopologyMutationResult::Provisioned { .. }
        | PointTopologyMutationResult::MappingsUpdated { .. } => {
            return Err(AppError::internal_error(
                "Point topology application returned an invalid batch receipt",
            ));
        },
    };

    let mut errors = Vec::new();
    for outcome in outcomes {
        let statistic = match outcome.operation {
            "create" => &mut create_stat,
            "update" => &mut update_stat,
            "delete" => &mut delete_stat,
            _ => {
                return Err(AppError::internal_error(
                    "Point topology application returned an invalid operation",
                ));
            },
        };
        if let Some(error) = outcome.error {
            statistic.failed += 1;
            errors.push(PointBatchError {
                operation: outcome.operation.to_string(),
                point_type: outcome.point_type.to_string(),
                point_id: outcome.point_id,
                error,
            });
        } else {
            statistic.succeeded += 1;
        }
    }

    let total_operations = create_stat.total + update_stat.total + delete_stat.total;
    let succeeded = create_stat.succeeded + update_stat.succeeded + delete_stat.succeeded;
    let failed = create_stat.failed + update_stat.failed + delete_stat.failed;
    trigger_channel_reload_if_needed(channel_id, &state, reload_query.auto_reload).await;

    Ok(Json(SuccessResponse::new(PointBatchResult {
        total_operations,
        succeeded,
        failed,
        operation_stats: OperationStats {
            create: create_stat,
            update: update_stat,
            delete: delete_stat,
        },
        errors,
        duration_ms: started.elapsed().as_millis() as u64,
        request_id,
        resulting_revision,
        completion_audit: audit,
        retryable: false,
    })))
}

fn create_mutation(item: PointBatchCreateItem) -> Result<PointMutation, AppError> {
    use crate::core::config::{AdjustmentPoint, ControlPoint, SignalPoint, TelemetryPoint};

    let kind = PointKind::parse(&item.point_type).map_err(AppError::bad_request)?;
    let mapping = protocol_mapping_directive(&item.data)?;
    let reverse = item
        .data
        .get("reverse")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let normal_state = item
        .data
        .get("normal_state")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    let mut data = item.data;
    if let Some(object) = data.as_object_mut() {
        object.insert("point_id".to_string(), serde_json::json!(item.point_id));
    }
    let definition = match kind {
        PointKind::Telemetry => {
            let point: TelemetryPoint = serde_json::from_value(data).map_err(|error| {
                AppError::bad_request(format!("Invalid telemetry point data: {error}"))
            })?;
            definition_from_base(
                point.base,
                point.scale,
                point.offset,
                point.reverse,
                point.data_type,
                normal_state,
                None,
                None,
                1.0,
                mapping,
            )
        },
        PointKind::Signal => {
            let point: SignalPoint = serde_json::from_value(data).map_err(|error| {
                AppError::bad_request(format!("Invalid signal point data: {error}"))
            })?;
            definition_from_base(
                point.base,
                1.0,
                0.0,
                point.reverse,
                "bool".to_string(),
                normal_state,
                None,
                None,
                1.0,
                mapping,
            )
        },
        PointKind::Control => {
            let point: ControlPoint = serde_json::from_value(data).map_err(|error| {
                AppError::bad_request(format!("Invalid control point data: {error}"))
            })?;
            definition_from_base(
                point.base,
                1.0,
                0.0,
                point.reverse,
                "bool".to_string(),
                normal_state,
                None,
                None,
                1.0,
                mapping,
            )
        },
        PointKind::Adjustment => {
            let point: AdjustmentPoint = serde_json::from_value(data).map_err(|error| {
                AppError::bad_request(format!("Invalid adjustment point data: {error}"))
            })?;
            definition_from_base(
                point.base,
                point.scale,
                point.offset,
                reverse,
                point.data_type,
                normal_state,
                point.min_value,
                point.max_value,
                point.step,
                mapping,
            )
        },
    };
    Ok(PointMutation::Create {
        kind,
        definition,
        force: item.force,
    })
}

#[allow(clippy::too_many_arguments)]
fn definition_from_base(
    base: crate::core::config::Point,
    scale: f64,
    offset: f64,
    reverse: bool,
    data_type: String,
    normal_state: i64,
    minimum: Option<f64>,
    maximum: Option<f64>,
    step: f64,
    protocol_mapping: Option<Option<String>>,
) -> PointDefinitionMutation {
    PointDefinitionMutation {
        point_id: base.point_id,
        signal_name: base.signal_name,
        scale,
        offset,
        unit: base.unit.unwrap_or_default(),
        reverse,
        data_type,
        description: base.description.unwrap_or_default(),
        normal_state,
        minimum,
        maximum,
        step,
        protocol_mapping,
    }
}

fn protocol_mapping_directive(
    data: &serde_json::Value,
) -> Result<Option<Option<String>>, AppError> {
    let Some(value) = data.get("protocol_mapping") else {
        return Ok(None);
    };
    if value.is_null() || value.as_object().is_some_and(serde_json::Map::is_empty) {
        return Ok(Some(None));
    }
    serde_json::to_string(value)
        .map(|value| Some(Some(value)))
        .map_err(|error| AppError::bad_request(format!("Invalid protocol_mapping: {error}")))
}

fn update_mutation(item: PointBatchUpdateItem) -> Result<PointMutation, AppError> {
    Ok(PointMutation::Update {
        kind: PointKind::parse(&item.point_type).map_err(AppError::bad_request)?,
        point_id: item.point_id,
        patch: PointPatchMutation {
            signal_name: item.data.signal_name,
            description: item.data.description,
            unit: item.data.unit,
            scale: item.data.scale,
            offset: item.data.offset,
            data_type: item.data.data_type,
            reverse: item.data.reverse,
            minimum: item.data.min_value,
            maximum: item.data.max_value,
            step: item.data.step,
        },
    })
}

fn delete_mutation(item: PointBatchDeleteItem) -> Result<PointMutation, AppError> {
    Ok(PointMutation::Delete {
        kind: PointKind::parse(&item.point_type).map_err(AppError::bad_request)?,
        point_id: item.point_id,
    })
}
