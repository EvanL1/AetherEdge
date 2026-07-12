//! API Route Configuration
//!
//! Central route definition for all Model Service API endpoints

use axum::{
    Router,
    extract::DefaultBodyLimit,
    routing::{get, post},
};
use std::sync::Arc;

#[cfg(feature = "swagger-ui")]
use utoipa::OpenApi;

use crate::app_state::AppState;

// Import handlers from api module
use crate::api::cloud_sync::export_instances;
use crate::api::health_handlers::health_check;
use crate::api::product_handlers::{get_product_points, list_products};

use crate::api::instance_management_handlers::{
    create_instance, delete_instance, execute_instance_action, reload_instances_from_db,
    update_instance,
};
use crate::api::instance_query_handlers::{
    get_instance, get_instance_children, get_instance_data, get_instance_points, get_topology_tree,
    list_instances, list_instances_slim, search_instances,
};

// New global routing handlers (work with unified database)
use crate::api::global_routing_handlers::{
    delete_all_routing_handler, delete_channel_routing_handler,
    delete_instance_routing_handler as global_delete_instance_routing, get_all_routing_handler,
    get_routing_by_channel_handler,
};
// Refactored routing handlers (work with unified database)
use crate::api::routing_management_handlers::{
    create_instance_routing, delete_instance_routing, update_instance_routing,
    validate_instance_routing,
};
use crate::api::routing_query_handlers::get_instance_routing_handler;

use crate::api::single_point_handlers::{
    delete_action_routing, delete_measurement_routing, get_action_point, get_measurement_point,
    toggle_action_routing, toggle_measurement_routing, upsert_action_routing,
    upsert_measurement_routing,
};

use crate::api::property_handlers::{delete_property, upsert_property};

use common::admin_api::{get_log_level, list_log_files, set_log_level, view_log_file};

// OpenAPI documentation - only compiled when swagger-ui feature is enabled
#[cfg(feature = "swagger-ui")]
#[derive(OpenApi)]
#[openapi(
    paths(
        crate::api::health_handlers::health_check,
        crate::api::instance_query_handlers::list_instances,
        crate::api::instance_query_handlers::list_instances_slim,
        crate::api::instance_query_handlers::search_instances,
        crate::api::instance_management_handlers::create_instance,
        crate::api::instance_query_handlers::get_instance,
        crate::api::instance_management_handlers::update_instance,
        crate::api::instance_management_handlers::delete_instance,
        crate::api::instance_query_handlers::get_instance_data,
        crate::api::instance_query_handlers::get_instance_points,
        crate::api::instance_query_handlers::get_instance_children,
        crate::api::instance_query_handlers::get_topology_tree,
        crate::api::instance_management_handlers::reload_instances_from_db,
        crate::api::instance_management_handlers::execute_instance_action,
        // Instance-level routing handlers (refactored for unified database)
        crate::api::routing_query_handlers::get_instance_routing_handler,
        crate::api::routing_management_handlers::create_instance_routing,
        crate::api::routing_management_handlers::update_instance_routing,
        crate::api::routing_management_handlers::delete_instance_routing,
        crate::api::routing_management_handlers::validate_instance_routing,
        // Single point routing handlers
        crate::api::single_point_handlers::get_measurement_point,
        crate::api::single_point_handlers::upsert_measurement_routing,
        crate::api::single_point_handlers::delete_measurement_routing,
        crate::api::single_point_handlers::toggle_measurement_routing,
        crate::api::single_point_handlers::get_action_point,
        crate::api::single_point_handlers::upsert_action_routing,
        crate::api::single_point_handlers::delete_action_routing,
        crate::api::single_point_handlers::toggle_action_routing,
        // Single property handlers
        crate::api::property_handlers::upsert_property,
        crate::api::property_handlers::delete_property,
        // Global routing handlers (unified database)
        crate::api::global_routing_handlers::get_all_routing_handler,
        crate::api::global_routing_handlers::delete_all_routing_handler,
        crate::api::global_routing_handlers::get_routing_by_channel_handler,
        crate::api::global_routing_handlers::delete_instance_routing_handler,
        crate::api::global_routing_handlers::delete_channel_routing_handler,
        crate::api::product_handlers::list_products,
        crate::api::product_handlers::get_product_points,
        // Cloud sync endpoints
        crate::api::cloud_sync::export_instances,
        // Admin endpoints
        common::admin_api::set_log_level,
        common::admin_api::get_log_level,
        common::admin_api::list_log_files,
        common::admin_api::view_log_file
    ),
    components(
        schemas(
            crate::dto::CreateInstanceDto,
            crate::dto::UpdateInstanceDto,
            crate::dto::ActionRequest,
            crate::dto::RoutingRequest,
            crate::dto::SinglePointRoutingRequest,
            crate::dto::ToggleRoutingRequest,
            crate::dto::RoutingUpdate,
            crate::dto::RoutingType,
            crate::config::Product,
            crate::config::MeasurementPoint,
            crate::config::ActionPoint,
            crate::config::PropertyTemplate,
            // Admin schemas
            common::admin_api::SetLogLevelRequest,
            common::admin_api::LogLevelResponse
        )
    ),
    tags(
        (name = "automation", description = "Instance, topology, routing, and action orchestration"),
        (name = "instances", description = "Instance import and export"),
        (name = "products", description = "Product template management (read-only)"),
        (name = "admin", description = "Administration and service management")
    ),
    modifiers(&SecurityAddon),
    info(
        title = "Aether Automation Service API",
        version = env!("CARGO_PKG_VERSION"),
        description = "Internal loopback API for instances, rules, routing, and device-action dispatch. Device actions require either a Bearer JWT or an AetherService credential; use an authenticated ingress or an on-device commissioning workflow for remote operations."
    )
)]
pub struct AutomationApiDoc;

#[cfg(feature = "swagger-ui")]
struct SecurityAddon;

#[cfg(feature = "swagger-ui")]
impl utoipa::Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                "bearer_auth",
                utoipa::openapi::security::SecurityScheme::Http(
                    utoipa::openapi::security::HttpBuilder::new()
                        .scheme(utoipa::openapi::security::HttpAuthScheme::Bearer)
                        .bearer_format("JWT")
                        .description(Some("Signed Aether access token"))
                        .build(),
                ),
            );
            components.add_security_scheme(
                "aether_service_auth",
                utoipa::openapi::security::SecurityScheme::ApiKey(
                    utoipa::openapi::security::ApiKey::Header(
                        utoipa::openapi::security::ApiKeyValue::with_description(
                            "Authorization",
                            "Dedicated uplink credential. Enter the complete value: AetherService <token>",
                        ),
                    ),
                ),
            );
        }
    }
}

/// Create all API routes for the Model Service
pub fn create_routes(state: Arc<AppState>) -> Router {
    Router::new()
        // Health check
        .route("/health", get(health_check))
        // Instance management API
        .route("/api/instances", get(list_instances).post(create_instance))
        .route("/api/instances/list", get(list_instances_slim))
        .route("/api/instances/search", get(search_instances))
        .route(
            "/api/instances/{id}",
            get(get_instance)
                .put(update_instance)
                .delete(delete_instance),
        )
        .route("/api/instances/{id}/data", get(get_instance_data))
        .route("/api/instances/{id}/points", get(get_instance_points))
        .route("/api/instances/{id}/action", post(execute_instance_action))
        .route("/api/instances/{id}/children", get(get_instance_children))
        // Topology tree endpoint
        .route("/api/topology", get(get_topology_tree))
        .route("/api/instances/reload", post(reload_instances_from_db))

        // Instance-level routing endpoints (refactored for unified database)
        .route(
            "/api/instances/{id}/routing",
            get(get_instance_routing_handler)
                .post(create_instance_routing)
                .put(update_instance_routing)
                .delete(delete_instance_routing),
        )
        .route(
            "/api/instances/{id}/routing/validate",
            post(validate_instance_routing),
        )
        // Single point routing endpoints
        .route(
            "/api/instances/{id}/measurements/{point_id}",
            get(get_measurement_point),
        )
        .route(
            "/api/instances/{id}/measurements/{point_id}/routing",
            axum::routing::put(upsert_measurement_routing)
                .delete(delete_measurement_routing)
                .patch(toggle_measurement_routing),
        )
        .route(
            "/api/instances/{id}/actions/{point_id}",
            get(get_action_point),
        )
        .route(
            "/api/instances/{id}/actions/{point_id}/routing",
            axum::routing::put(upsert_action_routing)
                .delete(delete_action_routing)
                .patch(toggle_action_routing),
        )

        // Single property value endpoints (instance_properties table)
        .route(
            "/api/instances/{id}/properties/{property_id}",
            axum::routing::put(upsert_property).delete(delete_property),
        )

        // Global routing management endpoints (new unified database APIs)
        .route("/api/routing", get(get_all_routing_handler).delete(delete_all_routing_handler))
        .route("/api/routing/by-channel/{channel_id}", get(get_routing_by_channel_handler))
        .route(
            "/api/routing/instances/{instance_name}",
            axum::routing::delete(global_delete_instance_routing),
        )
        .route("/api/routing/channels/{channel_id}", axum::routing::delete(delete_channel_routing_handler))

        // Product management endpoints (read-only)
        .route("/api/products", get(list_products))
        .route("/api/products/{product_name}/points", get(get_product_points))
        // Cloud sync endpoints
        .route("/api/instances/export", get(export_instances))
        // Admin endpoints (log level + file access)
        .route(
            "/api/admin/logs/level",
            get(get_log_level).post(set_log_level),
        )
        .route("/api/admin/logs/files", get(list_log_files))
        .route("/api/admin/logs/view", get(view_log_file))
        // Apply HTTP request logging middleware
        .layer(axum::middleware::from_fn(common::logging::http_request_logger))
        .layer(DefaultBodyLimit::max(1024 * 1024)) // 1 MB request body limit
        .with_state(state)
}

#[cfg(all(test, feature = "swagger-ui"))]
mod openapi_tests {
    use super::*;
    use crate::rule_routes::RuleApiDoc;

    fn document() -> serde_json::Value {
        let openapi = AutomationApiDoc::openapi().nest("", RuleApiDoc::openapi());
        serde_json::to_value(openapi).expect("OpenAPI document should serialize")
    }

    #[test]
    fn openapi_metadata_and_security_match_the_automation_service() {
        let document = document();

        assert_eq!(
            document
                .pointer("/info/title")
                .and_then(|value| value.as_str()),
            Some("Aether Automation Service API")
        );
        assert_eq!(
            document
                .pointer("/info/version")
                .and_then(|value| value.as_str()),
            Some(env!("CARGO_PKG_VERSION"))
        );
        let description = document
            .pointer("/info/description")
            .and_then(|value| value.as_str())
            .expect("OpenAPI description should be present");
        assert!(description.contains("Internal loopback API"));
        assert!(description.contains("Bearer JWT or an AetherService credential"));
        assert!(description.contains("authenticated ingress"));

        let bearer_auth = document
            .pointer("/components/securitySchemes/bearer_auth")
            .expect("bearer security scheme should be present");
        assert_eq!(bearer_auth["type"], "http");
        assert_eq!(bearer_auth["scheme"], "bearer");

        let service_auth = document
            .pointer("/components/securitySchemes/aether_service_auth")
            .expect("service security scheme should be present");
        assert_eq!(service_auth["type"], "apiKey");
        assert_eq!(service_auth["in"], "header");
        assert_eq!(service_auth["name"], "Authorization");
        assert!(
            document
                .pointer("/components/securitySchemes/aether_service_auth/description")
                .and_then(|value| value.as_str())
                .is_some_and(|value| value.contains("AetherService <token>"))
        );

        let tags = document
            .pointer("/tags")
            .and_then(|value| value.as_array())
            .expect("OpenAPI tags should be an array");
        assert!(tags.iter().any(|tag| tag["name"] == "automation"));
        assert!(tags.iter().any(|tag| tag["name"] == "instances"));
    }

    #[test]
    fn openapi_examples_are_industry_neutral() {
        let serialized = serde_json::to_string(&document())
            .expect("Automation OpenAPI document should serialize")
            .to_ascii_lowercase();

        for energy_pack_identity in [
            "pv inverter",
            "pv_inverter",
            "battery soc",
            "battery_",
            "\"battery\"",
            "\"ess\"",
            "\"ess_",
            "\"pcs\"",
            "pcs_",
            "pcs#",
            "diesel",
            "\"soc\"",
            "soc_",
        ] {
            assert!(
                !serialized.contains(energy_pack_identity),
                "Kernel Swagger must not embed Energy Pack identity {energy_pack_identity}"
            );
        }
    }

    #[test]
    fn openapi_paths_match_the_registered_router() {
        let document = document();
        let paths = document
            .pointer("/paths")
            .and_then(|value| value.as_object())
            .expect("OpenAPI paths should be an object");

        for path in [
            "/health",
            "/api/instances/{id}/children",
            "/api/topology",
            "/api/instances/reload",
            "/api/admin/logs/files",
            "/api/admin/logs/view",
            "/api/routing/instances/{instance_name}",
            "/api/rules/{id}/variables",
        ] {
            assert!(paths.contains_key(path), "missing OpenAPI path: {path}");
        }

        assert!(!paths.contains_key("/api/instances/{id}/sync"));
        assert!(!paths.contains_key("/api/instances/sync/all"));
        assert!(!paths.contains_key("/api/routing/instances/{id}"));

        let operation_count = paths
            .values()
            .filter_map(|path| path.as_object())
            .flat_map(|path| path.keys())
            .filter(|method| {
                matches!(
                    method.as_str(),
                    "get" | "put" | "post" | "delete" | "options" | "head" | "patch" | "trace"
                )
            })
            .count();
        assert_eq!(
            operation_count, 52,
            "OpenAPI operation count changed; re-audit Router parity before updating this contract"
        );
    }

    #[test]
    fn openapi_instance_and_channel_ids_match_u32_handlers() {
        let document = document();
        let paths = document["paths"]
            .as_object()
            .expect("OpenAPI paths should be an object");

        for (path, item) in paths {
            let Some(operations) = item.as_object() else {
                continue;
            };
            for operation in operations.values().filter(|value| value.is_object()) {
                let Some(parameters) = operation["parameters"].as_array() else {
                    continue;
                };
                for parameter in parameters {
                    let name = parameter["name"].as_str().unwrap_or_default();
                    let is_instance_id = name == "id"
                        && (path.starts_with("/api/instances/")
                            || path.starts_with("/api/routing/instances/"));
                    let is_channel_id = name == "channel_id";
                    if is_instance_id || is_channel_id {
                        assert_eq!(
                            parameter["schema"]["format"], "int32",
                            "path ID type drift on {path}: {parameter}"
                        );
                        assert_eq!(parameter["schema"]["minimum"], 0);
                        assert!(
                            parameter["schema"].get("maximum").is_none(),
                            "u32 path IDs must not be capped at the legacy u16 maximum on {path}: {parameter}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn action_openapi_contract_declares_auth_and_failure_responses() {
        let document = document();
        let operation = document
            .pointer("/paths/~1api~1instances~1{id}~1action/post")
            .expect("action POST operation should be documented");

        for status in ["403", "422", "503"] {
            assert!(
                operation.pointer(&format!("/responses/{status}")).is_some(),
                "missing action response: {status}"
            );
        }

        let security = operation
            .pointer("/security")
            .and_then(|value| value.as_array())
            .expect("action security should be an array");
        assert!(
            security
                .iter()
                .any(|entry| entry.get("bearer_auth").is_some())
        );
        assert!(
            security
                .iter()
                .any(|entry| entry.get("aether_service_auth").is_some())
        );

        let request_schema = operation
            .pointer("/requestBody/content/application~1json/schema/$ref")
            .and_then(|value| value.as_str())
            .expect("action request schema should be referenced");
        assert!(request_schema.ends_with("/ActionRequest"));
        assert!(
            operation["parameters"]
                .as_array()
                .is_some_and(|parameters| parameters.iter().any(|parameter| {
                    parameter["name"] == "x-request-id" && parameter["in"] == "header"
                })),
            "device action must document its optional audit correlation header"
        );
        assert_eq!(
            document
                .pointer("/components/schemas/ActionRequest/properties/confirmed/type")
                .and_then(|value| value.as_str()),
            Some("boolean")
        );
        assert!(
            document
                .pointer("/components/schemas/ActionRequest/required")
                .and_then(|value| value.as_array())
                .is_some_and(|required| required.iter().any(|field| field == "confirmed")),
            "high-risk action confirmation must be required in Swagger"
        );

        let success_description = operation
            .pointer("/responses/200/description")
            .and_then(|value| value.as_str())
            .expect("action success semantics should be documented");
        assert!(success_description.contains("accepted by the local command plane"));
        assert!(success_description.contains("not a device completion time"));
        assert!(success_description.contains("audit.status=incomplete"));
        assert!(success_description.contains("retryable=false"));
        let lower = success_description.to_ascii_lowercase();
        assert!(!lower.contains("action executed"));
        assert!(!lower.contains("delivered to device"));
    }

    #[test]
    fn manual_rule_execution_openapi_matches_the_governed_application_command() {
        let document = document();
        let operation = document
            .pointer("/paths/~1api~1rules~1{id}~1execute/post")
            .expect("manual rule execution should be documented");

        for status in ["200", "403", "422", "503"] {
            assert!(
                operation.pointer(&format!("/responses/{status}")).is_some(),
                "missing manual rule response: {status}"
            );
        }

        let security = operation
            .pointer("/security")
            .and_then(|value| value.as_array())
            .expect("manual rule security should be an array");
        assert_eq!(security.len(), 1, "manual rules accept only access JWTs");
        assert!(security[0].get("bearer_auth").is_some());
        assert!(security[0].get("aether_service_auth").is_none());

        let request_schema = operation
            .pointer("/requestBody/content/application~1json/schema/$ref")
            .and_then(|value| value.as_str())
            .expect("manual rule request schema should be referenced");
        assert!(request_schema.ends_with("/ExecuteRuleRequest"));
        assert!(
            operation["parameters"]
                .as_array()
                .is_some_and(|parameters| parameters.iter().any(|parameter| {
                    parameter["name"] == "x-request-id" && parameter["in"] == "header"
                })),
            "manual rule execution must document its optional audit correlation header"
        );
        let confirmed = document
            .pointer("/components/schemas/ExecuteRuleRequest/properties/confirmed/type")
            .and_then(|value| value.as_str());
        assert_eq!(confirmed, Some("boolean"));
        assert!(
            document
                .pointer("/components/schemas/ExecuteRuleRequest/required")
                .and_then(|value| value.as_array())
                .is_some_and(|required| required.iter().any(|field| field == "confirmed"))
        );
        let success_description = operation
            .pointer("/responses/200/description")
            .and_then(|value| value.as_str())
            .expect("manual rule success semantics should be documented");
        assert!(success_description.contains("audit.status=incomplete"));
        assert!(success_description.contains("retryable=false"));
        assert!(success_description.contains("must not be retried"));
    }

    #[test]
    fn rule_mutation_openapi_requires_bearer_confirmation_and_fail_closed_audit() {
        let document = document();
        for (pointer, schema) in [
            ("/paths/~1api~1rules/post", "CreateRuleRequest"),
            ("/paths/~1api~1rules~1{id}/put", "UpdateRuleRequest"),
            ("/paths/~1api~1rules~1{id}/delete", "RuleMutationRequest"),
            (
                "/paths/~1api~1rules~1{id}~1enable/post",
                "RuleMutationRequest",
            ),
            (
                "/paths/~1api~1rules~1{id}~1disable/post",
                "RuleMutationRequest",
            ),
            (
                "/paths/~1api~1scheduler~1reload/post",
                "RuleMutationRequest",
            ),
        ] {
            let operation = document
                .pointer(pointer)
                .unwrap_or_else(|| panic!("missing governed mutation operation {pointer}"));
            for status in ["200", "403", "422", "503"] {
                assert!(
                    operation.pointer(&format!("/responses/{status}")).is_some(),
                    "{pointer} is missing response {status}"
                );
            }
            let security = operation["security"]
                .as_array()
                .expect("mutation security should be an array");
            assert_eq!(security.len(), 1, "{pointer} must accept only bearer JWTs");
            assert!(security[0].get("bearer_auth").is_some());
            assert!(security[0].get("aether_service_auth").is_none());
            let request_schema = operation
                .pointer("/requestBody/content/application~1json/schema/$ref")
                .and_then(|value| value.as_str())
                .expect("mutation request schema should be referenced");
            assert!(request_schema.ends_with(schema));
            assert!(
                operation["parameters"]
                    .as_array()
                    .is_some_and(|parameters| parameters.iter().any(|parameter| {
                        parameter["name"] == "x-request-id" && parameter["in"] == "header"
                    })),
                "{pointer} must document its optional audit correlation header"
            );
            assert!(
                document
                    .pointer(&format!("/components/schemas/{schema}/required"))
                    .and_then(|value| value.as_array())
                    .is_some_and(|required| required.iter().any(|field| field == "confirmed")),
                "{schema}.confirmed must be required"
            );
            assert!(
                operation["responses"]["200"]["description"]
                    .as_str()
                    .is_some_and(|description| {
                        description.contains("non-retryable")
                            && description.contains("scheduler-refresh")
                    })
            );
        }
    }

    #[test]
    fn action_routing_openapi_matches_the_governed_application_command() {
        let document = document();

        for (method, schema) in [
            ("put", "ActionRoutingUpsertBody"),
            ("delete", "ActionRoutingConfirmationBody"),
            ("patch", "ActionRoutingToggleBody"),
        ] {
            let pointer =
                format!("/paths/~1api~1instances~1{{id}}~1actions~1{{point_id}}~1routing/{method}");
            let operation = document
                .pointer(&pointer)
                .unwrap_or_else(|| panic!("missing action-routing operation {pointer}"));

            for status in ["200", "403", "422", "503"] {
                assert!(
                    operation.pointer(&format!("/responses/{status}")).is_some(),
                    "{pointer} is missing response {status}"
                );
            }

            let security = operation["security"]
                .as_array()
                .expect("action-routing security should be an array");
            assert_eq!(security.len(), 1, "{pointer} must accept only bearer JWTs");
            assert!(security[0].get("bearer_auth").is_some());
            assert!(security[0].get("aether_service_auth").is_none());

            assert!(
                operation["parameters"]
                    .as_array()
                    .is_some_and(|parameters| parameters.iter().any(|parameter| {
                        parameter["name"] == "x-request-id" && parameter["in"] == "header"
                    })),
                "{pointer} must document its optional audit correlation header"
            );

            let request_body = &operation["requestBody"];
            assert_eq!(
                request_body["required"], true,
                "{pointer} must require its confirmation body"
            );
            assert!(
                request_body["description"]
                    .as_str()
                    .is_some_and(|description| description.contains("confirmed=true")),
                "{pointer} must explain the explicit confirmation value"
            );
            let request_schema = request_body
                .pointer("/content/application~1json/schema/$ref")
                .and_then(|value| value.as_str())
                .expect("action-routing request schema should be referenced");
            assert!(request_schema.ends_with(schema));
            assert_eq!(
                document
                    .pointer(&format!(
                        "/components/schemas/{schema}/properties/confirmed/type"
                    ))
                    .and_then(|value| value.as_str()),
                Some("boolean")
            );
            assert!(
                document
                    .pointer(&format!("/components/schemas/{schema}/required"))
                    .and_then(|value| value.as_array())
                    .is_some_and(|required| required.iter().any(|field| field == "confirmed")),
                "{schema}.confirmed must be required in Swagger"
            );
            if method == "put" {
                let action_point_kinds: Vec<_> = document
                    .pointer("/components/schemas/ActionRoutingFourRemote/enum")
                    .and_then(|value| value.as_array())
                    .expect("action-routing four_remote enum should be documented")
                    .iter()
                    .filter_map(|value| value.as_str())
                    .collect();
                assert_eq!(action_point_kinds, ["C", "A"]);
                assert!(!action_point_kinds.contains(&"T"));
                assert!(!action_point_kinds.contains(&"S"));
            }

            let success = &operation["responses"]["200"];
            assert!(
                success["description"].as_str().is_some_and(|description| {
                    description.contains("audit.status=incomplete")
                        && description.contains("retryable=false")
                        && description.contains("must not be retried")
                }),
                "{pointer} must document non-retryable terminal-audit degradation"
            );
            let success_schema = success
                .pointer("/content/application~1json/schema/$ref")
                .and_then(|value| value.as_str())
                .expect("action-routing success schema should be referenced");
            assert!(success_schema.ends_with("/ActionRoutingMutationResponse"));
            for field in [
                "request_id",
                "operation",
                "affected_routes",
                "audit",
                "retryable",
            ] {
                assert!(
                    document
                        .pointer(&format!(
                            "/components/schemas/ActionRoutingMutationData/properties/{field}"
                        ))
                        .is_some(),
                    "{pointer} success schema must expose data.{field}"
                );
            }
        }
    }
}
