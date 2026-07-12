use aether_config::automation::AutomationConfig;
use aether_config::io::IoConfig;
use common::ConfigValidator as _;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(path: &Path) -> String {
    fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
}

#[test]
fn default_distribution_has_no_commissioned_device_or_rule() {
    let root = repository_root();
    let global_path = root.join("config.template/global.yaml");
    let global: Value = serde_yml::from_str(&read(&global_path))
        .unwrap_or_else(|error| panic!("failed to parse {}: {error}", global_path.display()));
    assert_eq!(
        global.get("packs").and_then(Value::as_array).map(Vec::len),
        Some(0),
        "the distribution template must not activate a domain Pack"
    );

    let io_path = root.join("config.template/io/io.yaml");
    let io: IoConfig = serde_yml::from_str(&read(&io_path))
        .unwrap_or_else(|error| panic!("failed to parse {}: {error}", io_path.display()));
    assert!(
        io.channels.is_empty(),
        "the distribution template must not commission a device channel"
    );
    let schema_validation = io
        .validate_schema()
        .expect("default io config schema validation should run");
    assert!(
        schema_validation.is_valid,
        "an uncommissioned zero-channel installation must be valid: {:?}",
        schema_validation.errors
    );

    let automation_path = root.join("config.template/automation/automation.yaml");
    let automation: AutomationConfig = serde_yml::from_str(&read(&automation_path))
        .unwrap_or_else(|error| panic!("failed to parse {}: {error}", automation_path.display()));
    assert!(
        !automation.auto_load_instances,
        "the distribution template must not auto-load device instances"
    );
    assert!(
        automation.products_path.is_none(),
        "the distribution template must not select an implicit product directory"
    );

    let instances_path = root.join("config.template/automation/instances.yaml");
    let instances: Value = serde_yml::from_str(&read(&instances_path))
        .unwrap_or_else(|error| panic!("failed to parse {}: {error}", instances_path.display()));
    assert_eq!(
        instances
            .get("instances")
            .and_then(Value::as_object)
            .map(|items| items.len()),
        Some(0),
        "the distribution template must start with an empty instance map"
    );

    let rules_dir = root.join("config.template/automation/rules");
    if rules_dir.is_dir() {
        for entry in fs::read_dir(&rules_dir)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", rules_dir.display()))
        {
            let path = entry
                .expect("rule directory entry should be readable")
                .path();
            if path
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                let rule: Value = serde_json::from_str(&read(&path))
                    .unwrap_or_else(|error| panic!("failed to parse {}: {error}", path.display()));
                assert_eq!(
                    rule.get("enabled").and_then(Value::as_bool),
                    Some(false),
                    "distribution rule examples must be disabled: {}",
                    path.display()
                );
            }
        }
    }
}

#[test]
fn energy_pack_examples_require_explicit_activation() {
    let examples = repository_root().join("packs/energy/examples/config");

    let io_path = examples.join("io/io.yaml");
    let io: IoConfig = serde_yml::from_str(&read(&io_path))
        .unwrap_or_else(|error| panic!("failed to parse {}: {error}", io_path.display()));
    assert!(
        !io.channels.is_empty(),
        "energy pack should retain its examples"
    );
    for channel in &io.channels {
        assert!(
            !channel.is_enabled(),
            "energy example channel must be disabled: {}",
            channel.name()
        );
    }

    let automation_path = examples.join("automation/automation.yaml");
    let automation: AutomationConfig = serde_yml::from_str(&read(&automation_path))
        .unwrap_or_else(|error| panic!("failed to parse {}: {error}", automation_path.display()));
    assert!(
        !automation.auto_load_instances,
        "energy examples must require explicit instance activation"
    );

    let rules_dir = examples.join("automation/rules");
    for entry in fs::read_dir(&rules_dir)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", rules_dir.display()))
    {
        let path = entry
            .expect("rule directory entry should be readable")
            .path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            let rule: Value = serde_json::from_str(&read(&path))
                .unwrap_or_else(|error| panic!("failed to parse {}: {error}", path.display()));
            assert_eq!(
                rule.get("enabled").and_then(Value::as_bool),
                Some(false),
                "energy rule example must be disabled: {}",
                path.display()
            );
        }
    }

    let instance_path = examples.join("automation/instances/diesel_gen_01/instance.yaml");
    let instance: Value = serde_yml::from_str(&read(&instance_path))
        .unwrap_or_else(|error| panic!("failed to parse {}: {error}", instance_path.display()));
    assert_eq!(
        instance
            .pointer("/instance/enabled")
            .and_then(Value::as_bool),
        Some(false),
        "energy instance example must be disabled"
    );
}
