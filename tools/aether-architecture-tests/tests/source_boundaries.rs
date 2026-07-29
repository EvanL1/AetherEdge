use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use proc_macro2::{TokenStream, TokenTree};
use serde::Deserialize;
use syn::punctuated::Punctuated;
use syn::visit::{self, Visit};
use syn::{
    Attribute, Expr, ExprPath, Ident, ImplItemFn, Item, ItemEnum, ItemImpl, ItemUse, Lit, Meta,
    Token, UseTree,
};

const AUTOMATION_CONFIGURATION_TABLES: &[&str] = &[
    "measurement_routing",
    "action_routing",
    "rules",
    "instances",
    "instance_properties",
];
const IO_POINT_CONFIGURATION_TABLES: &[&str] = &[
    "telemetry_points",
    "signal_points",
    "control_points",
    "adjustment_points",
];
const LEGACY_ACTION_ROUTING_METHODS: &[&str] = &[
    "upsert_action_routing",
    "delete_action_routing",
    "toggle_action_routing",
    "delete_all_routing",
];
const LEGACY_INSTANCE_MANAGER_MUTATIONS: &[&str] = &[
    "create_instance",
    "rename_instance",
    "delete_instance",
    "collect_descendants",
    "delete_single_instance",
    "upsert_single_property",
    "delete_single_property",
    "upsert_measurement_routing",
    "delete_measurement_routing",
    "toggle_measurement_routing",
    "upsert_action_routing",
    "delete_action_routing",
    "toggle_action_routing",
    "delete_all_routing",
];
const LEGACY_COMMAND_TRANSPORT_SYMBOLS: &[&str] = &[
    "ActionDispatch",
    "ShmDispatch",
    "ActionWriter",
    "ShmNotifier",
];
const LEGACY_PRODUCT_SYMBOLS: &[&str] = &[
    "get_builtin_products",
    "get_builtin_product",
    "get_product_names",
    "get_child_products",
    "builtin_only",
];
const RETIRED_IO_SYMBOLS: &[&str] = &[
    "NetworkConfigUpdateRequest",
    "NetworkInterfaceConfig",
    "ScriptRunner",
    "VirtualAddress",
    "VirtualChannel",
    "VirtualMapping",
    "network_handlers",
    "transform_script",
];

#[derive(Debug, Deserialize)]
struct MetadataDocument {
    packages: Vec<Package>,
    workspace_members: BTreeSet<String>,
    workspace_root: PathBuf,
}

#[derive(Debug, Deserialize)]
struct Package {
    id: String,
    name: String,
    manifest_path: PathBuf,
    targets: Vec<Target>,
}

#[derive(Debug, Deserialize)]
struct Target {
    kind: Vec<String>,
    src_path: PathBuf,
}

struct SourceScanner<'a> {
    workspace_root: &'a Path,
    package: &'a Package,
    is_core_crate: bool,
    visited: HashSet<(PathBuf, Vec<String>)>,
    violations: BTreeSet<String>,
}

impl<'a> SourceScanner<'a> {
    fn new(workspace_root: &'a Path, package: &'a Package) -> Self {
        let relative_manifest = package
            .manifest_path
            .strip_prefix(workspace_root)
            .unwrap_or(&package.manifest_path);
        Self {
            workspace_root,
            package,
            is_core_crate: relative_manifest.starts_with("crates"),
            visited: HashSet::new(),
            violations: BTreeSet::new(),
        }
    }

    fn scan(mut self) -> BTreeSet<String> {
        for target in &self.package.targets {
            if !is_production_target(target) {
                continue;
            }
            let root = &target.src_path;
            let module_directory = root_module_directory(root);
            self.scan_file(root, &module_directory, &[]);
        }
        self.violations
    }

    fn scan_file(&mut self, source: &Path, module_directory: &Path, module_path: &[String]) {
        let key = (source.to_path_buf(), module_path.to_vec());
        if !self.visited.insert(key) {
            return;
        }

        let text = fs::read_to_string(source).unwrap_or_else(|error| {
            panic!("failed to read Rust source {}: {error}", source.display())
        });
        let syntax = syn::parse_file(&text).unwrap_or_else(|error| {
            panic!("failed to parse Rust source {}: {error}", source.display())
        });
        self.scan_items(&syntax.items, source, module_directory, module_path);
    }

    fn scan_items(
        &mut self,
        items: &[Item],
        source: &Path,
        module_directory: &Path,
        module_path: &[String],
    ) {
        for item in items {
            if has_cfg_test(item_attributes(item)) {
                continue;
            }

            if let Item::Mod(module) = item {
                let mut child_module_path = module_path.to_vec();
                child_module_path.push(module.ident.to_string());
                let child_module_directory = module_directory.join(module.ident.to_string());
                if let Some((_, items)) = &module.content {
                    self.scan_items(items, source, &child_module_directory, &child_module_path);
                } else {
                    let module_source = resolve_module_source(
                        source,
                        module_directory,
                        &module.ident,
                        &module.attrs,
                    );
                    self.scan_file(&module_source, &child_module_directory, &child_module_path);
                }
                continue;
            }

            let mut visitor = BoundaryVisitor {
                package_name: &self.package.name,
                module_path,
                source,
                workspace_root: self.workspace_root,
                is_core_crate: self.is_core_crate,
                violations: &mut self.violations,
            };
            visitor.visit_item(item);
        }
    }
}

struct BoundaryVisitor<'a> {
    package_name: &'a str,
    module_path: &'a [String],
    source: &'a Path,
    workspace_root: &'a Path,
    is_core_crate: bool,
    violations: &'a mut BTreeSet<String>,
}

impl BoundaryVisitor<'_> {
    fn in_api(&self) -> bool {
        self.module_path
            .first()
            .is_some_and(|module| module == "api")
    }

    fn record(&mut self, detail: impl AsRef<str>) {
        let source = self
            .source
            .strip_prefix(self.workspace_root)
            .unwrap_or(self.source);
        self.violations.insert(format!(
            "{}:{}: {}",
            self.package_name,
            source.display(),
            detail.as_ref()
        ));
    }

    fn inspect_sql(&mut self, sql: &str) {
        let Some(table) = mutation_table(sql) else {
            return;
        };
        let table = table.as_str();

        if self.package_name == "aether-automation"
            && self.in_api()
            && AUTOMATION_CONFIGURATION_TABLES.contains(&table)
        {
            self.record(format!(
                "HTTP adapter contains direct mutation SQL for {table}; use the governed application facade"
            ));
        }
        if self.package_name == "aether-io"
            && self.in_api()
            && IO_POINT_CONFIGURATION_TABLES.contains(&table)
        {
            self.record(format!(
                "HTTP adapter contains direct point mutation SQL for {table}; use the point-topology application facade"
            ));
        }
        if self.package_name == "aether" && table == "action_routing" {
            self.record(
                "CLI contains direct action-routing mutation SQL; use the governed HTTP/application command",
            );
        }
    }

    fn inspect_identifier(&mut self, identifier: &str) {
        if LEGACY_COMMAND_TRANSPORT_SYMBOLS.contains(&identifier) {
            self.record(format!(
                "retired command-transport symbol {identifier} was restored"
            ));
        }
        if matches!(
            identifier,
            "AcquisitionStateWriter" | "ShmAcquisitionStateWriter" | "ShmWriterHandle"
        ) && !matches!(
            self.package_name,
            "aether-acquisition-port" | "aether-shm-bridge" | "aether-io"
        ) {
            self.record(format!(
                "non-acquisition package references writer capability {identifier}"
            ));
        }
        if self.is_core_crate && matches!(identifier, "Rtdb" | "RedisRtdb") {
            self.record(format!(
                "core crate restored Redis-shaped abstraction {identifier}"
            ));
        }
        if self.package_name == "aether-rules"
            && matches!(identifier, "ActionDispatch" | "with_action_dispatch")
        {
            self.record(format!(
                "rule engine bypasses the governed command facade through {identifier}"
            ));
        }
        if matches!(self.package_name, "aether-automation" | "aether-rules")
            && matches!(
                identifier,
                "LegacyRoutingTables" | "RoutingCache" | "compatibility_routing" | "routing_cache"
            )
        {
            self.record(format!(
                "automation restored mutable legacy routing projection {identifier}"
            ));
        }
        if matches!(
            self.package_name,
            "aether-pack" | "aether-automation" | "aether"
        ) && LEGACY_PRODUCT_SYMBOLS.contains(&identifier)
        {
            self.record(format!(
                "removed built-in product compatibility symbol {identifier} was restored"
            ));
        }
        if matches!(self.package_name, "aether-automation" | "aether-io")
            && identifier == "point_mappings"
        {
            self.record("removed point_mappings compatibility projection was restored");
        }
        if self.package_name == "aether-io" && identifier == "ReloadableService" {
            self.record("IO restored a duplicate runtime-reload owner");
        }
        if self.package_name == "aether-io" && RETIRED_IO_SYMBOLS.contains(&identifier) {
            self.record(format!(
                "production IO restored retired symbol {identifier}"
            ));
        }
    }

    fn inspect_call_name(&mut self, name: &str) {
        if ((self.package_name == "aether-automation" && self.in_api())
            || self.package_name == "aether")
            && LEGACY_ACTION_ROUTING_METHODS.contains(&name)
        {
            self.record(format!(
                "transport calls retired direct action-routing mutator {name}"
            ));
        }
        if self.package_name == "aether-io"
            && self.in_api()
            && matches!(
                name,
                "create_channel" | "remove_channel" | "respawn_channel"
            )
        {
            self.record(format!(
                "IO HTTP adapter calls runtime lifecycle method {name} directly"
            ));
        }
    }
}

impl<'ast> Visit<'ast> for BoundaryVisitor<'_> {
    fn visit_ident(&mut self, identifier: &'ast Ident) {
        self.inspect_identifier(&identifier.to_string());
        visit::visit_ident(self, identifier);
    }

    fn visit_lit_str(&mut self, literal: &'ast syn::LitStr) {
        let value = literal.value();
        self.inspect_sql(&value);
        if self.package_name == "aether-io"
            && (value == "networkctl"
                || value.contains("script-host")
                || value.contains("/api/network/"))
        {
            self.record(format!(
                "production IO restored retired host surface {value:?}"
            ));
        }
        visit::visit_lit_str(self, literal);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        if self.in_api()
            && matches!(self.package_name, "aether-automation" | "aether-io")
            && node
                .path
                .segments
                .last()
                .is_some_and(|segment| segment.ident == "include")
        {
            self.record(
                "governed HTTP adapter includes source that the AST boundary cannot inspect",
            );
        }
        inspect_macro_strings(node.tokens.clone(), |value| self.inspect_sql(&value));
        visit::visit_macro(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        self.inspect_call_name(&node.method.to_string());
        visit::visit_expr_method_call(self, node);
    }

    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let Expr::Path(path) = &*node.func
            && let Some(segment) = path.path.segments.last()
        {
            self.inspect_call_name(&segment.ident.to_string());
        }
        visit::visit_expr_call(self, node);
    }

    fn visit_expr_path(&mut self, node: &'ast ExprPath) {
        if self.package_name == "aether-io" {
            let segments = node
                .path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect::<Vec<_>>();
            if segments
                .windows(2)
                .any(|window| window == ["process", "Command"])
            {
                self.record("production IO launches a subprocess");
            }
        }
        visit::visit_expr_path(self, node);
    }

    fn visit_item_use(&mut self, node: &'ast ItemUse) {
        if self.package_name == "aether-io" {
            let mut paths = Vec::new();
            collect_use_paths(&node.tree, &mut Vec::new(), &mut paths);
            if paths.iter().any(|path| {
                path.windows(2)
                    .any(|window| window == ["process", "Command"])
            }) {
                self.record("production IO imports a subprocess launcher");
            }
        }
        visit::visit_item_use(self, node);
    }

    fn visit_item_enum(&mut self, node: &'ast ItemEnum) {
        if self.package_name == "aether"
            && node.ident == "ServiceCommands"
            && node
                .variants
                .iter()
                .any(|variant| variant.ident == "Reload")
        {
            self.record("service CLI restored the legacy reload fanout command");
        }
        visit::visit_item_enum(self, node);
    }

    fn visit_item_impl(&mut self, node: &'ast ItemImpl) {
        let is_instance_manager = match &*node.self_ty {
            syn::Type::Path(path) => path
                .path
                .segments
                .last()
                .is_some_and(|segment| segment.ident == "InstanceManager"),
            _ => false,
        };
        if self.package_name == "aether-automation" && is_instance_manager {
            for item in &node.items {
                if let syn::ImplItem::Fn(method) = item
                    && !has_cfg_test(&method.attrs)
                    && LEGACY_INSTANCE_MANAGER_MUTATIONS
                        .contains(&method.sig.ident.to_string().as_str())
                {
                    self.record(format!(
                        "InstanceManager restored direct mutation method {}",
                        method.sig.ident
                    ));
                }
            }
        }
        visit::visit_item_impl(self, node);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast ImplItemFn) {
        if has_cfg_test(&node.attrs) {
            return;
        }
        visit::visit_impl_item_fn(self, node);
    }
}

fn collect_use_paths(tree: &UseTree, prefix: &mut Vec<String>, paths: &mut Vec<Vec<String>>) {
    match tree {
        UseTree::Path(path) => {
            prefix.push(path.ident.to_string());
            collect_use_paths(&path.tree, prefix, paths);
            prefix.pop();
        },
        UseTree::Name(name) => {
            let mut path = prefix.clone();
            path.push(name.ident.to_string());
            paths.push(path);
        },
        UseTree::Rename(rename) => {
            let mut path = prefix.clone();
            path.push(rename.ident.to_string());
            paths.push(path);
        },
        UseTree::Glob(_) => paths.push(prefix.clone()),
        UseTree::Group(group) => {
            for tree in &group.items {
                collect_use_paths(tree, prefix, paths);
            }
        },
    }
}

#[test]
fn production_sources_preserve_irreducible_boundaries() {
    let metadata = workspace_metadata();
    let mut violations = BTreeSet::new();

    for package in metadata
        .packages
        .iter()
        .filter(|package| metadata.workspace_members.contains(&package.id))
    {
        violations.extend(SourceScanner::new(&metadata.workspace_root, package).scan());
    }

    assert!(
        violations.is_empty(),
        "source architecture boundaries changed:\n- {}",
        violations.into_iter().collect::<Vec<_>>().join("\n- ")
    );
}

#[test]
fn rule_calculation_has_one_concrete_owner() {
    let root = workspace_metadata().workspace_root;
    for relative in [
        "libs/aether-rules/src/calc.rs",
        "libs/aether-rules/src/calc/state.rs",
        "libs/aether-rules/src/executor.rs",
        "libs/aether-rules/src/scheduler.rs",
        "services/automation/src/rule_routes.rs",
        "services/automation/src/infra/rule_mutation.rs",
    ] {
        let source = fs::read_to_string(root.join(relative))
            .unwrap_or_else(|error| panic!("failed to read {relative}: {error}"));
        for retired in ["StateStore", "aether_calc", "MemoryStateStore"] {
            assert!(
                !source.contains(retired),
                "{relative} restored speculative calculation abstraction {retired}"
            );
        }
    }
}

#[test]
fn io_point_topology_is_offline_only() {
    let root = workspace_metadata().workspace_root;
    for (relative, forbidden) in [
        (
            "services/io/src/api/handlers/mapping_handlers.rs",
            "update_channel_mappings_handler",
        ),
        (
            "services/io/src/api/routes.rs",
            "create_telemetry_point_handler",
        ),
        (
            "services/io/src/api/routes.rs",
            "batch_point_operations_handler",
        ),
        (
            "services/io/src/api/routes.rs",
            "update_channel_mappings_handler",
        ),
        ("services/io/src/api/routes.rs", "/api/templates"),
        ("services/io/src/api/routes.rs", "/api/channels/reload"),
        ("services/io/src/api/routes.rs", "/api/routing/reload"),
        ("services/io/src/api/routes.rs", "/write"),
        (
            "services/io/src/api/handlers/channel_management_handlers/reload.rs",
            "reload_configuration_handler",
        ),
        (
            "services/io/src/api/handlers/channel_management_handlers/reload.rs",
            "reload_routing_handler",
        ),
    ] {
        let source = fs::read_to_string(root.join(relative))
            .unwrap_or_else(|error| panic!("failed to read {relative}: {error}"));
        assert!(
            !source.contains(forbidden),
            "{relative} restored online point-topology compatibility surface {forbidden}"
        );
    }
}

#[test]
fn shared_service_kernel_stays_console_first() {
    let root = workspace_metadata().workspace_root;
    for retired in [
        "libs/common/src/admin_api.rs",
        "services/io/src/api/handlers/admin_handlers.rs",
        "services/io/src/api/handlers/control_handlers.rs",
        "services/automation/src/api/admin_handlers.rs",
    ] {
        assert!(
            !root.join(retired).exists(),
            "retired runtime log administration surface was restored at {retired}"
        );
    }

    for (relative, forbidden) in [
        ("libs/common/Cargo.toml", "tracing-appender"),
        ("libs/common/Cargo.toml", "flate2"),
        ("libs/common/Cargo.toml", "aether-schema-macro"),
        ("libs/common/Cargo.toml", "schemars"),
        ("libs/common/src/lib.rs", "test_utils"),
        ("libs/common/src/lib.rs", "pub mod validation;"),
        ("libs/common/src/lib.rs", "pub mod config_loader;"),
        ("libs/common/src/lib.rs", "pub mod dependency;"),
        ("libs/common/src/lib.rs", "pub mod bootstrap_system;"),
        ("libs/common/src/lib.rs", "log_rotation"),
        ("libs/common/src/service_config.rs", "ReloadableService"),
        ("libs/common/src/service_config.rs", "GenericValidator"),
        ("libs/common/src/service_config.rs", "FourRemote"),
        ("libs/common/src/site_schema.rs", "channel_templates"),
        ("tools/aether/src/core/schema.rs", "channel_templates"),
        ("tools/aether/src/core/exporter.rs", "\"rules\" =>"),
        ("libs/common/src/logging.rs", "DailyRollingWriter"),
        ("libs/common/src/lib.rs", "pub mod admin_api;"),
        ("libs/common/src/logging.rs", "reload::Layer"),
        ("libs/common/src/logging.rs", "pub fn set_log_level"),
        ("libs/common/src/logging.rs", "pub fn get_log_level"),
        ("services/io/src/api/routes.rs", "/api/admin/logs/level"),
        (
            "services/io/src/api/routes.rs",
            "/api/channels/{id}/logging",
        ),
        ("services/io/src/core/channels/types.rs", "SetLogLevel"),
        (
            "services/io/src/protocols/gateway/runtime.rs",
            "fn log_handler",
        ),
        (
            "services/io/src/protocols/core/logging.rs",
            "fn set_log_level",
        ),
        ("services/io/src/api/routes.rs", "/api/admin/logs/files"),
        ("services/io/src/api/routes.rs", "/api/admin/logs/view"),
        ("services/automation/src/routes.rs", "/api/admin/logs/level"),
        ("services/automation/src/routes.rs", "/api/admin/logs/files"),
        ("services/automation/src/routes.rs", "/api/admin/logs/view"),
        ("services/history/src/routes.rs", "/api/admin/logs/level"),
        ("services/history/src/routes.rs", "/api/admin/logs/files"),
        ("services/uplink/src/routes.rs", "/api/admin/logs/level"),
        ("services/uplink/src/routes.rs", "/api/admin/logs/files"),
        ("services/alarm/src/routes.rs", "/api/admin/logs/level"),
        ("services/alarm/src/routes.rs", "/api/admin/logs/files"),
        ("services/api/src/main.rs", "/logs/level"),
        ("services/api/src/main.rs", "/logs/files"),
        ("services/automation/src/lib.rs", "pub mod reload;"),
        ("config.template/global.yaml", "logging:"),
    ] {
        let source = fs::read_to_string(root.join(relative))
            .unwrap_or_else(|error| panic!("failed to read {relative}: {error}"));
        assert!(
            !source.contains(forbidden),
            "{relative} restored retired shared service surface {forbidden}"
        );
    }
}

#[test]
fn zero_consumer_runtime_compatibility_planes_stay_retired() {
    let root = workspace_metadata().workspace_root;
    for (relative, forbidden) in [
        (
            "services/io/src/protocols/adapters/http.rs",
            "WebhookHandler",
        ),
        ("services/io/src/protocols/adapters/http.rs", "HttpMode"),
        ("services/automation/src/routes.rs", "/api/instances/reload"),
        (
            "services/automation/src/routes.rs",
            "/api/instances/{id}/routing/validate",
        ),
        (
            "services/automation/src/routes.rs",
            "/api/routing/instances/{instance_name}",
        ),
        (
            "services/automation/src/routes.rs",
            "/api/routing/channels/{channel_id}",
        ),
        (
            "services/uplink/adapters/cloudlink-mqtt/src/config.rs",
            "CloudLinkMigrationMode",
        ),
        ("tools/aether/src/models.rs", "ProductCommands::Available"),
    ] {
        let source = fs::read_to_string(root.join(relative))
            .unwrap_or_else(|error| panic!("failed to read {relative}: {error}"));
        assert!(
            !source.contains(forbidden),
            "{relative} restored zero-consumer compatibility surface {forbidden}"
        );
    }
}

#[test]
fn zero_consumer_service_aliases_stay_retired() {
    let root = workspace_metadata().workspace_root;

    for retired in [
        "services/history/src/backend_influx.rs",
        "services/automation/src/api/cloud_sync.rs",
    ] {
        assert!(
            !root.join(retired).exists(),
            "retired zero-consumer service module was restored at {retired}"
        );
    }

    for (relative, forbidden) in [
        ("services/history/src/main.rs", "backend_influx"),
        ("services/history/src/routes.rs", "influxdb"),
        ("services/history/src/routes.rs", "Influx"),
        ("services/history/src/models.rs", "influxdb"),
        ("services/history/src/models.rs", "Influx"),
        (
            "services/automation/src/routes.rs",
            ".route(\"/api/instances/list\"",
        ),
        (
            "services/automation/src/routes.rs",
            ".route(\"/api/instances/search\"",
        ),
        (
            "services/automation/src/routes.rs",
            ".route(\"/api/instances/export\"",
        ),
        ("services/automation/src/lib.rs", "pub mod cloud_sync"),
        (
            "services/uplink/src/routes.rs",
            ".route(\"/netApi/inst-sync\"",
        ),
        ("services/io/Cargo.toml", "swagger-ui = [\"openapi\"]"),
        (
            "services/automation/Cargo.toml",
            "swagger-ui = [\"openapi\"]",
        ),
        ("services/history/Cargo.toml", "swagger-ui = [\"openapi\"]"),
        ("services/uplink/Cargo.toml", "swagger-ui = [\"openapi\"]"),
        ("services/alarm/Cargo.toml", "swagger-ui = [\"openapi\"]"),
    ] {
        let source = fs::read_to_string(root.join(relative))
            .unwrap_or_else(|error| panic!("failed to read {relative}: {error}"));
        assert!(
            !source.contains(forbidden),
            "{relative} restored retired zero-consumer service surface {forbidden}"
        );
    }
}

#[test]
fn io_composition_matches_the_runtime_manifest_authority() {
    let root = workspace_metadata().workspace_root;
    for retired in [
        "services/io/src/api/handlers/protocol_handlers.rs",
        "services/io/src/protocols/core/metadata.rs",
        "services/io/src/core/config/manager.rs",
        "services/io/src/core/config/types.rs",
        "services/io/src/core/channels/traits.rs",
    ] {
        assert!(
            !root.join(retired).exists(),
            "retired duplicate IO discovery authority was restored at {retired}"
        );
    }

    let routes =
        fs::read_to_string(root.join("services/io/src/api/routes.rs")).expect("read IO routes");
    for retired in [
        "pub fn create_api_routes(",
        "/api/channels/{id}/control",
        "/api/protocols",
        "/api/channels/list",
        "/api/channels/search",
        "/api/points",
        "/T/points/{point_id}",
        "/S/points/{point_id}",
        "/C/points/{point_id}",
        "/A/points/{point_id}",
    ] {
        assert!(
            !routes.contains(retired),
            "IO restored duplicate browser/discovery route {retired}"
        );
    }

    for (relative, retired) in [
        (
            "services/io/src/api/handlers/channel_management_handlers.rs",
            "fn unavailable",
        ),
        (
            "services/io/src/api/handlers/channel_management_handlers.rs",
            "inner: Option<GovernedChannelManagement>",
        ),
        (
            "services/io/src/protocols/adapters/modbus_config.rs",
            "ReconnectConfig",
        ),
        ("services/io/src/protocols/core/slot.rs", "ShardedSlotStore"),
        ("services/io/src/core/config.rs", "pub type AppConfig"),
        ("services/io/src/core/config.rs", "pub type ServiceConfig"),
        (
            "services/io/src/lib.rs",
            "pub use core::config::ConfigManager",
        ),
        (
            "services/io/src/lib.rs",
            "pub use core::channels::ChannelManager",
        ),
        ("services/io/src/protocols.rs", "pub mod prelude"),
        ("services/io/src/core/channels.rs", "pub use factory::"),
        (
            "services/io/src/core/channels/converters.rs",
            "convert_can_to_point_configs",
        ),
    ] {
        let source = fs::read_to_string(root.join(relative))
            .unwrap_or_else(|error| panic!("failed to read {relative}: {error}"));
        assert!(
            !source.contains(retired),
            "{relative} restored retired IO compatibility implementation {retired}"
        );
    }

    let creation =
        fs::read_to_string(root.join("services/io/src/core/channels/channel_creation.rs"))
            .expect("read IO channel composition");
    for required in [
        "\"mqtt\" =>",
        "create_mqtt_channel_impl",
        "\"http\" =>",
        "create_http_channel_impl",
    ] {
        assert!(
            creation.contains(required),
            "distribution IO feature has no concrete composition branch {required}"
        );
    }

    let mapper = fs::read_to_string(root.join("services/io/src/protocols/core/json_mapper.rs"))
        .expect("read JSON mapper");
    assert!(mapper.contains("from_inline_mappings"));
    assert!(!mapper.contains("from_database"));

    let loader = fs::read_to_string(root.join("services/io/src/core/config/sqlite_loader.rs"))
        .expect("read IO topology loader");
    assert!(loader.contains("physical topology snapshot"));
    assert!(loader.matches("fetch_all(&mut *transaction)").count() >= 4);
}

#[test]
fn routing_library_is_storage_agnostic() {
    let root = workspace_metadata().workspace_root;
    for relative in [
        "libs/aether-routing/Cargo.toml",
        "libs/aether-routing/src/lib.rs",
        "libs/aether-routing/src/channel.rs",
        "libs/aether-routing/src/snapshot.rs",
    ] {
        let source = fs::read_to_string(root.join(relative))
            .unwrap_or_else(|error| panic!("failed to read {relative}: {error}"));
        for forbidden in [
            "sqlx",
            "Sqlite",
            "measurement_routing",
            "action_routing",
            "channel_routing",
        ] {
            assert!(
                !source.contains(forbidden),
                "{relative} restored concrete storage concern {forbidden}"
            );
        }
    }
}

#[test]
fn cli_and_mcp_share_one_concrete_authenticated_http_client() {
    let root = workspace_metadata().workspace_root;
    for relative in [
        "tools/aether/src/alarms.rs",
        "tools/aether/src/channels.rs",
        "tools/aether/src/history.rs",
        "tools/aether/src/models/client.rs",
        "tools/aether/src/net.rs",
        "tools/aether/src/routing.rs",
        "tools/aether/src/rules.rs",
    ] {
        let source = fs::read_to_string(root.join(relative))
            .unwrap_or_else(|error| panic!("failed to read {relative}: {error}"));
        assert!(
            !source.contains("access_token: Option<String>")
                && !source.contains("client: reqwest::Client"),
            "{relative} restored duplicate authenticated HTTP client state"
        );
    }
}

#[test]
fn sql_classifier_recognizes_supported_mutations_without_matching_reads() {
    assert_eq!(
        mutation_table(" INSERT OR REPLACE INTO action_routing (id) VALUES (1)"),
        Some("action_routing".to_string())
    );
    assert_eq!(
        mutation_table("UPDATE instances SET name = ? WHERE id = ?"),
        Some("instances".to_string())
    );
    assert_eq!(
        mutation_table("DELETE FROM telemetry_points WHERE channel_id = ?"),
        Some("telemetry_points".to_string())
    );
    assert_eq!(mutation_table("SELECT * FROM action_routing"), None);
}

#[test]
fn structured_policy_detects_bypasses_and_ignores_test_only_fixtures() {
    let automation = inspect_snippet(
        "aether-automation",
        &["api"],
        r#"
        async fn bypass(pool: &sqlx::SqlitePool) {
            sqlx::query("DELETE FROM action_routing WHERE instance_id = ?")
                .execute(pool)
                .await;
        }

        #[cfg(test)]
        async fn fixture(pool: &sqlx::SqlitePool) {
            sqlx::query("DELETE FROM instances").execute(pool).await;
        }
        "#,
    );
    assert_eq!(automation.len(), 1, "{automation:#?}");
    assert!(
        automation
            .iter()
            .next()
            .is_some_and(|violation| violation.contains("action_routing"))
    );

    let cli = inspect_snippet(
        "aether",
        &[],
        "enum ServiceCommands { Start, Reload { services: Vec<String> } }",
    );
    assert!(
        cli.iter()
            .any(|violation| violation.contains("reload fanout")),
        "{cli:#?}"
    );
}

#[test]
fn production_io_rejects_simulation_host_network_and_subprocess_surfaces() {
    let violations = inspect_snippet(
        "aether-io",
        &[],
        r#"
        use tokio::process::Command;

        struct VirtualChannel;

        async fn mutate_host() {
            let _ = Command::new("networkctl").output().await;
        }

        #[cfg(test)]
        struct ScriptRunner;
        "#,
    );

    for expected in ["subprocess launcher", "VirtualChannel", "networkctl"] {
        assert!(
            violations
                .iter()
                .any(|violation| violation.contains(expected)),
            "missing {expected:?} violation: {violations:#?}"
        );
    }
    assert!(
        violations
            .iter()
            .all(|violation| !violation.contains("ScriptRunner")),
        "test-only fixtures must stay outside production checks: {violations:#?}"
    );
}

#[test]
fn cfg_test_detection_is_structural() {
    let item: Item = syn::parse_quote! {
        #[cfg(any(test, feature = "fixture"))]
        mod fixture;
    };
    assert!(has_cfg_test(item_attributes(&item)));

    let production: Item = syn::parse_quote! {
        #[cfg(feature = "sqlite")]
        mod adapter;
    };
    assert!(!has_cfg_test(item_attributes(&production)));
}

fn inspect_snippet(package_name: &str, modules: &[&str], source: &str) -> BTreeSet<String> {
    let syntax = syn::parse_file(source).expect("valid architecture fixture");
    let module_path = modules
        .iter()
        .map(|module| (*module).to_string())
        .collect::<Vec<_>>();
    let mut violations = BTreeSet::new();
    for item in &syntax.items {
        if has_cfg_test(item_attributes(item)) {
            continue;
        }
        let mut visitor = BoundaryVisitor {
            package_name,
            module_path: &module_path,
            source: Path::new("fixture.rs"),
            workspace_root: Path::new(""),
            is_core_crate: false,
            violations: &mut violations,
        };
        visitor.visit_item(item);
    }
    violations
}

fn workspace_metadata() -> MetadataDocument {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let output = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--locked",
            "--format-version",
            "1",
            "--no-deps",
            "--manifest-path",
        ])
        .arg(&manifest)
        .output()
        .unwrap_or_else(|error| panic!("failed to execute cargo metadata: {error}"));
    assert!(
        output.status.success(),
        "cargo metadata failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("cargo metadata returned invalid JSON: {error}"))
}

fn is_production_target(target: &Target) -> bool {
    target.kind.iter().any(|kind| {
        matches!(
            kind.as_str(),
            "lib" | "bin" | "proc-macro" | "cdylib" | "staticlib"
        )
    })
}

fn root_module_directory(source: &Path) -> PathBuf {
    let parent = source.parent().unwrap_or_else(|| Path::new("."));
    match source.file_stem().and_then(|stem| stem.to_str()) {
        Some("lib" | "main" | "mod") => parent.to_path_buf(),
        Some(stem) => parent.join(stem),
        None => parent.to_path_buf(),
    }
}

fn resolve_module_source(
    current_source: &Path,
    module_directory: &Path,
    module: &Ident,
    attributes: &[Attribute],
) -> PathBuf {
    if let Some(path) = path_attribute(attributes) {
        return current_source
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(path);
    }

    let flat = module_directory.join(format!("{module}.rs"));
    if flat.is_file() {
        return flat;
    }
    let nested = module_directory.join(module.to_string()).join("mod.rs");
    if nested.is_file() {
        return nested;
    }
    panic!(
        "module {module} declared by {} has no source at {} or {}",
        current_source.display(),
        flat.display(),
        nested.display()
    );
}

fn path_attribute(attributes: &[Attribute]) -> Option<PathBuf> {
    attributes.iter().find_map(|attribute| {
        if !attribute.path().is_ident("path") {
            return None;
        }
        let Meta::NameValue(value) = &attribute.meta else {
            return None;
        };
        let Expr::Lit(literal) = &value.value else {
            return None;
        };
        let Lit::Str(path) = &literal.lit else {
            return None;
        };
        Some(PathBuf::from(path.value()))
    })
}

fn item_attributes(item: &Item) -> &[Attribute] {
    match item {
        Item::Const(item) => &item.attrs,
        Item::Enum(item) => &item.attrs,
        Item::ExternCrate(item) => &item.attrs,
        Item::Fn(item) => &item.attrs,
        Item::ForeignMod(item) => &item.attrs,
        Item::Impl(item) => &item.attrs,
        Item::Macro(item) => &item.attrs,
        Item::Mod(item) => &item.attrs,
        Item::Static(item) => &item.attrs,
        Item::Struct(item) => &item.attrs,
        Item::Trait(item) => &item.attrs,
        Item::TraitAlias(item) => &item.attrs,
        Item::Type(item) => &item.attrs,
        Item::Union(item) => &item.attrs,
        Item::Use(item) => &item.attrs,
        _ => &[],
    }
}

fn has_cfg_test(attributes: &[Attribute]) -> bool {
    attributes
        .iter()
        .any(|attribute| attribute.path().is_ident("cfg") && meta_mentions_test(&attribute.meta))
}

fn meta_mentions_test(meta: &Meta) -> bool {
    match meta {
        Meta::Path(path) => path.is_ident("test"),
        Meta::List(list) => list
            .parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
            .is_ok_and(|items| items.iter().any(meta_mentions_test)),
        Meta::NameValue(_) => false,
    }
}

fn mutation_table(sql: &str) -> Option<String> {
    let tokens = sql
        .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .filter(|token| !token.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    let first = tokens.first()?.as_str();
    let table = match first {
        "insert" | "replace" => {
            let into = tokens.iter().position(|token| token == "into")?;
            tokens.get(into + 1)?
        },
        "update" => tokens.get(1)?,
        "delete" => {
            let from = tokens.iter().position(|token| token == "from")?;
            tokens.get(from + 1)?
        },
        _ => return None,
    };
    Some(table.clone())
}

fn inspect_macro_strings(tokens: TokenStream, mut inspect: impl FnMut(String)) {
    fn collect(tokens: TokenStream, strings: &mut Vec<String>) {
        for token in tokens {
            match token {
                TokenTree::Group(group) => collect(group.stream(), strings),
                TokenTree::Literal(literal) => {
                    if let Ok(string) = syn::parse_str::<syn::LitStr>(&literal.to_string()) {
                        strings.push(string.value());
                    }
                },
                TokenTree::Ident(_) | TokenTree::Punct(_) => {},
            }
        }
    }

    let mut strings = Vec::new();
    collect(tokens, &mut strings);
    for string in &strings {
        inspect(string.clone());
    }
    if strings.len() > 1 {
        inspect(strings.concat());
    }
}
