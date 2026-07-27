use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use serde::Deserialize;

const RETIRED_PACKAGES: &[&str] = &[
    "aether-home-assistant-bridge",
    "aether-http-history-query",
    "aether-infra",
    "aether-integration-control",
    "aether-model",
    "aether-postgres-history",
    "aether-redis-bridge",
    "aether-rtdb",
    "aether-rtdb-shm",
    "aether-sunspec",
];
const RETIRED_ROOT_PATHS: &[&str] = &[
    "libs/aether-infra",
    "libs/aether-model",
    "libs/aether-rtdb",
    "libs/aether-rtdb-shm",
    "libs/aether-shm",
    "libs/aether-infra/src/redis.rs",
    "libs/common/src/warning_monitor.rs",
    "services/io/assets/script-host",
    "services/io/src/api/handlers/network_handlers.rs",
    "services/io/src/protocols/adapters/virtual_channel.rs",
    "services/io/src/protocols/core/script_runner.rs",
    "services/io/src/protocols/sunspec/expand.rs",
    "services/io/src/protocols/sunspec/model.rs",
    "services/io/src/protocols/sunspec/models",
    "services/io/src/protocols/sunspec/types.rs",
];
const RETIRED_EXTERNAL_RUST_DEPENDENCIES: &[&str] = &["bb8", "bb8-redis", "redis"];
const CORE_INFRASTRUCTURE_DEPENDENCIES: &[&str] = &[
    "aether-http-data-processor",
    "aether-http-history-query",
    "aether-sqlite-history-query",
    "axum",
    "bb8",
    "bb8-redis",
    "redis",
    "reqwest",
    "rumqttc",
    "sqlx",
    "workspace-hack",
];
#[derive(Debug, Deserialize)]
struct MetadataDocument {
    packages: Vec<Package>,
    workspace_members: Vec<String>,
    workspace_root: PathBuf,
}

#[derive(Debug, Deserialize)]
struct Package {
    id: String,
    name: String,
    manifest_path: PathBuf,
    dependencies: Vec<Dependency>,
    features: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct Dependency {
    name: String,
    kind: Option<String>,
    optional: bool,
    path: Option<PathBuf>,
}

struct Workspace {
    metadata: MetadataDocument,
    member_ids: BTreeSet<String>,
}

impl Workspace {
    fn load() -> Self {
        let metadata = metadata_for_manifest(Path::new(env!("CARGO_MANIFEST_DIR")));
        let member_ids = metadata.workspace_members.iter().cloned().collect();
        Self {
            metadata,
            member_ids,
        }
    }

    fn root(&self) -> &Path {
        &self.metadata.workspace_root
    }

    fn members(&self) -> impl Iterator<Item = &Package> {
        self.metadata
            .packages
            .iter()
            .filter(|package| self.member_ids.contains(&package.id))
    }

    fn package(&self, name: &str) -> &Package {
        self.members()
            .find(|package| package.name == name)
            .unwrap_or_else(|| panic!("workspace package {name:?} is missing"))
    }

    fn relative_manifest<'a>(&self, package: &'a Package) -> &'a Path {
        package
            .manifest_path
            .strip_prefix(self.root())
            .unwrap_or_else(|_| {
                panic!(
                    "workspace package {} is outside workspace root {}",
                    package.name,
                    self.root().display()
                )
            })
    }

    fn package_is_under(&self, package: &Package, owner: &str) -> bool {
        self.relative_manifest(package).starts_with(owner)
    }

    fn workspace_dependency<'a>(&'a self, dependency: &Dependency) -> Option<&'a Package> {
        let dependency_path = dependency.path.as_deref()?;
        self.members()
            .find(|package| package.manifest_path.parent() == Some(dependency_path))
    }

    fn production_workspace_dependencies<'a>(
        &'a self,
        package: &'a Package,
    ) -> impl Iterator<Item = (&'a Dependency, &'a Package)> + 'a {
        production_dependencies(package).filter_map(|dependency| {
            self.workspace_dependency(dependency)
                .map(|target| (dependency, target))
        })
    }
}

fn workspace() -> &'static Workspace {
    static WORKSPACE: OnceLock<Workspace> = OnceLock::new();
    WORKSPACE.get_or_init(Workspace::load)
}

fn production_dependencies(package: &Package) -> impl Iterator<Item = &Dependency> {
    package
        .dependencies
        .iter()
        .filter(|dependency| dependency.kind.as_deref() != Some("dev"))
}

fn metadata_for_manifest(manifest_owner: &Path) -> MetadataDocument {
    let manifest = if manifest_owner.is_dir() {
        manifest_owner.join("Cargo.toml")
    } else {
        manifest_owner.to_path_buf()
    };
    let mut command = Command::new(env!("CARGO"));
    command.args([
        "metadata",
        "--locked",
        "--format-version",
        "1",
        "--manifest-path",
    ]);
    command.arg(&manifest).arg("--no-deps");

    let output = command
        .output()
        .unwrap_or_else(|error| panic!("failed to execute cargo metadata: {error}"));
    assert!(
        output.status.success(),
        "cargo metadata failed for {}:\n{}",
        manifest.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "cargo metadata returned invalid JSON for {}: {error}",
            manifest.display()
        )
    })
}

fn assert_no_violations(contract: &str, mut violations: Vec<String>) {
    if violations.is_empty() {
        return;
    }
    violations.sort();
    panic!("{contract}:\n- {}", violations.join("\n- "));
}

fn files_with_extension(root: &Path, extension: &str) -> Vec<PathBuf> {
    let mut matches = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)
            .unwrap_or_else(|error| panic!("failed to inspect {}: {error}", directory.display()))
        {
            let entry = entry.unwrap_or_else(|error| {
                panic!(
                    "failed to inspect entry below {}: {error}",
                    directory.display()
                )
            });
            let path = entry.path();
            let file_type = entry.file_type().unwrap_or_else(|error| {
                panic!(
                    "failed to inspect file type for {}: {error}",
                    path.display()
                )
            });
            if file_type.is_dir() {
                pending.push(path);
            } else if path.extension().and_then(|value| value.to_str()) == Some(extension) {
                matches.push(path);
            }
        }
    }
    matches.sort();
    matches
}

fn has_production_workspace_dependency(
    workspace: &Workspace,
    source: &Package,
    target_name: &str,
) -> bool {
    workspace
        .production_workspace_dependencies(source)
        .any(|(_, target)| target.name == target_name)
}

fn production_workspace_dependency_closure<'a>(
    workspace: &'a Workspace,
    root: &'a Package,
) -> BTreeSet<&'a str> {
    let mut visited = BTreeSet::new();
    let mut pending = vec![root];

    while let Some(package) = pending.pop() {
        if !visited.insert(package.name.as_str()) {
            continue;
        }
        pending.extend(
            workspace
                .production_workspace_dependencies(package)
                .map(|(_, dependency)| dependency),
        );
    }

    visited
}

#[test]
fn canonical_layers_only_depend_inward() {
    let workspace = workspace();
    let contracts: &[(&str, &[&str])] = &[
        ("aether-domain", &[]),
        ("aether-ports", &["aether-domain"]),
        ("aether-data-processing", &["aether-domain"]),
        (
            "aether-acquisition-port",
            &["aether-domain", "aether-ports"],
        ),
        (
            "aether-application",
            &["aether-data-processing", "aether-domain", "aether-ports"],
        ),
    ];
    let mut violations = Vec::new();

    for (package_name, allowed_dependencies) in contracts {
        let package = workspace.package(package_name);
        for (_, dependency) in workspace.production_workspace_dependencies(package) {
            if !allowed_dependencies.contains(&dependency.name.as_str()) {
                violations.push(format!(
                    "{package_name} depends outward on {} ({})",
                    dependency.name,
                    workspace.relative_manifest(dependency).display()
                ));
            }
        }
    }

    assert_no_violations("canonical dependency direction changed", violations);
}

#[test]
fn core_crates_do_not_select_infrastructure_or_runtime_implementations() {
    let workspace = workspace();
    let forbidden: BTreeSet<_> = CORE_INFRASTRUCTURE_DEPENDENCIES.iter().copied().collect();
    let mut violations = Vec::new();

    for package in workspace
        .members()
        .filter(|package| workspace.package_is_under(package, "crates"))
    {
        for dependency in production_dependencies(package) {
            if forbidden.contains(dependency.name.as_str()) {
                violations.push(format!(
                    "{} declares forbidden infrastructure dependency {}",
                    package.name, dependency.name
                ));
            }
        }

        for (declaration, dependency) in workspace.production_workspace_dependencies(package) {
            let runtime_owned = ["extensions", "services", "tools", "examples"]
                .iter()
                .any(|owner| workspace.package_is_under(dependency, owner));
            if !runtime_owned {
                continue;
            }

            // ADR-0013 retains this optional SDK composition facade. Keeping the
            // exception explicit prevents it from becoming a general core ->
            // extension dependency allowance.
            let sdk_local_runtime_compatibility = package.name == "aether-edge-sdk"
                && dependency.name == "aether-store-local"
                && declaration.optional;
            if !sdk_local_runtime_compatibility {
                violations.push(format!(
                    "{} depends on runtime-owned package {} ({})",
                    package.name,
                    dependency.name,
                    workspace.relative_manifest(dependency).display()
                ));
            }
        }
    }

    assert_no_violations(
        "core crates must remain independent of infrastructure and runtime implementations",
        violations,
    );
}

#[test]
fn kernel_workspace_has_no_redis_client_dependency() {
    let workspace = workspace();
    let forbidden: BTreeSet<_> = RETIRED_EXTERNAL_RUST_DEPENDENCIES.iter().copied().collect();
    let mut violations = Vec::new();

    for package in workspace.members() {
        for dependency in &package.dependencies {
            if forbidden.contains(dependency.name.as_str()) {
                violations.push(format!(
                    "{} declares retired external client dependency {}",
                    package.name, dependency.name
                ));
            }
        }
    }

    assert_no_violations(
        "Redis clients belong in downstream integrations, not the edge kernel workspace",
        violations,
    );
}

#[test]
fn retired_workspace_crates_stay_retired() {
    let workspace = workspace();
    let member_names: BTreeSet<_> = workspace
        .members()
        .map(|package| package.name.as_str())
        .collect();
    let mut violations = Vec::new();

    for retired in RETIRED_PACKAGES {
        if member_names.contains(retired) {
            violations.push(format!("retired package {retired} is a workspace member"));
        }
        for owner in workspace.members() {
            if owner
                .dependencies
                .iter()
                .any(|dependency| dependency.name == *retired)
            {
                violations.push(format!(
                    "{} declares a dependency on retired package {retired}",
                    owner.name
                ));
            }
        }
    }
    for retired_path in RETIRED_ROOT_PATHS {
        if workspace.root().join(retired_path).exists() {
            violations.push(format!(
                "retired root-workspace path {retired_path} was restored"
            ));
        }
    }

    assert_no_violations("retired workspace crates were restored", violations);
}

#[test]
fn production_io_is_rust_only_and_in_tree_sunspec_is_retired() {
    let workspace = workspace();
    let io = workspace.package("aether-io");
    let member_names = workspace
        .members()
        .map(|package| package.name.as_str())
        .collect::<BTreeSet<_>>();

    assert!(!member_names.contains("aether-sunspec"));
    assert!(
        production_dependencies(io).all(|dependency| dependency.name != "aether-sunspec"),
        "standard aether-io must not depend on a SunSpec implementation"
    );
    assert!(
        !io.features.contains_key("sunspec"),
        "standard aether-io must not expose an in-tree SunSpec feature"
    );
    assert!(
        !workspace.root().join("extensions/sunspec").exists(),
        "the retired in-tree SunSpec catalog was restored"
    );

    let python_files = files_with_extension(&workspace.root().join("services/io"), "py");
    assert!(
        python_files.is_empty(),
        "production IO contains Python assets: {python_files:?}"
    );
}

#[test]
fn kernel_has_no_in_tree_extension_layer_and_uplink_owns_cloudlink_transport() {
    let workspace = workspace();
    assert!(
        !workspace.root().join("extensions").exists(),
        "the retired in-tree extension layer was restored"
    );
    assert!(
        workspace
            .members()
            .all(|package| !workspace.package_is_under(package, "extensions")),
        "a workspace package is still owned by the retired extension layer"
    );

    let cloudlink_mqtt = workspace.package("aether-cloudlink-mqtt");
    assert!(
        workspace.package_is_under(cloudlink_mqtt, "services/uplink"),
        "CloudLink MQTT must be owned by aether-uplink"
    );
    let io = workspace.package("aether-io");
    for dependency in [
        "aether-cloudlink",
        "aether-cloudlink-mqtt",
        "aether-home-assistant-bridge",
        "aether-integration-control",
    ] {
        assert!(
            !has_production_workspace_dependency(workspace, io, dependency),
            "aether-io still selects extracted integration dependency {dependency}"
        );
    }
}

#[test]
fn acquisition_writer_dependency_has_exactly_one_runtime_owner() {
    let workspace = workspace();
    let owners: BTreeSet<_> = workspace
        .members()
        .filter(|package| {
            production_dependencies(package)
                .any(|dependency| dependency.name == "aether-acquisition-port")
        })
        .map(|package| package.name.as_str())
        .collect();
    let expected = BTreeSet::from(["aether-shm-bridge"]);

    assert_eq!(
        owners, expected,
        "only the SHM acquisition adapter may depend on the acquisition writer capability"
    );
}

#[test]
fn swagger_ui_has_exactly_one_package_owner() {
    let workspace = workspace();
    let owners: BTreeSet<_> = workspace
        .members()
        .filter(|package| {
            production_dependencies(package)
                .any(|dependency| dependency.name == "utoipa-swagger-ui")
        })
        .map(|package| package.name.as_str())
        .collect();
    let expected = BTreeSet::from(["aether-api"]);

    assert_eq!(
        owners, expected,
        "Swagger UI must be owned only by the authenticated aether-api gateway"
    );
}

#[test]
fn core_and_gateway_do_not_select_forbidden_sdk_or_adapter_edges() {
    let workspace = workspace();
    let mut violations = Vec::new();

    for package in workspace.members() {
        for dependency in production_dependencies(package) {
            if dependency.name == "opentelemetry" || dependency.name.starts_with("opentelemetry-") {
                violations.push(format!(
                    "{} depends on forbidden OpenTelemetry SDK package {}",
                    package.name, dependency.name
                ));
            }
        }
    }

    let rules = workspace.package("aether-rules");
    if has_production_workspace_dependency(workspace, rules, "aether-routing") {
        violations.push("aether-rules depends on the mutable aether-routing cache".to_string());
    }
    if has_production_workspace_dependency(workspace, rules, "aether-shm-bridge") {
        violations.push("aether-rules selects the concrete SHM runtime".to_string());
    }
    let local_store = workspace.package("aether-store-local");
    if has_production_workspace_dependency(workspace, local_store, "aether-shm-bridge") {
        violations.push("aether-store-local selects the concrete SHM runtime".to_string());
    }
    let sqlite_topology = workspace.package("aether-sqlite-topology");
    if !has_production_workspace_dependency(workspace, sqlite_topology, "aether-shm-bridge") {
        violations
            .push("aether-sqlite-topology no longer owns SQLite-to-SHM composition".to_string());
    }

    let codec = workspace.package("aether-data-processing");
    if has_production_workspace_dependency(workspace, codec, "aether-http-data-processor") {
        violations.push(
            "aether-data-processing depends outward on aether-http-data-processor".to_string(),
        );
    }
    let http_adapter = workspace.package("aether-http-data-processor");
    if !has_production_workspace_dependency(workspace, http_adapter, "aether-data-processing") {
        violations.push(
            "aether-http-data-processor no longer composes aether-data-processing".to_string(),
        );
    }

    assert_no_violations("SDK and adapter dependency boundaries changed", violations);
}

#[test]
fn example_packages_preserve_composition_direction() {
    let workspace = workspace();
    let minimal = workspace.package("aether-example-minimal-gateway");
    let energy = workspace.package("aether-example-energy-gateway");
    let mut violations = Vec::new();

    if !has_production_workspace_dependency(workspace, minimal, "aether-edge-sdk") {
        violations.push("minimal gateway no longer composes aether-edge-sdk".to_string());
    }
    let minimal_graph = production_workspace_dependency_closure(workspace, minimal);
    if minimal_graph.contains("aether-example-energy-gateway") {
        violations
            .push("minimal gateway transitively depends on the energy composition".to_string());
    }
    if !has_production_workspace_dependency(workspace, energy, "aether-example-minimal-gateway") {
        violations.push("energy gateway no longer extends the minimal composition".to_string());
    }
    let energy_graph = production_workspace_dependency_closure(workspace, energy);
    if !energy_graph.contains("aether-edge-sdk") {
        violations.push("energy gateway no longer composes aether-edge-sdk".to_string());
    }

    assert_no_violations("example composition direction changed", violations);
}
