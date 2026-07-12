use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aether_pack::load_active_packs;
use aether_runtime_catalog::KernelRuntimeManifest;

fn host_target_triple() -> String {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("aarch64", "macos") => "aarch64-apple-darwin".to_string(),
        ("x86_64", "macos") => "x86_64-apple-darwin".to_string(),
        (architecture, "linux") => format!("{architecture}-unknown-linux-gnu"),
        (architecture, operating_system) => {
            panic!("unsupported Pack installer test host {architecture}-{operating_system}")
        },
    }
}

fn write_runtime_manifest(config: &Path, features: &[&str]) -> KernelRuntimeManifest {
    let manifest = KernelRuntimeManifest::from_io_features(
        env!("CARGO_PKG_VERSION"),
        host_target_triple(),
        features.iter().copied(),
    )
    .expect("test runtime manifest");
    manifest
        .write_to_config_directory(config)
        .expect("write runtime manifest");
    manifest
}

fn write_pack(root: &Path) {
    fs::create_dir_all(root.join("knowledge")).expect("knowledge directory");
    fs::write(
        root.join("pack.yaml"),
        r#"schema_version: 1
id: demo-pack
name: Demo Pack
version: 1.2.3
status: stable
description: Pack-only installer fixture
distribution:
  id: demo-distribution
  version: 1.2.3
  composition: demo-gateway
compatibility:
  aether: ">=0.5.0,<0.6.0"
  required_capabilities: []
  required_protocols: []
assets:
  knowledge: knowledge
examples:
  commissioned: false
capabilities: {}
"#,
    )
    .expect("pack manifest");
    fs::write(root.join("knowledge/guide.md"), "# Demo Pack\n").expect("knowledge asset");
}

fn aether(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aether"))
        .args(arguments)
        .env_remove("AETHER_CONFIG_PATH")
        .env_remove("AETHER_DATA_PATH")
        .env_remove("AETHER_INSTALL_CONTEXT_PATH")
        .output()
        .expect("run aether CLI")
}

fn build_bundle(site: &Path, runtime_manifest: &Path) -> PathBuf {
    let pack = site.join("source-pack");
    let bundle = site.join("demo-pack.bundle");
    write_pack(&pack);
    let output = aether(&[
        "--json",
        "packs",
        "build",
        "--pack-root",
        pack.to_str().expect("UTF-8 pack path"),
        "--runtime-manifest",
        runtime_manifest.to_str().expect("UTF-8 runtime path"),
        "--output",
        bundle.to_str().expect("UTF-8 bundle path"),
    ]);
    assert!(
        output.status.success(),
        "bundle build failed; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    bundle
}

#[test]
fn pack_only_bundle_contains_data_and_closed_metadata_but_no_kernel_payload() {
    let site = tempfile::tempdir().expect("site");
    let runtime_config = site.path().join("runtime");
    fs::create_dir_all(&runtime_config).expect("runtime config");
    write_runtime_manifest(&runtime_config, &[]);
    let bundle = build_bundle(site.path(), &runtime_config.join("runtime-manifest.json"));

    assert!(bundle.join("pack-artifact.json").is_file());
    assert!(bundle.join("pack/pack.yaml").is_file());
    for forbidden in ["bin", "crates", "libs", "services", "tools"] {
        assert!(!bundle.join(forbidden).exists());
        assert!(!bundle.join("pack").join(forbidden).exists());
    }

    let metadata: serde_json::Value = serde_json::from_slice(
        &fs::read(bundle.join("pack-artifact.json")).expect("artifact metadata"),
    )
    .expect("metadata JSON");
    assert_eq!(metadata["schema"], "aether.pack-artifact.v1");
    assert_eq!(metadata["pack"]["id"], "demo-pack");
    assert_eq!(metadata["pack"]["version"], "1.2.3");
    assert_eq!(metadata["kernel"]["version"], env!("CARGO_PKG_VERSION"));
    assert!(
        metadata["kernel"]["runtime_manifest_digest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:") && digest.len() == 71)
    );
    assert!(
        metadata["payload"]["files"]
            .as_array()
            .is_some_and(|files| {
                files.iter().all(|file| {
                    file["path"]
                        .as_str()
                        .is_some_and(|path| !path.starts_with("bin/") && !path.contains("/crates/"))
                })
            })
    );
}

#[test]
fn builder_rejects_kernel_binaries_and_core_source_even_inside_the_pack_root() {
    for forbidden_path in ["bin/aether", "crates/aether-domain/src/lib.rs"] {
        let site = tempfile::tempdir().expect("site");
        let runtime_config = site.path().join("runtime");
        let pack = site.path().join("source-pack");
        let bundle = site.path().join("rejected.bundle");
        fs::create_dir_all(&runtime_config).expect("runtime config");
        write_runtime_manifest(&runtime_config, &[]);
        write_pack(&pack);
        let forbidden = pack.join(forbidden_path);
        fs::create_dir_all(forbidden.parent().expect("forbidden parent"))
            .expect("forbidden directory");
        fs::write(&forbidden, b"kernel payload").expect("forbidden payload");

        let output = aether(&[
            "--json",
            "packs",
            "build",
            "--pack-root",
            pack.to_str().expect("UTF-8 pack path"),
            "--runtime-manifest",
            runtime_config
                .join("runtime-manifest.json")
                .to_str()
                .expect("UTF-8 runtime path"),
            "--output",
            bundle.to_str().expect("UTF-8 bundle path"),
        ]);
        assert!(!output.status.success(), "accepted {forbidden_path}");
        let error = String::from_utf8_lossy(&output.stdout);
        assert!(
            error.contains("forbidden Kernel") || error.contains("source file"),
            "unexpected rejection for {forbidden_path}: {error}"
        );
        assert!(!bundle.exists(), "published rejected bundle");
    }
}

#[test]
fn install_verifies_exact_runtime_digest_then_atomically_activates_the_pack() {
    let site = tempfile::tempdir().expect("site");
    let build_runtime = site.path().join("build-runtime");
    let config = site.path().join("config");
    let data = site.path().join("data");
    fs::create_dir_all(&build_runtime).expect("build runtime");
    fs::create_dir_all(&config).expect("site config");
    fs::create_dir_all(&data).expect("site data");
    write_runtime_manifest(&build_runtime, &[]);
    let bundle = build_bundle(site.path(), &build_runtime.join("runtime-manifest.json"));

    fs::write(
        config.join("global.yaml"),
        "site_name: fixture\npacks: []\n",
    )
    .expect("safe global config");
    write_runtime_manifest(&config, &["modbus"]);
    let before = fs::read(config.join("global.yaml")).expect("before mismatch");
    let rejected = aether(&[
        "--json",
        "--config-path",
        config.to_str().expect("UTF-8 config path"),
        "--db-path",
        data.to_str().expect("UTF-8 data path"),
        "packs",
        "install",
        "--artifact",
        bundle.to_str().expect("UTF-8 bundle path"),
    ]);
    assert!(
        !rejected.status.success(),
        "accepted a different runtime digest"
    );
    assert_eq!(
        fs::read(config.join("global.yaml")).expect("unchanged global config"),
        before
    );
    assert!(!data.join("packs/demo-pack/1.2.3").exists());

    fs::copy(
        build_runtime.join("runtime-manifest.json"),
        config.join("runtime-manifest.json"),
    )
    .expect("install matching runtime manifest");
    let installed = aether(&[
        "--json",
        "--config-path",
        config.to_str().expect("UTF-8 config path"),
        "--db-path",
        data.to_str().expect("UTF-8 data path"),
        "packs",
        "install",
        "--artifact",
        bundle.to_str().expect("UTF-8 bundle path"),
    ]);
    assert!(
        installed.status.success(),
        "install failed: {}",
        String::from_utf8_lossy(&installed.stderr)
    );

    let final_root = data.join("packs/demo-pack/1.2.3");
    assert!(final_root.join("pack.yaml").is_file());
    let runtime = aether_runtime_catalog::load_runtime_manifest_for_current_process(
        &config,
        env!("CARGO_PKG_VERSION"),
    )
    .expect("installed runtime manifest");
    let active = load_active_packs(
        &config,
        &runtime.pack_runtime().expect("Pack compatibility view"),
    )
    .expect("active Pack config");
    assert_eq!(
        active.get("demo-pack").expect("active demo Pack").root(),
        fs::canonicalize(final_root).expect("canonical installed Pack root")
    );
    let global = fs::read_to_string(config.join("global.yaml")).expect("activated config");
    assert!(global.contains("site_name: fixture"));
}

#[test]
fn tampered_payload_is_rejected_before_publish_or_activation() {
    let site = tempfile::tempdir().expect("site");
    let config = site.path().join("config");
    let data = site.path().join("data");
    fs::create_dir_all(&config).expect("site config");
    fs::create_dir_all(&data).expect("site data");
    write_runtime_manifest(&config, &[]);
    fs::write(config.join("global.yaml"), "packs: []\n").expect("safe config");
    let bundle = build_bundle(site.path(), &config.join("runtime-manifest.json"));
    fs::write(bundle.join("pack/knowledge/guide.md"), "tampered\n").expect("tamper payload");
    let before = fs::read(config.join("global.yaml")).expect("before install");

    let output = aether(&[
        "--json",
        "--config-path",
        config.to_str().expect("UTF-8 config path"),
        "--db-path",
        data.to_str().expect("UTF-8 data path"),
        "packs",
        "install",
        "--artifact",
        bundle.to_str().expect("UTF-8 bundle path"),
    ]);
    assert!(!output.status.success());
    assert_eq!(
        fs::read(config.join("global.yaml")).expect("global"),
        before
    );
    assert!(!data.join("packs/demo-pack/1.2.3").exists());
}

#[cfg(unix)]
#[test]
fn activation_write_failure_rolls_back_the_newly_published_pack() {
    use std::os::unix::fs::PermissionsExt;

    let effective_user = Command::new("id")
        .arg("-u")
        .output()
        .expect("query effective user");
    if effective_user.stdout == b"0\n" {
        // A root process bypasses directory write permission bits, so this
        // failure injection cannot make a meaningful assertion there.
        return;
    }

    let site = tempfile::tempdir().expect("site");
    let config = site.path().join("config");
    let data = site.path().join("data");
    fs::create_dir_all(&config).expect("site config");
    fs::create_dir_all(&data).expect("site data");
    write_runtime_manifest(&config, &[]);
    fs::write(
        config.join("global.yaml"),
        "site_name: rollback-fixture\npacks: []\n",
    )
    .expect("safe config");
    let bundle = build_bundle(site.path(), &config.join("runtime-manifest.json"));
    let before = fs::read(config.join("global.yaml")).expect("before install");
    let original_permissions = fs::metadata(&config)
        .expect("config metadata")
        .permissions();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o555))
        .expect("make config directory read-only");

    let output = aether(&[
        "--json",
        "--config-path",
        config.to_str().expect("UTF-8 config path"),
        "--db-path",
        data.to_str().expect("UTF-8 data path"),
        "packs",
        "install",
        "--artifact",
        bundle.to_str().expect("UTF-8 bundle path"),
    ]);

    fs::set_permissions(&config, original_permissions).expect("restore config permissions");
    assert!(
        !output.status.success(),
        "activation unexpectedly succeeded"
    );
    assert_eq!(
        fs::read(config.join("global.yaml")).expect("unchanged global config"),
        before
    );
    assert!(
        !data.join("packs/demo-pack/1.2.3").exists(),
        "newly published Pack was not rolled back"
    );
}
