use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use proc_macro2::{TokenStream, TokenTree};
use serde::Deserialize;
use syn::punctuated::Punctuated;
use syn::visit::{self, Visit};
use syn::{
    Attribute, Expr, ExprPath, Ident, ImplItemFn, Item, ItemEnum, ItemImpl, ItemStruct, ItemTrait,
    ItemUse, Lit, Meta, Token, UseTree,
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
    "AppConfig",
    "BatchCommand",
    "CanAddress",
    "ChannelBuildResult",
    "ChannelMode",
    "ChannelModeConfig",
    "ChannelStatus",
    "CommandBatcher",
    "ConfigManager",
    "DataEventHandler",
    "DataSlot",
    "Dl645Address",
    "ErrorExt",
    "ExtendedPointData",
    "GatewayConfig",
    "GatewayGlobalConfig",
    "GpioAddress",
    "MatterAddress",
    "MatterChannel",
    "MatterConfig",
    "MatterParamsConfig",
    "ModbusChannelParamsConfig",
    "NetworkConfigUpdateRequest",
    "NetworkInterfaceConfig",
    "PointData",
    "PointDataMap",
    "PointDef",
    "PollingConfig",
    "ProtocolAddress",
    "ProtocolValue",
    "ProtocolServer",
    "RuntimeIoConfig",
    "ScriptRunner",
    "SharedJsonMapper",
    "ShardedSlotStore",
    "SlotStore",
    "TelemetryBatch",
    "TestChannelParams",
    "VirtualAddress",
    "VirtualChannel",
    "VirtualMapping",
    "WebhookHandler",
    "WritePointRequest",
    "data_store",
    "device_id_path",
    "network_handlers",
    "run_automatic_io_reconciliation",
    "simulation_writes_enabled",
    "start_communication_service",
    "transform_script",
    "write_channel_point",
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

    fn in_io_protocol_adapter(&self) -> bool {
        self.package_name == "aether-io"
            && self
                .module_path
                .first()
                .is_some_and(|module| module == "protocols")
            && self
                .module_path
                .get(1)
                .is_some_and(|module| module == "adapters")
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
                "LegacyRoutingTables"
                    | "RoutingCache"
                    | "compatibility_routing"
                    | "routing_cache"
                    | "aether_routing"
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
        if self.package_name == "aether" && identifier == "channels_write" {
            self.record("CLI/MCP restored direct acquisition live-state injection");
        }
        if self.in_io_protocol_adapter() && identifier == "sqlx" {
            self.record(
                "protocol adapter restored SQLite ownership; consume the immutable runtime snapshot",
            );
        }
        if self.package_name == "aether-config"
            && matches!(
                identifier,
                "RuntimeChannelConfig"
                    | "ModbusMapping"
                    | "GpioMapping"
                    | "IecMapping"
                    | "GrpcMapping"
                    | "CanMapping"
            )
        {
            self.record(format!(
                "shared configuration restored IO-owned runtime or protocol DTO {identifier}"
            ));
        }
        if self.package_name == "aether-config"
            && matches!(
                identifier,
                "supported_protocols"
                    | "validate_required_string_parameter"
                    | "validate_required_integer_parameter"
            )
        {
            self.record(format!(
                "shared configuration restored protocol-specific IO policy {identifier}"
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
        if matches!(self.package_name, "aether-io" | "aether")
            && (value == "AETHER_ALLOW_SIMULATION_WRITES"
                || (value.contains("/api/channels/") && value.contains("/write")))
        {
            self.record("production surface restored direct acquisition live-state injection");
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

    fn visit_item_struct(&mut self, node: &'ast ItemStruct) {
        if self.package_name == "aether-io" {
            if node.ident == "ChannelManager"
                && node
                    .fields
                    .iter()
                    .filter_map(|field| field.ident.as_ref())
                    .any(|field| field == "sqlite_pool")
            {
                self.record(
                    "ChannelManager restored SQLite ownership; load complete snapshots before activation",
                );
            }
            if node.ident == "PointConfig" {
                for field in node.fields.iter().filter_map(|field| field.ident.as_ref()) {
                    if matches!(
                        field.to_string().as_str(),
                        "name" | "poll_group" | "enabled"
                    ) {
                        self.record(format!(
                            "PointConfig restored unused field {field}; protocol adapters own mapping metadata"
                        ));
                    }
                }
            }
            if node.ident == "StoredChannelConfig" {
                self.record(
                    "IO restored a duplicate channels.config payload model; use aether-config's persisted codec",
                );
            }
            let retired_fields: &[&str] = match node.ident.to_string().as_str() {
                "BleParamsConfig" => &["mtu", "reconnect_interval_ms"],
                "HttpParamsConfig" => &[
                    "mode",
                    "listen_path",
                    "auth_token",
                    "retry_delay_ms",
                    "interval_ms",
                    "max_retries",
                ],
                "J1939Config" => &["our_address", "pgn_list", "request_interval_ms"],
                "MqttParamsConfig" => &[
                    "connect_timeout_ms",
                    "max_reconnect_attempts",
                    "reconnect_delay_ms",
                ],
                "ZigbeeParamsConfig" => &[
                    "gateway_type",
                    "pan_id",
                    "channel",
                    "permit_join_on_start",
                    "reconnect_interval_ms",
                ],
                _ => &[],
            };
            for field in node.fields.iter().filter_map(|field| field.ident.as_ref()) {
                if retired_fields.contains(&field.to_string().as_str()) {
                    self.record(format!(
                        "{} restored retired no-effect field {field}",
                        node.ident
                    ));
                }
            }
        }
        visit::visit_item_struct(self, node);
    }

    fn visit_item_trait(&mut self, node: &'ast ItemTrait) {
        if self.package_name == "aether-io" {
            for item in &node.items {
                let syn::TraitItem::Fn(method) = item else {
                    continue;
                };
                let method = method.sig.ident.to_string();
                let retired = (node.ident == "ChannelRuntime"
                    && matches!(method.as_str(), "id" | "name" | "protocol" | "log_handler"))
                    || (node.ident == "EventDrivenProtocol" && method == "set_event_handler");
                if retired {
                    self.record(format!("{} restored retired method {method}", node.ident));
                }
            }
        }
        visit::visit_item_trait(self, node);
    }

    fn visit_item_impl(&mut self, node: &'ast ItemImpl) {
        let owner = match &*node.self_ty {
            syn::Type::Path(path) => path
                .path
                .segments
                .last()
                .map(|segment| segment.ident.to_string()),
            _ => None,
        };
        if self.package_name == "aether-automation" && owner.as_deref() == Some("InstanceManager") {
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
        if self.package_name == "aether-io"
            && let Some(owner) = owner.as_deref()
        {
            for item in &node.items {
                let syn::ImplItem::Fn(method) = item else {
                    continue;
                };
                if has_cfg_test(&method.attrs) {
                    continue;
                }
                let method = method.sig.ident.to_string();
                let retired = match owner {
                    "ChannelEntry" => matches!(method.as_str(), "abort_task" | "is_task_finished"),
                    "MmsValue" => {
                        matches!(method.as_str(), "to_f64" | "to_bool" | "to_string_val")
                    },
                    "AtomicDiagnostics" => method == "inc_error",
                    "ChannelFileLogHandler" => method == "add_channel",
                    "DataPoint" => {
                        matches!(method.as_str(), "with_quality" | "control" | "adjustment")
                    },
                    "ChannelManager" => {
                        matches!(
                            method.as_str(),
                            "cleanup"
                                | "connect_all_channels"
                                | "create_data_store"
                                | "get_channel_metadata"
                                | "load_channel_configuration"
                        )
                    },
                    "IoSqliteLoader" => matches!(method.as_str(), "new" | "with_pool"),
                    "RuntimeChannelConfig" => matches!(
                        method.as_str(),
                        "get_telemetry_point"
                            | "get_signal_point"
                            | "get_control_point"
                            | "get_adjustment_point"
                    ),
                    "PointConfig" => method == "with_name",
                    "JsonMapper" => method == "from_database",
                    "MqttChannel" | "HttpChannel" => {
                        matches!(method.as_str(), "load_mapper" | "run_poll_loop")
                    },
                    _ => false,
                };
                if retired {
                    self.record(format!("{owner} restored retired method {method}"));
                }
            }
        }
        if self.package_name == "aether-config" && owner.as_deref() == Some("ChannelConfig") {
            for item in &node.items {
                if let syn::ImplItem::Fn(method) = item
                    && method.sig.ident == "is_enabled"
                {
                    self.record("ChannelConfig restored unused is_enabled accessor");
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

        struct ConfigManager;
        struct RuntimeIoConfig;
        struct VirtualChannel;

        async fn run_automatic_io_reconciliation() {}
        async fn start_communication_service() {}

        async fn mutate_host() {
            let _ = Command::new("networkctl").output().await;
        }

        #[cfg(test)]
        struct ScriptRunner;
        "#,
    );

    for expected in [
        "subprocess launcher",
        "ConfigManager",
        "RuntimeIoConfig",
        "VirtualChannel",
        "networkctl",
        "run_automatic_io_reconciliation",
        "start_communication_service",
    ] {
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
fn io_cleanup_boundaries_are_owner_aware() {
    let violations = inspect_snippet(
        "aether-io",
        &[],
        r#"
        struct ChannelModeConfig;
        struct ChannelStatus;
        struct DataEventHandler;
        struct Dl645Address;
        struct MatterChannel;
        struct ChannelManager { sqlite_pool: () }
        struct IoSqliteLoader;
        struct PointConfig {
            name: Option<String>,
            poll_group: Option<String>,
            enabled: bool,
        }
        enum ProtocolAddress {}
        struct StoredChannelConfig;
        struct ZclValue;

        impl ChannelManager {
            fn connect_all_channels(&self) {}
            fn load_channel_configuration(&self) {}
        }

        impl IoSqliteLoader {
            fn new() -> Self { Self }
        }

        impl PointConfig {
            fn with_name(self) -> Self { self }
        }

        impl ZclValue {
            fn to_f64(&self) -> f64 { 0.0 }
        }

        trait ChannelRuntime {
            fn id(&self) -> u32;
            fn name(&self) -> &str;
            fn protocol(&self) -> &str;
        }

        trait EventDrivenProtocol {
            fn set_event_handler(&mut self, handler: DataEventHandler);
        }
        "#,
    );

    for expected in [
        "ChannelManager restored SQLite ownership",
        "PointConfig restored unused field name",
        "PointConfig restored unused field poll_group",
        "PointConfig restored unused field enabled",
        "duplicate channels.config payload model",
        "ChannelManager restored retired method connect_all_channels",
        "ChannelManager restored retired method load_channel_configuration",
        "IoSqliteLoader restored retired method new",
        "PointConfig restored retired method with_name",
        "ProtocolAddress",
        "ChannelModeConfig",
        "ChannelStatus",
        "DataEventHandler",
        "Dl645Address",
        "MatterChannel",
        "ChannelRuntime restored retired method id",
        "ChannelRuntime restored retired method name",
        "ChannelRuntime restored retired method protocol",
        "EventDrivenProtocol restored retired method set_event_handler",
    ] {
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
            .all(|violation| !violation.contains("ZclValue")),
        "active similarly named surfaces must remain allowed: {violations:#?}"
    );

    let shared_config = inspect_snippet(
        "aether-config",
        &[],
        r#"
        struct RuntimeChannelConfig;
        fn validate() {
            let supported_protocols = ["modbus_tcp"];
        }
        "#,
    );
    for expected in [
        "runtime or protocol DTO RuntimeChannelConfig",
        "supported_protocols",
    ] {
        assert!(
            shared_config
                .iter()
                .any(|violation| violation.contains(expected)),
            "missing {expected:?} violation: {shared_config:#?}"
        );
    }
}

#[test]
fn io_protocol_adapters_cannot_reclaim_sqlite_or_retired_runtime_fields() {
    let violations = inspect_snippet(
        "aether-io",
        &["protocols", "adapters"],
        r#"
        use sqlx::SqlitePool;

        struct J1939Config {
            pgn_list: Vec<u32>,
        }

        struct HttpParamsConfig {
            mode: String,
        }

        async fn reload(pool: &SqlitePool) {
            let _ = sqlx::query("SELECT 1").fetch_one(pool).await;
        }
        "#,
    );

    for expected in [
        "protocol adapter restored SQLite ownership",
        "J1939Config restored retired no-effect field pgn_list",
        "HttpParamsConfig restored retired no-effect field mode",
    ] {
        assert!(
            violations
                .iter()
                .any(|violation| violation.contains(expected)),
            "missing {expected:?} violation: {violations:#?}"
        );
    }
}

#[test]
fn polling_adapters_keep_owner_local_state_unshared() {
    let root = workspace_metadata().workspace_root;
    for relative in [
        "services/io/src/protocols/adapters/modbus.rs",
        "services/io/src/protocols/adapters/gpio.rs",
        "services/io/src/protocols/adapters/dl645.rs",
    ] {
        let source = fs::read_to_string(root.join(relative))
            .unwrap_or_else(|error| panic!("failed to read {relative}: {error}"));
        assert!(
            !source.contains("RwLock<ConnectionState>"),
            "{relative} restored shared connection state despite single-task ownership"
        );
    }

    let relative = "services/io/src/protocols/adapters/dl645.rs";
    let source = fs::read_to_string(root.join(relative))
        .unwrap_or_else(|error| panic!("failed to read {relative}: {error}"));
    assert!(
        !source.contains("Arc<LogContext>"),
        "{relative} restored an unshared logging allocation"
    );
}

#[test]
fn persisted_channel_payload_has_one_codec() {
    let root = workspace_metadata().workspace_root;
    for relative in [
        "services/io/src/core/config/sqlite_loader.rs",
        "services/io/src/channel_mutator.rs",
        "services/io/src/api/handlers/channel_handlers.rs",
        "tools/aether/src/core/exporter.rs",
        "tools/aether/src/core/syncer.rs",
    ] {
        let source = fs::read_to_string(root.join(relative))
            .unwrap_or_else(|error| panic!("failed to read {relative}: {error}"));
        assert!(
            source.contains("StoredChannelConfig"),
            "{relative} bypasses the canonical channels.config payload codec"
        );
        if !relative.starts_with("services/io/") {
            continue;
        }
        for retired in [
            "serde_json::from_str::<StoredChannelConfig",
            "serde_json::from_value::<StoredChannelConfig",
            ".remove(\"description\")",
            ".remove(\"parameters\")",
            ".remove(\"logging\")",
            "fn decode_config(",
            "fn extract_description_from_config(",
        ] {
            assert!(
                !source.contains(retired),
                "{relative} restored direct channels.config parsing through {retired}"
            );
        }
    }

    let exporter_source =
        fs::read_to_string(root.join("tools/aether/src/core/exporter.rs")).expect("CLI exporter");
    let exporter = inspect_named_method(&exporter_source, "export_channels");
    assert!(
        exporter.decode && !exporter.direct_json_parse && !exporter.direct_payload_field_access,
        "CLI export_channels must decode channels.config only through StoredChannelConfig"
    );

    let syncer_source =
        fs::read_to_string(root.join("tools/aether/src/core/syncer.rs")).expect("CLI syncer");
    let syncer = inspect_named_method(&syncer_source, "insert_channels");
    assert!(
        syncer.from_value
            && syncer.encode
            && !syncer.direct_json_parse
            && !syncer.direct_payload_field_access,
        "CLI insert_channels must encode channels.config only through StoredChannelConfig"
    );
}

#[test]
fn io_protocol_factory_registry_is_the_only_runtime_dispatch_table() {
    let root = workspace_metadata().workspace_root;
    let factory = fs::read_to_string(root.join("services/io/src/protocols/factory.rs"))
        .expect("IO protocol factory registry");
    assert!(
        factory.contains("struct ProtocolFactory"),
        "the IO registry must carry runtime factory entries, not metadata alone"
    );
    assert!(
        factory.contains("static PROTOCOL_REGISTRY: LazyLock<ProtocolRegistry>"),
        "the IO protocol composition must remain one immutable process-wide registry"
    );
    assert!(
        !factory.contains("validate_snapshot_mappings"),
        "runtime mappings must be compiled once by the selected adapter builder"
    );

    let creation =
        fs::read_to_string(root.join("services/io/src/core/channels/channel_creation.rs"))
            .expect("channel creation source");
    for retired in [
        "create_channel_by_protocol",
        "create_modbus_channel_impl",
        "create_mqtt_channel_impl",
        "match protocol_name",
    ] {
        assert!(
            !creation.contains(retired),
            "ChannelManager restored protocol dispatch through {retired}"
        );
    }
    let manager = fs::read_to_string(root.join("services/io/src/core/channels/channel_manager.rs"))
        .expect("channel manager source");
    assert!(
        manager.contains("lifecycle_in_progress") && manager.contains("reserve_channel_lifecycle"),
        "channel create/remove must share an atomic per-channel lifecycle reservation"
    );

    let topology = fs::read_to_string(root.join("services/io/src/point_topology.rs"))
        .expect("point topology source");
    assert!(
        !topology.contains("normalize_protocol_name(protocol)"),
        "point topology restored a second protocol mapping dispatch table"
    );
}

#[test]
fn io_runtime_snapshot_and_channel_state_keep_single_ownership() {
    let root = workspace_metadata().workspace_root;
    let manager = fs::read_to_string(root.join("services/io/src/core/channels/channel_manager.rs"))
        .expect("channel manager source");
    for retired in [
        "sqlite_pool",
        "load_channel_configuration",
        "create_data_store",
        "pub fn data_store",
        "CommandTxCache",
    ] {
        assert!(
            !manager.contains(retired),
            "ChannelManager restored duplicate IO ownership through {retired}"
        );
    }

    let creation =
        fs::read_to_string(root.join("services/io/src/core/channels/channel_creation.rs"))
            .expect("channel creation source");
    assert!(
        !creation.contains("Arc<RuntimeChannelConfig>"),
        "runtime snapshots must move by value into channel creation"
    );
    assert!(
        creation.contains("compare_and_swap"),
        "channel runtime publication must remain atomic under concurrent creation"
    );

    let loader = fs::read_to_string(root.join("services/io/src/core/config/sqlite_loader.rs"))
        .expect("SQLite loader source");
    assert!(
        loader.contains("load_runtime_channels")
            && loader.contains("Result<Vec<RuntimeChannelConfig>>"),
        "the SQLite boundary must compile complete runtime snapshots"
    );

    let entry = fs::read_to_string(root.join("services/io/src/core/channels/channel_entry.rs"))
        .expect("channel entry source");
    assert!(
        !entry.contains("channel_config: ChannelConfig")
            && !entry.contains("Arc<RuntimeChannelConfig>")
            && entry.contains("shared: Arc<ChannelSharedState>"),
        "ChannelEntry must retain only runtime identity and one shared atomic state allocation"
    );

    let task = fs::read_to_string(root.join("services/io/src/core/channels/channel_task.rs"))
        .expect("channel task source");
    assert!(
        !task.contains("batch.as_ref().clone()"),
        "the event path must move DataBatch instead of cloning the complete batch"
    );
    let command_types = fs::read_to_string(root.join("services/io/src/core/channels/types.rs"))
        .expect("channel command types");
    for retired in ["GetDiagnostics {", "GetConnectionState {"] {
        assert!(
            !command_types.contains(retired),
            "unused task command surface {retired} must not be restored"
        );
    }

    assert!(
        !root
            .join("services/io/src/api/command_cache.rs")
            .try_exists()
            .expect("command cache path"),
        "the unused command sender cache must not be restored"
    );

    for retired in [
        "services/io/src/protocols/core/quality.rs",
        "services/io/src/protocols/core/slot.rs",
    ] {
        assert!(
            !root
                .join(retired)
                .try_exists()
                .expect("retired IO model path"),
            "{retired} restored a duplicate runtime data model"
        );
    }
    let data = fs::read_to_string(root.join("services/io/src/protocols/core/data.rs"))
        .expect("runtime acquisition data model");
    assert!(
        data.contains("PointQuality")
            && !data.contains("String(String)")
            && !data.contains("Null,"),
        "runtime acquisition batches must remain canonical-quality and numeric-only"
    );
}

#[test]
fn automation_starts_without_an_io_http_liveness_dependency() {
    let root = workspace_metadata().workspace_root;
    let bootstrap = fs::read_to_string(root.join("services/automation/src/bootstrap.rs"))
        .expect("automation bootstrap source");
    for forbidden in ["common::io_url", "wait_for_dependency"] {
        assert!(
            !bootstrap.contains(forbidden),
            "automation must discover IO availability through its typed lazy SHM topology, not {forbidden}"
        );
    }

    let manifest = fs::read_to_string(root.join("services/automation/Cargo.toml"))
        .expect("automation manifest");
    assert!(
        !manifest.contains("\"dependency\""),
        "automation must not retain the HTTP dependency-polling feature"
    );

    let manager = fs::read_to_string(root.join("services/automation/src/instance_manager.rs"))
        .expect("automation instance manager");
    for forbidden in [
        "OnceLock",
        "set_runtime_topology",
        "if let Some(topology)",
        "SELECT (SELECT COUNT(*) FROM measurement_routing",
    ] {
        assert!(
            !manager.contains(forbidden),
            "automation must require one coherent runtime topology instead of restoring {forbidden}"
        );
    }
}

#[test]
fn automation_rule_runtime_and_dead_surface_keep_single_ownership() {
    let root = workspace_metadata().workspace_root;
    let main =
        fs::read_to_string(root.join("services/automation/src/main.rs")).expect("automation main");
    let mutation = fs::read_to_string(root.join("services/automation/src/infra/rule_mutation.rs"))
        .expect("rule mutation adapter");
    let runtime = fs::read_to_string(root.join("services/automation/src/infra/rule_runtime.rs"))
        .expect("rule runtime coordinator");
    for duplicate in [
        "mark_unready(",
        ".rebuild_point_watch(",
        "with_topology_guard",
    ] {
        assert!(
            !main.contains(duplicate) && !mutation.contains(duplicate),
            "rule/PointWatch publication must remain owned by RuleRuntimeCoordinator, not {duplicate}"
        );
    }
    assert!(
        runtime.contains("pub struct RuleRuntimeCoordinator")
            && runtime.contains("reload_gate: Mutex<()>")
            && runtime.contains(".rebuild_point_watch(")
            && runtime.contains("pub fn topology_changes(")
            && !main.contains("runtime_topology.subscribe()"),
        "automation must retain one serialized, typed rule/PointWatch publication coordinator"
    );
    let topology =
        fs::read_to_string(root.join("services/automation/src/infra/runtime_topology.rs"))
            .expect("automation runtime topology");
    assert!(
        topology.contains("pub struct RoutingMutationLease")
            && !topology.contains("ActionRoutingMutationLease")
            && !topology.contains("MeasurementRoutingMutationLease"),
        "logical routing must retain one plane-aware mutation lease"
    );
    let command_dispatch =
        fs::read_to_string(root.join("services/automation/src/infra/application_control.rs"))
            .expect("automation command dispatcher");
    for forbidden in ["sqlx::", "control_points", "adjustment_points"] {
        assert!(
            !command_dispatch.contains(forbidden),
            "command dispatch must use the pinned runtime snapshot instead of restoring {forbidden}"
        );
    }
    assert!(
        command_dispatch.contains(".command_route(")
            && command_dispatch.contains("routed.constraints()"),
        "command dispatch must resolve the target and limits from one immutable generation"
    );

    assert!(
        !root
            .join("services/automation/src/infra/channel_health.rs")
            .try_exists()
            .expect("retired channel-health adapter path"),
        "automation must not restore the unused second channel-health reader"
    );

    let instance_data = fs::read_to_string(root.join("services/automation/src/instance_data.rs"))
        .expect("instance data adapter");
    assert!(
        !instance_data.contains("pub async fn get_instance_points("),
        "InstanceManager must not restore the duplicate JSON point projection"
    );
    let manager = fs::read_to_string(root.join("services/automation/src/instance_manager.rs"))
        .expect("automation instance manager");
    assert!(
        !manager.contains("name_cache")
            && !manager.contains("get_instance_id(")
            && !manager.contains("populate_name_cache("),
        "automation must not rebuild an unused process-local instance-name index"
    );
    let single_point_handlers =
        fs::read_to_string(root.join("services/automation/src/api/single_point_handlers.rs"))
            .expect("single-point handlers");
    assert!(
        !single_point_handlers.contains(".routing_revision()"),
        "point detail and its ETag revision must come from one SQLite snapshot"
    );
    assert!(
        instance_data.contains("routing_revision: i64")
            && instance_data.contains("configuration_revisions cr")
            && instance_data.contains("name: mp.name,")
            && instance_data.contains("name: ap.name,"),
        "single-point reads must load route and revision together and consume product points"
    );
    assert!(
        instance_data.contains("routing_snapshot(RoutingScope::Instance(instance_id))")
            && !instance_data.contains("FROM measurement_routing mr")
            && !instance_data.contains("FROM action_routing ar")
            && !instance_data.contains("m_routing_query")
            && !instance_data.contains("a_routing_query"),
        "instance points must reuse the one typed routing snapshot instead of duplicating SQL"
    );
    let instance_queries =
        fs::read_to_string(root.join("services/automation/src/api/instance_query_handlers.rs"))
            .expect("instance query handlers");
    assert!(
        !instance_queries.contains("InstancePointsResponse still needs it")
            && !instance_queries.contains("instance.instance_name().to_string()"),
        "the points handler must not pre-load the instance before its typed repository query"
    );
    assert!(
        !instance_queries.contains("RawQuery")
            && !instance_queries.contains("product_cache")
            && !instance_queries.contains("parse_ids_param")
            && instance_queries.contains("InstanceSearchResponseDto")
            && instance_queries.contains("InstanceListResponseDto")
            && instance_queries.contains("InstancePickerResponseDto")
            && instance_queries.contains("serde(deny_unknown_fields)")
            && !instance_queries.contains("instance_manager.pool")
            && manager.contains("find_instances")
            && manager.contains("QueryBuilder::<sqlx::Sqlite>"),
        "instance search must remain typed, bounded, and backed by one batch ID query"
    );
    let uplink_mqtt =
        fs::read_to_string(root.join("services/uplink/src/mqtt.rs")).expect("uplink MQTT adapter");
    let automation_client =
        fs::read_to_string(root.join("services/uplink/src/automation_client.rs"))
            .expect("uplink Automation client");
    assert!(
        automation_client.contains("decode_instance_list")
            && automation_client.contains("MAX_INSTANCE_RESPONSE_BYTES")
            && automation_client.contains("read_bounded_response")
            && !uplink_mqtt.contains("reqwest::")
            && !uplink_mqtt.contains("AutomationInstanceListResponse")
            && !uplink_mqtt.contains("AutomationActionBody"),
        "uplink must keep bounded Automation HTTP decoding behind its typed loopback client"
    );
    for internal_source in [
        "services/automation/src/instance_data.rs",
        "services/automation/src/instance_configuration.rs",
    ] {
        let source = fs::read_to_string(root.join(internal_source))
            .expect("automation internal read-model source");
        assert!(
            !source.contains("crate::dto") && !source.contains("api::dto"),
            "{internal_source} must return internal read models, not HTTP/OpenAPI DTOs"
        );
    }
    let instance_query = fs::read_to_string(root.join("services/automation/src/instance_query.rs"))
        .expect("automation internal instance read models");
    for wire_surface in ["serde::", "Serialize", "Deserialize", "ToSchema", "axum::"] {
        assert!(
            !instance_query.contains(wire_surface),
            "internal instance read models must not depend on wire surface {wire_surface}"
        );
    }
    let automation_lib =
        fs::read_to_string(root.join("services/automation/src/lib.rs")).expect("automation lib");
    assert!(
        !automation_lib.contains("\npub mod dto;")
            && automation_lib.contains("pub mod api")
            && automation_lib.contains("    pub mod dto;"),
        "HTTP DTOs must remain namespaced under api instead of being re-exported at crate root"
    );
    let application_control =
        fs::read_to_string(root.join("services/automation/src/infra/application_control.rs"))
            .expect("automation application control");
    for transport_surface in [
        "axum::",
        "HeaderMap",
        "command_invocation_from_headers",
        "completion_audit_response",
    ] {
        assert!(
            !application_control.contains(transport_surface),
            "application control must not depend on HTTP surface {transport_surface}"
        );
    }
    let http_boundary =
        fs::read_to_string(root.join("services/automation/src/api/http_boundary.rs"))
            .expect("automation HTTP boundary");
    assert!(
        http_boundary.contains("command_invocation_from_headers")
            && http_boundary.contains("completion_audit_response"),
        "HTTP header extraction and response projection must remain in the API adapter"
    );
    let automation_error = fs::read_to_string(root.join("services/automation/src/error.rs"))
        .expect("automation errors");
    for wire_surface in [
        "axum::",
        "common::AppError",
        "AetherErrorTrait",
        "http_status(",
    ] {
        assert!(
            !automation_error.contains(wire_surface),
            "transport-neutral AutomationError must not depend on {wire_surface}"
        );
    }
    assert!(
        !root
            .join("services/automation/src/rule_routes.rs")
            .try_exists()
            .expect("legacy root rule routes")
            && root
                .join("services/automation/src/api/rule_routes.rs")
                .try_exists()
                .expect("HTTP rule routes"),
        "rule HTTP handlers must remain inside the API adapter"
    );
    let automation_src = root.join("services/automation/src");
    for path in rust_files_under(&automation_src) {
        let relative = path
            .strip_prefix(&automation_src)
            .expect("automation source path");
        if relative.starts_with("api")
            || relative == Path::new("main.rs")
            || relative == Path::new("routes.rs")
        {
            continue;
        }
        let source = fs::read_to_string(&path).expect("automation internal source");
        for wire_surface in [
            "api::dto",
            "axum::",
            "utoipa",
            "HeaderMap",
            "SuccessResponse",
            "IntoResponse",
            "common::AppError",
        ] {
            assert!(
                !source.contains(wire_surface),
                "{} must not depend on HTTP surface {wire_surface}",
                relative.display()
            );
        }
    }
    let app_state = fs::read_to_string(root.join("services/automation/src/app_state.rs"))
        .expect("automation app state");
    for forwarding_method in [
        "pub async fn get_instance_id(",
        "pub async fn populate_name_cache(",
        "pub fn update_name_cache(",
    ] {
        assert!(
            !app_state.contains(forwarding_method),
            "AppState must not restore unused forwarding method {forwarding_method}"
        );
    }

    let dto = fs::read_to_string(root.join("services/automation/src/api/dto.rs"))
        .expect("automation DTOs");
    assert!(
        dto.contains("pub logical_routing_revision: u64"),
        "instance points must expose the shared route CAS head"
    );
    let rule_routes = fs::read_to_string(root.join("services/automation/src/api/rule_routes.rs"))
        .expect("automation rule routes");
    for retired in [
        "pub struct RoutingRequest",
        "RoutingUpdate",
        "RoutingType",
        "RoutingValidationRequest",
        "SinglePointRoutingRequest",
        "ToggleRoutingRequest",
        "RoutingMutationConfirmation",
        "InstanceResult",
        "pub struct InstanceDetail {",
        "RuleVariableSchema",
        "PeriodDeltaNodeSchema",
        "VueFlowPeriodDeltaNode",
        "PeriodDeltaConfigSchema",
    ] {
        assert!(
            !dto.contains(retired) && !rule_routes.contains(retired),
            "automation restored unused DTO {retired}"
        );
    }

    let routes = fs::read_to_string(root.join("services/automation/src/routes.rs"))
        .expect("automation routes");
    for retired_route in [
        ".route(\"/api/instances/reload\"",
        ".route(\"/api/instances/{id}/routing/validate\"",
        ".post(create_instance_routing)",
        ".put(update_instance_routing)",
        ".delete(delete_instance_routing)",
        ".route(\"/api/routing/instances/",
        ".route(\"/api/routing/channels/",
    ] {
        assert!(
            !routes.contains(retired_route),
            "automation restored ambiguous or ungoverned mutation route {retired_route}"
        );
    }
    assert!(
        !root
            .join("services/automation/src/reload.rs")
            .try_exists()
            .expect("retired automation reload adapter path"),
        "automation must not restore the unused generic reload adapter"
    );
    assert!(
        !root
            .join("services/automation/src/api/routing_management_handlers.rs")
            .try_exists()
            .expect("retired routing validation handler path"),
        "automation must not restore the inaccurate duplicate routing validator"
    );
    let common_service_config = fs::read_to_string(root.join("libs/common/src/service_config.rs"))
        .expect("common service configuration");
    assert!(
        !common_service_config.contains("trait ReloadableService")
            && !common_service_config.contains("struct ReloadResult"),
        "common must not restore service-specific reload ownership"
    );

    let routing_cli =
        fs::read_to_string(root.join("tools/aether/src/routing.rs")).expect("routing CLI");
    for retired_command in [
        "RoutingCommands::Create",
        "RoutingCommands::Batch",
        "RoutingCommands::DeleteInstance",
        "RoutingCommands::DeleteChannel",
    ] {
        assert!(
            !routing_cli.contains(retired_command),
            "CLI restored broken routing compatibility command {retired_command}"
        );
    }
    assert!(
        routing_cli.contains("\"expected_revision\": expected_revision"),
        "CLI routing mutations must keep the shared CAS revision in their wire bodies"
    );

    for handler in [
        "services/automation/src/api/global_routing_handlers.rs",
        "services/automation/src/api/routing_query_handlers.rs",
        "services/automation/src/api/single_point_handlers.rs",
    ] {
        let source = fs::read_to_string(root.join(handler)).expect("routing HTTP adapter");
        assert!(
            !source.contains("sqlx::"),
            "{handler} must use the single typed routing query boundary"
        );
    }
    let routing_loader = fs::read_to_string(root.join("services/automation/src/routing_loader.rs"))
        .expect("routing snapshot loader");
    assert!(
        routing_loader.contains("pub(crate) struct RoutingSnapshot")
            && routing_loader.contains("UNION ALL")
            && routing_loader.contains("transaction.commit().await?"),
        "routing queries must retain one revision-consistent SQLite snapshot loader"
    );
    let instance_routing =
        fs::read_to_string(root.join("services/automation/src/instance_routing.rs"))
            .expect("instance routing boundary");
    for retired_query in ["get_measurement_routing", "get_action_routing"] {
        assert!(
            !instance_routing.contains(retired_query),
            "automation restored split routing query {retired_query}"
        );
    }

    let product_loader = fs::read_to_string(root.join("services/automation/src/product_loader.rs"))
        .expect("automation product loader");
    for retired in [
        "pub fn get_all_products(",
        "pub fn get_product_hierarchy(",
        "pub fn product_exists(",
    ] {
        assert!(
            !product_loader.contains(retired),
            "ProductLoader must not restore test-only cloning surface {retired}"
        );
    }
    assert!(
        product_loader.contains(
            "pub(crate) fn get_definition(&self, product_name: &str) -> Result<&ProductDefinition>",
        ) && product_loader.contains("property_id: i32::try_from(point.id)"),
        "internal product validation must borrow Pack authority and narrow property IDs strictly"
    );
    for validator in [
        "services/automation/src/instance_manager.rs",
        "services/automation/src/instance_configuration.rs",
        "services/automation/src/infra/measurement_routing.rs",
        "services/automation/src/infra/action_routing.rs",
    ] {
        let source = fs::read_to_string(root.join(validator)).expect("product validator source");
        assert!(
            !source.contains(".get_product(") && source.contains(".get_definition("),
            "{validator} must not deep-convert an entire Product DTO for validation"
        );
    }
}

#[test]
fn automation_storage_and_rule_http_keep_explicit_ownership() {
    let root = workspace_metadata().workspace_root;
    let manager = fs::read_to_string(root.join("services/automation/src/instance_manager.rs"))
        .expect("automation instance manager");
    assert!(
        !manager.contains("pub fn pool("),
        "InstanceManager must not expose SQLite as a service locator"
    );

    let main =
        fs::read_to_string(root.join("services/automation/src/main.rs")).expect("automation main");
    assert!(
        !main.contains("instance_manager.pool()"),
        "the composition root must receive SQLite explicitly instead of borrowing it from InstanceManager"
    );
    assert!(
        !main.contains("instance_manager.runtime_topology()"),
        "the composition root must receive runtime topology explicitly instead of locating it through InstanceManager"
    );

    for adapter in [
        "services/automation/src/instance_configuration.rs",
        "services/automation/src/infra/action_routing.rs",
        "services/automation/src/infra/measurement_routing.rs",
    ] {
        let source = fs::read_to_string(root.join(adapter)).expect("automation SQLite adapter");
        assert!(
            !source.contains("manager.pool()"),
            "{adapter} must own its SQLite handle explicitly"
        );
    }

    let rule_routes = fs::read_to_string(root.join("services/automation/src/api/rule_routes.rs"))
        .expect("automation rule HTTP adapter");
    for forbidden in ["sqlx::", "SqlitePool", "RuleScheduler", "StateStore"] {
        assert!(
            !rule_routes.contains(forbidden),
            "rule HTTP adapter must use internal rule queries instead of owning {forbidden}"
        );
    }
    assert!(
        !rule_routes.contains("application: Option<"),
        "mounted rule routes must require complete application composition"
    );

    let lib = fs::read_to_string(root.join("services/automation/src/lib.rs"))
        .expect("read automation lib");
    assert!(
        !lib.contains("pub use aether_rules"),
        "the Automation service crate must not republish the rule library surface"
    );
    assert!(
        !lib.contains("pub mod runtime {"),
        "empty compatibility modules must not return to Automation"
    );
}

#[test]
fn service_internal_models_do_not_depend_on_http_dtos() {
    let root = workspace_metadata().workspace_root;

    for relative in [
        "services/history/src/models.rs",
        "services/history/src/storage.rs",
        "services/history/src/collector.rs",
        "services/alarm/src/models.rs",
        "services/alarm/src/db.rs",
        "services/alarm/src/monitor.rs",
        "services/alarm/src/notification.rs",
        "services/uplink/src/config_model.rs",
        "services/uplink/src/db_config.rs",
        "services/uplink/src/state.rs",
        "services/uplink/src/system_monitor.rs",
        "services/uplink/src/models.rs",
        "services/uplink/src/mqtt.rs",
        "services/api/src/read_models.rs",
        "services/api/src/db.rs",
        "services/api/src/auth.rs",
    ] {
        let source = fs::read_to_string(root.join(relative)).expect("service internal source");
        for forbidden in ["utoipa", "ToSchema", "IntoParams", "api::dto"] {
            assert!(
                !source.contains(forbidden),
                "{relative} must not depend on HTTP/OpenAPI surface {forbidden}"
            );
        }
    }

    for relative in [
        "services/api/src/read_models.rs",
        "services/api/src/db.rs",
        "services/api/src/auth.rs",
    ] {
        let source = fs::read_to_string(root.join(relative)).expect("API internal source");
        for forbidden in ["axum::", "crate::models"] {
            assert!(
                !source.contains(forbidden),
                "{relative} must not depend on HTTP DTO surface {forbidden}"
            );
        }
    }

    let uplink_mqtt =
        fs::read_to_string(root.join("services/uplink/src/mqtt.rs")).expect("uplink MQTT source");
    for forbidden in [
        "reqwest::",
        ".http_client",
        "AutomationInstanceListResponse",
        "AutomationActionBody",
        "AlarmReplayRequest",
    ] {
        assert!(
            !uplink_mqtt.contains(forbidden),
            "MQTT session ownership must not include loopback HTTP surface {forbidden}"
        );
    }

    let io_error =
        fs::read_to_string(root.join("services/io/src/error.rs")).expect("IO internal errors");
    let io_error = io_error
        .split("#[cfg(test)]")
        .next()
        .expect("IO production error source");
    for forbidden in [
        "AetherErrorTrait",
        "common::AppError",
        "status_for_error_category",
    ] {
        assert!(
            !io_error.contains(forbidden),
            "IO internal errors must not depend on HTTP projection {forbidden}"
        );
    }
    let io_lib = fs::read_to_string(root.join("services/io/src/lib.rs")).expect("IO library root");
    assert!(
        !io_lib.contains("pub use crate::api::dto") && !io_lib.contains("pub use api::dto"),
        "IO HTTP DTOs must not be publicly re-exported from the crate root"
    );

    let errors_manifest =
        fs::read_to_string(root.join("libs/errors/Cargo.toml")).expect("errors manifest");
    let errors_source = fs::read_to_string(root.join("libs/errors/src/lib.rs"))
        .expect("transport-neutral errors source");
    for forbidden in ["axum", "http_status", "into_http_response"] {
        assert!(
            !errors_manifest.contains(forbidden) && !errors_source.contains(forbidden),
            "shared errors must remain transport-neutral and not restore {forbidden}"
        );
    }
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

fn rust_files_under(directory: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in fs::read_dir(current).expect("source directory") {
            let path = entry.expect("source entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                files.push(path);
            }
        }
    }
    files
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

#[derive(Default)]
struct StoredCodecMethodUsage {
    decode: bool,
    from_value: bool,
    encode: bool,
    direct_json_parse: bool,
    direct_payload_field_access: bool,
}

impl<'ast> Visit<'ast> for StoredCodecMethodUsage {
    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let Expr::Path(path) = &*node.func {
            let segments = path
                .path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect::<Vec<_>>();
            match segments.as_slice() {
                [.., owner, method] if owner == "StoredChannelConfig" && method == "decode" => {
                    self.decode = true;
                },
                [.., owner, method] if owner == "StoredChannelConfig" && method == "from_value" => {
                    self.from_value = true;
                },
                [.., owner, method]
                    if owner == "serde_json"
                        && matches!(method.as_str(), "from_str" | "from_value") =>
                {
                    self.direct_json_parse = true;
                },
                _ => {},
            }
        }
        visit::visit_expr_call(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        if node.method == "encode" {
            self.encode = true;
        }
        if matches!(node.method.to_string().as_str(), "get" | "remove")
            && let Some(Expr::Lit(argument)) = node.args.first()
            && let Lit::Str(field) = &argument.lit
            && matches!(
                field.value().as_str(),
                "description" | "parameters" | "logging"
            )
        {
            self.direct_payload_field_access = true;
        }
        visit::visit_expr_method_call(self, node);
    }
}

fn inspect_named_method(source: &str, method_name: &str) -> StoredCodecMethodUsage {
    let syntax = syn::parse_file(source)
        .unwrap_or_else(|error| panic!("failed to parse method source: {error}"));
    for item in syntax.items {
        let Item::Impl(item_impl) = item else {
            continue;
        };
        for item in item_impl.items {
            let syn::ImplItem::Fn(method) = item else {
                continue;
            };
            if method.sig.ident == method_name {
                let mut usage = StoredCodecMethodUsage::default();
                usage.visit_block(&method.block);
                return usage;
            }
        }
    }
    panic!("method {method_name} was not found");
}
