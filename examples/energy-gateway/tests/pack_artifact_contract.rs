use std::fs;
use std::path::{Path, PathBuf};

use aether_sdk::pack::{PackRuntime, load_active_packs, load_pack_manifest};

const EXPECTED_MODELS: [&str; 13] = [
    "Battery.json",
    "Diesel.json",
    "ESS.json",
    "EVChargingLoad.json",
    "Env.json",
    "Generator.json",
    "HVACLoad.json",
    "Load.json",
    "Load_Three_Phase.json",
    "PCS.json",
    "PVInverter.json",
    "PV_DCDC.json",
    "Station.json",
];
const EXPECTED_KNOWLEDGE: [&str; 5] = [
    "control-strategies.md",
    "ess-primer.md",
    "power-forecasting.md",
    "product-models.md",
    "safe-operations.md",
];
const EXPECTED_MAPPINGS: [&str; 3] = [
    "energy.example-instance-channel-bindings",
    "energy.legacy-instance-properties-v5",
    "energy.product-name-aliases",
];
const EXPECTED_RULES: [&str; 1] = ["energy.battery-soc-management"];
const EXPECTED_EVALUATIONS: [&str; 1] = ["energy.pack-safety"];
const EXPECTED_DATA_PROCESSING_TASKS: [&str; 2] =
    ["energy.site-load-forecast", "energy.site-pv-forecast"];

fn repository_pack_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packs/energy")
}

fn runtime() -> PackRuntime {
    aether_runtime_catalog::KernelRuntimeManifest::from_io_features(
        env!("CARGO_PKG_VERSION"),
        "aarch64-unknown-linux-musl",
        ["can", "gpio", "http", "modbus", "mqtt"],
    )
    .and_then(|manifest| manifest.pack_runtime())
    .expect("explicit Energy Pack artifact test composition")
}

fn file_names(directory: &Path, extension: &str) -> Vec<String> {
    let mut names = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", directory.display()))
        .map(|entry| entry.expect("asset directory entry").path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some(extension))
        .map(|path| {
            path.file_name()
                .and_then(|value| value.to_str())
                .expect("UTF-8 asset filename")
                .to_string()
        })
        .collect::<Vec<_>>();
    names.sort_unstable();
    names
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).expect("create isolated pack directory");
    for entry in fs::read_dir(source).expect("read source pack") {
        let entry = entry.expect("source pack entry");
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if source_path.is_dir() {
            copy_tree(&source_path, &destination_path);
        } else {
            fs::copy(&source_path, &destination_path).expect("copy isolated pack asset");
        }
    }
}

fn yaml_value(path: &Path) -> serde_json::Value {
    serde_yml::from_slice(
        &fs::read(path).unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display())),
    )
    .unwrap_or_else(|error| panic!("cannot parse {}: {error}", path.display()))
}

#[test]
fn energy_pack_declares_complete_formal_asset_directories() {
    let root = repository_pack_root();
    let manifest = load_pack_manifest(&root, &runtime()).expect("repository energy pack loads");
    let models = manifest
        .asset_directory("models")
        .expect("models must be a declared Pack v1 asset");
    let knowledge = manifest
        .asset_directory("knowledge")
        .expect("knowledge must be a declared Pack v1 asset");

    assert_eq!(file_names(&root.join(models), "json"), EXPECTED_MODELS);
    assert_eq!(file_names(&root.join(knowledge), "md"), EXPECTED_KNOWLEDGE);
    let actual_model_names = EXPECTED_MODELS
        .iter()
        .map(|model| {
            let bytes = fs::read(root.join(models).join(model)).expect("read model JSON");
            let value = serde_json::from_slice::<serde_json::Value>(&bytes)
                .expect("model asset must be JSON");
            value["name"]
                .as_str()
                .expect("model name must be a string")
                .to_string()
        })
        .collect::<std::collections::BTreeSet<_>>();
    let declared_model_names = manifest
        .capability_ids("models")
        .expect("model capabilities")
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(declared_model_names, actual_model_names);
    for page in EXPECTED_KNOWLEDGE {
        let body =
            fs::read_to_string(root.join(knowledge).join(page)).expect("read knowledge page");
        assert!(body.starts_with("---\n"), "{page} must retain frontmatter");
    }

    for (category, expected) in [
        ("mappings", EXPECTED_MAPPINGS.as_slice()),
        ("rules", EXPECTED_RULES.as_slice()),
        ("evaluations", EXPECTED_EVALUATIONS.as_slice()),
        ("data_processing", EXPECTED_DATA_PROCESSING_TASKS.as_slice()),
    ] {
        let directory = manifest
            .asset_directory(category)
            .unwrap_or_else(|| panic!("{category} must be declared in pack.yaml"));
        assert!(root.join(directory).join("index.yaml").is_file());
        let mut actual = manifest
            .asset_index(category)
            .unwrap_or_else(|| panic!("{category} index must validate"))
            .assets()
            .iter()
            .map(|asset| asset.id())
            .collect::<Vec<_>>();
        actual.sort_unstable();
        assert_eq!(actual, expected);
    }
}

#[test]
fn energy_pack_loads_after_copying_only_the_pack_artifact() {
    let source = repository_pack_root();
    let isolated = tempfile::tempdir().expect("isolated pack root");
    copy_tree(&source, isolated.path());

    let manifest = load_pack_manifest(isolated.path(), &runtime())
        .expect("isolated Pack v1 artifact must be self-contained");

    assert_eq!(manifest.id(), "energy");
    assert!(isolated.path().join("models").is_dir());
    assert!(isolated.path().join("knowledge").is_dir());
    assert_eq!(
        file_names(&isolated.path().join("models"), "json"),
        EXPECTED_MODELS
    );
    assert_eq!(
        file_names(&isolated.path().join("knowledge"), "md"),
        EXPECTED_KNOWLEDGE
    );
    for category in ["mappings", "rules", "evaluations", "data_processing"] {
        let directory = manifest
            .asset_directory(category)
            .unwrap_or_else(|| panic!("missing {category} directory declaration"));
        assert!(
            isolated.path().join(directory).is_dir(),
            "missing {category}"
        );
        assert!(
            manifest.asset_index(category).is_some(),
            "isolated {category} index did not validate"
        );
    }
}

#[test]
fn formal_energy_assets_retain_versioned_fail_safe_payloads() {
    let root = repository_pack_root();

    let aliases = yaml_value(&root.join("mappings/product-name-aliases.yaml"));
    assert_eq!(aliases["schema"], "aether.pack.mapping-set.v1");
    assert_eq!(aliases["kind"], "product_aliases");
    assert_eq!(aliases["compatibility"]["removed_from_kernel"], "0.5.0");
    assert_eq!(aliases["compatibility"]["apply_before_kernel_schema"], 2);

    let legacy_properties = yaml_value(&root.join("mappings/legacy-instance-properties-v5.yaml"));
    assert_eq!(
        legacy_properties["kind"],
        "legacy_instance_properties_migration"
    );
    assert_eq!(
        legacy_properties["compatibility"]["removed_from_kernel"],
        "0.5.0"
    );
    assert_eq!(
        legacy_properties["compatibility"]["apply_before_kernel_schema"],
        5
    );

    let bindings = yaml_value(&root.join("mappings/example-instance-channel-bindings.yaml"));
    assert_eq!(bindings["commissioned"], false);
    assert!(
        bindings["bindings"]
            .as_array()
            .is_some_and(|entries| entries.iter().all(|entry| entry["enabled"] == false))
    );

    let rule: serde_json::Value = serde_json::from_slice(
        &fs::read(root.join("rules/battery_soc_management.json")).expect("read rule"),
    )
    .expect("parse rule");
    assert_eq!(rule["schema"], "aether.pack.rule.v1");
    assert_eq!(rule["enabled"], false);
    assert_eq!(rule["commissioned"], false);

    let evaluation = yaml_value(&root.join("evaluations/pack-safety.yaml"));
    assert_eq!(evaluation["schema"], "aether.pack.evaluation-suite.v1");
    assert_eq!(evaluation["execution"], "cargo_test_evidence");
    assert!(
        evaluation["scenarios"]
            .as_array()
            .is_some_and(|scenarios| !scenarios.is_empty())
    );

    for task in ["site-load-forecast.yaml", "site-pv-forecast.yaml"] {
        let task = yaml_value(&root.join("data-processing/tasks").join(task));
        assert_eq!(task["schema"], "aether.data-processing-task.v1");
        assert_eq!(task["enabled"], false);
    }
}

#[test]
fn only_an_explicitly_active_energy_pack_exposes_namespaced_formal_assets() {
    let source = repository_pack_root();
    let isolated = tempfile::tempdir().expect("isolated site");
    let pack = isolated.path().join("packs/energy");
    let config = isolated.path().join("config");
    copy_tree(&source, &pack);
    fs::create_dir_all(&config).expect("site config");
    fs::write(config.join("global.yaml"), "packs: []\n").expect("empty Pack config");

    let empty = load_active_packs(&config, &runtime()).expect("empty active set");
    for category in ["mappings", "rules", "evaluations", "data_processing"] {
        assert!(empty.namespaced_asset_ids(category).is_empty());
    }

    fs::write(
        config.join("global.yaml"),
        format!("packs:\n  - id: energy\n    root: {}\n", pack.display()),
    )
    .expect("active Energy config");
    let active = load_active_packs(&config, &runtime()).expect("active Energy Pack");
    assert_eq!(
        active.namespaced_asset_ids("rules"),
        vec!["energy/rules/energy.battery-soc-management"]
    );
    assert_eq!(
        active.namespaced_asset_ids("evaluations"),
        vec!["energy/evaluations/energy.pack-safety"]
    );
    assert_eq!(
        active.namespaced_asset_ids("data_processing"),
        vec![
            "energy/data_processing/energy.site-load-forecast",
            "energy/data_processing/energy.site-pv-forecast",
        ]
    );
    assert!(
        active
            .namespaced_asset_ids("mappings")
            .iter()
            .all(|id| id.starts_with("energy/mappings/energy."))
    );
}

#[test]
fn energy_requirements_are_application_catalog_capabilities() {
    let root = repository_pack_root();
    let manifest = load_pack_manifest(&root, &runtime()).expect("repository energy pack loads");
    let catalog = aether_sdk::application::capability_catalog()
        .iter()
        .map(|descriptor| descriptor.name())
        .collect::<std::collections::BTreeSet<_>>();

    let unknown = manifest
        .required_capabilities()
        .iter()
        .filter(|capability| !catalog.contains(capability.as_str()))
        .collect::<Vec<_>>();

    assert!(
        unknown.is_empty(),
        "energy requirements absent from the application catalog: {unknown:?}"
    );
}
