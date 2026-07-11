use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SAFE_CONFIG_FILES: [&str; 4] = [
    "global.yaml",
    "io/io.yaml",
    "automation/automation.yaml",
    "automation/instances.yaml",
];

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn invoke_setup(config_path: &Path, data_path: &Path, setup_arguments: &[&str]) -> Output {
    let executable = env!("CARGO_BIN_EXE_aether");
    let mut command = Command::new(executable);
    command
        .arg("--json")
        .arg("--config-path")
        .arg(config_path)
        .arg("--db-path")
        .arg(data_path)
        .arg("setup");
    command.args(setup_arguments);
    command.output().expect("run aether setup")
}

fn invoke_init(config_path: &Path, data_path: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aether"))
        .arg("--json")
        .arg("--config-path")
        .arg(config_path)
        .arg("--db-path")
        .arg(data_path)
        .arg("init")
        .output()
        .expect("run aether init")
}

fn parse_json_output(output: &Output) -> serde_json::Value {
    let stdout = String::from_utf8(output.stdout.clone()).expect("setup stdout is UTF-8");
    serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("setup stdout was not one JSON envelope: {error}\n{stdout}"))
}

fn plan_id(plan_envelope: &serde_json::Value) -> &str {
    plan_envelope
        .pointer("/data/plan_id")
        .and_then(serde_json::Value::as_str)
        .expect("setup plan contains plan_id")
}

#[cfg(unix)]
fn assert_blocked_symlink_setup(
    config_path: &Path,
    data_path: &Path,
    paths_that_must_remain_absent: &[&Path],
) {
    let plan_output = invoke_setup(config_path, data_path, &[]);
    assert!(
        plan_output.status.success(),
        "blocked setup plan should remain inspectable: {}",
        String::from_utf8_lossy(&plan_output.stderr)
    );
    let plan = parse_json_output(&plan_output);
    let apply_output = invoke_setup(
        config_path,
        data_path,
        &["apply", "--plan-id", plan_id(&plan)],
    );

    assert_eq!(
        plan.pointer("/data/site_state"),
        Some(&serde_json::json!("blocked"))
    );
    assert!(
        plan.pointer("/data/blockers")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|blockers| blockers.iter().any(|blocker| {
                blocker
                    .as_str()
                    .is_some_and(|message| message.contains("symlink"))
            }))
    );
    assert!(!apply_output.status.success());
    for path in paths_that_must_remain_absent {
        assert!(
            !path.exists(),
            "blocked setup wrote through an unsafe path: {}",
            path.display()
        );
    }
}

fn copy_safe_config_file(config_path: &Path, relative_path: &str) {
    let source = repository_root()
        .join("config.template")
        .join(relative_path);
    let destination = config_path.join(relative_path);
    std::fs::create_dir_all(destination.parent().expect("safe config parent"))
        .expect("create safe config parent");
    std::fs::copy(source, destination).expect("copy safe config fixture");
}

fn copy_all_safe_config_files(config_path: &Path) {
    for relative_path in SAFE_CONFIG_FILES {
        copy_safe_config_file(config_path, relative_path);
    }
}

async fn configured_entity_counts(database_file: &Path) -> (i64, i64, i64) {
    let options = SqliteConnectOptions::new()
        .filename(database_file)
        .read_only(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("open setup database read-only");
    let channels = sqlx::query_scalar("SELECT COUNT(*) FROM channels")
        .fetch_one(&pool)
        .await
        .expect("count channels");
    let instances = sqlx::query_scalar("SELECT COUNT(*) FROM instances")
        .fetch_one(&pool)
        .await
        .expect("count instances");
    let rules = sqlx::query_scalar("SELECT COUNT(*) FROM rules")
        .fetch_one(&pool)
        .await
        .expect("count rules");
    (channels, instances, rules)
}

#[test]
fn default_setup_plan_is_structured_and_persistently_read_only() {
    let workspace = tempfile::tempdir().expect("create setup workspace");
    let config_path = workspace.path().join("config");
    let data_path = workspace.path().join("data");

    let output = invoke_setup(&config_path, &data_path, &[]);

    assert!(
        output.status.success(),
        "setup plan failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let envelope = parse_json_output(&output);
    assert_eq!(envelope.get("success"), Some(&serde_json::json!(true)));
    assert_eq!(
        envelope.pointer("/data/plan_schema_version"),
        Some(&serde_json::json!(2))
    );
    assert_eq!(
        envelope.pointer("/data/aether_version"),
        Some(&serde_json::json!(env!("CARGO_PKG_VERSION")))
    );
    assert_eq!(
        envelope.pointer("/data/site_state"),
        Some(&serde_json::json!("fresh"))
    );
    assert_eq!(
        envelope.pointer("/data/read_only"),
        Some(&serde_json::json!(true))
    );
    assert_eq!(
        envelope.pointer("/data/physical_side_effects"),
        Some(&serde_json::json!(false))
    );
    let generated_plan_id = plan_id(&envelope);
    assert_eq!(generated_plan_id.len(), 64);
    assert_eq!(
        envelope.pointer("/data/apply_argv"),
        Some(&serde_json::json!([
            "aether",
            "--config-path",
            config_path.display().to_string(),
            "--db-path",
            data_path.display().to_string(),
            "setup",
            "apply",
            "--plan-id",
            generated_plan_id,
        ]))
    );
    assert!(!config_path.exists(), "plan must not create config files");
    assert!(!data_path.exists(), "plan must not create a database");
}

#[cfg(unix)]
#[test]
fn setup_rejects_a_symlinked_config_directory_without_writing_through_it() {
    use std::os::unix::fs::symlink;

    let workspace = tempfile::tempdir().expect("create setup workspace");
    let redirected_config = workspace.path().join("redirected-config");
    std::fs::create_dir(&redirected_config).expect("create redirect target");
    let config_path = workspace.path().join("config-link");
    symlink(&redirected_config, &config_path).expect("create config symlink");
    let data_path = workspace.path().join("data");
    let escaped_global = redirected_config.join("global.yaml");

    assert_blocked_symlink_setup(
        &config_path,
        &data_path,
        &[
            escaped_global.as_path(),
            data_path.join("aether.db").as_path(),
        ],
    );
}

#[cfg(unix)]
#[test]
fn setup_rejects_a_symlinked_data_directory_without_writing_through_it() {
    use std::os::unix::fs::symlink;

    let workspace = tempfile::tempdir().expect("create setup workspace");
    let config_path = workspace.path().join("config");
    let redirected_data = workspace.path().join("redirected-data");
    std::fs::create_dir(&redirected_data).expect("create redirect target");
    let data_path = workspace.path().join("data-link");
    symlink(&redirected_data, &data_path).expect("create data symlink");
    let escaped_database = redirected_data.join("aether.db");

    assert_blocked_symlink_setup(
        &config_path,
        &data_path,
        &[
            config_path.join("global.yaml").as_path(),
            escaped_database.as_path(),
        ],
    );
}

#[cfg(unix)]
#[test]
fn setup_rejects_a_required_config_file_symlink() {
    use std::os::unix::fs::symlink;

    let workspace = tempfile::tempdir().expect("create setup workspace");
    let config_path = workspace.path().join("config");
    let data_path = workspace.path().join("data");
    std::fs::create_dir(&config_path).expect("create config directory");
    let external_global = workspace.path().join("external-global.yaml");
    std::fs::copy(
        repository_root().join("config.template/global.yaml"),
        &external_global,
    )
    .expect("copy safe global fixture");
    symlink(&external_global, config_path.join("global.yaml"))
        .expect("create required-file symlink");

    assert_blocked_symlink_setup(
        &config_path,
        &data_path,
        &[
            config_path.join("io/io.yaml").as_path(),
            data_path.join("aether.db").as_path(),
        ],
    );
}

#[cfg(unix)]
#[test]
fn setup_rejects_a_symlinked_database_and_preserves_its_target() {
    use std::os::unix::fs::symlink;

    let workspace = tempfile::tempdir().expect("create setup workspace");
    let config_path = workspace.path().join("config");
    let data_path = workspace.path().join("data");
    std::fs::create_dir(&data_path).expect("create data directory");
    let external_database = workspace.path().join("external.db");
    let original_contents = b"not an Aether database";
    std::fs::write(&external_database, original_contents).expect("write external database");
    symlink(&external_database, data_path.join("aether.db")).expect("create database symlink");

    let plan_output = invoke_setup(&config_path, &data_path, &[]);
    assert!(plan_output.status.success());
    let plan = parse_json_output(&plan_output);
    let apply_output = invoke_setup(
        &config_path,
        &data_path,
        &["apply", "--plan-id", plan_id(&plan)],
    );

    assert_eq!(
        plan.pointer("/data/site_state"),
        Some(&serde_json::json!("blocked"))
    );
    assert!(
        plan.pointer("/data/blockers")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|blockers| blockers.iter().any(|blocker| {
                blocker
                    .as_str()
                    .is_some_and(|message| message.contains("symlink"))
            }))
    );
    assert!(!apply_output.status.success());
    assert_eq!(
        std::fs::read(&external_database).expect("read preserved external database"),
        original_contents
    );
    assert!(!config_path.exists());
}

#[test]
fn setup_allows_normal_nonexistent_tail_directories() {
    let workspace = tempfile::tempdir().expect("create setup workspace");
    let site_root = workspace.path().join("new/site");
    let config_path = site_root.join("config");
    let data_path = site_root.join("data");

    let plan_output = invoke_setup(&config_path, &data_path, &[]);
    assert!(plan_output.status.success());
    let plan = parse_json_output(&plan_output);
    assert_eq!(
        plan.pointer("/data/site_state"),
        Some(&serde_json::json!("fresh"))
    );

    let apply_output = invoke_setup(
        &config_path,
        &data_path,
        &["apply", "--plan-id", plan_id(&plan)],
    );
    assert!(
        apply_output.status.success(),
        "safe nested-tail setup failed: {}",
        String::from_utf8_lossy(&apply_output.stderr)
    );
    assert!(config_path.join("global.yaml").is_file());
    assert!(data_path.join("aether.db").is_file());
}

#[test]
fn relative_setup_paths_are_bound_to_absolute_working_directory_paths() {
    let workspace = tempfile::tempdir().expect("create setup workspace");
    let absolute_workspace = workspace
        .path()
        .canonicalize()
        .expect("canonicalize setup workspace");
    let output = Command::new(env!("CARGO_BIN_EXE_aether"))
        .current_dir(workspace.path())
        .arg("--json")
        .arg("--config-path")
        .arg("relative-config")
        .arg("--db-path")
        .arg("relative-data")
        .arg("setup")
        .output()
        .expect("run relative-path setup plan");

    assert!(output.status.success());
    let plan = parse_json_output(&output);
    assert_eq!(
        plan.pointer("/data/config_path"),
        Some(&serde_json::json!(
            absolute_workspace
                .join("relative-config")
                .display()
                .to_string()
        ))
    );
    assert_eq!(
        plan.pointer("/data/data_path"),
        Some(&serde_json::json!(
            absolute_workspace
                .join("relative-data")
                .display()
                .to_string()
        ))
    );
}

#[tokio::test]
async fn fresh_setup_apply_creates_only_the_safe_empty_runtime() {
    let workspace = tempfile::tempdir().expect("create setup workspace");
    let config_path = workspace.path().join("config");
    let data_path = workspace.path().join("data");
    let plan_output = invoke_setup(&config_path, &data_path, &[]);
    let plan = parse_json_output(&plan_output);

    let apply_output = invoke_setup(
        &config_path,
        &data_path,
        &["apply", "--plan-id", plan_id(&plan)],
    );

    assert!(
        apply_output.status.success(),
        "setup apply failed: {}",
        String::from_utf8_lossy(&apply_output.stderr)
    );
    let applied = parse_json_output(&apply_output);
    assert_eq!(
        applied.pointer("/data/configured"),
        Some(&serde_json::json!(true))
    );
    assert_eq!(
        applied.pointer("/data/ready"),
        Some(&serde_json::json!(false))
    );
    assert_eq!(
        applied.pointer("/data/applied"),
        Some(&serde_json::json!(true))
    );
    for relative_path in SAFE_CONFIG_FILES {
        let actual = std::fs::read(config_path.join(relative_path)).expect("read created config");
        let expected = std::fs::read(
            repository_root()
                .join("config.template")
                .join(relative_path),
        )
        .expect("read distribution template");
        assert_eq!(actual, expected, "setup changed {relative_path}");
    }
    assert!(!config_path.join("automation/rules").exists());
    assert_eq!(
        configured_entity_counts(&data_path.join("aether.db")).await,
        (0, 0, 0)
    );
}

#[test]
fn safe_partial_setup_preserves_existing_files_and_second_apply_is_noop() {
    let workspace = tempfile::tempdir().expect("create setup workspace");
    let config_path = workspace.path().join("config");
    let data_path = workspace.path().join("data");
    copy_safe_config_file(&config_path, "global.yaml");
    let original_global = std::fs::read(config_path.join("global.yaml")).expect("read global");

    let partial_plan = parse_json_output(&invoke_setup(&config_path, &data_path, &[]));
    assert_eq!(
        partial_plan.pointer("/data/site_state"),
        Some(&serde_json::json!("safe_partial"))
    );
    let first_apply = invoke_setup(
        &config_path,
        &data_path,
        &["apply", "--plan-id", plan_id(&partial_plan)],
    );
    assert!(first_apply.status.success());
    assert_eq!(
        std::fs::read(config_path.join("global.yaml")).expect("read preserved global"),
        original_global
    );

    let ready_plan = parse_json_output(&invoke_setup(&config_path, &data_path, &[]));
    assert_eq!(
        ready_plan.pointer("/data/site_state"),
        Some(&serde_json::json!("safe_ready"))
    );
    let database_before = std::fs::read(data_path.join("aether.db")).expect("read setup database");
    let second_apply = invoke_setup(
        &config_path,
        &data_path,
        &["apply", "--plan-id", plan_id(&ready_plan)],
    );
    assert!(second_apply.status.success());
    let second_result = parse_json_output(&second_apply);
    assert_eq!(
        second_result.pointer("/data/applied"),
        Some(&serde_json::json!(false))
    );
    assert_eq!(
        std::fs::read(data_path.join("aether.db")).expect("read unchanged setup database"),
        database_before
    );
}

#[test]
fn existing_site_plan_and_apply_never_modify_the_site() {
    let workspace = tempfile::tempdir().expect("create setup workspace");
    let config_path = workspace.path().join("config");
    let data_path = workspace.path().join("data");
    copy_all_safe_config_files(&config_path);
    std::fs::write(
        config_path.join("io/io.yaml"),
        "channels:\n  - id: 1\n    name: existing\n    protocol: virtual\n    enabled: false\n",
    )
    .expect("write existing site channel");
    let io_before = std::fs::read(config_path.join("io/io.yaml")).expect("read existing io");

    let plan_output = invoke_setup(&config_path, &data_path, &[]);
    assert!(plan_output.status.success());
    let plan = parse_json_output(&plan_output);
    assert_eq!(
        plan.pointer("/data/site_state"),
        Some(&serde_json::json!("existing"))
    );

    let apply_output = invoke_setup(
        &config_path,
        &data_path,
        &["apply", "--plan-id", plan_id(&plan)],
    );
    assert!(!apply_output.status.success());
    assert_eq!(
        std::fs::read(config_path.join("io/io.yaml")).expect("read unchanged existing io"),
        io_before
    );
    assert!(!data_path.exists());
}

#[test]
fn stale_setup_plan_is_rejected_before_any_persistent_write() {
    let workspace = tempfile::tempdir().expect("create setup workspace");
    let config_path = workspace.path().join("config");
    let data_path = workspace.path().join("data");
    let fresh_plan = parse_json_output(&invoke_setup(&config_path, &data_path, &[]));
    std::fs::create_dir_all(&config_path).expect("create changed config directory");
    std::fs::write(config_path.join("global.yaml"), "site_name: changed\n")
        .expect("change site after planning");
    let changed_global = std::fs::read(config_path.join("global.yaml")).expect("read changed site");

    let apply_output = invoke_setup(
        &config_path,
        &data_path,
        &["apply", "--plan-id", plan_id(&fresh_plan)],
    );

    assert!(!apply_output.status.success());
    let error = parse_json_output(&apply_output);
    assert_eq!(error.get("success"), Some(&serde_json::json!(false)));
    assert!(
        error
            .get("error")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|message| message.contains("stale"))
    );
    assert_eq!(
        std::fs::read(config_path.join("global.yaml")).expect("read unchanged changed site"),
        changed_global
    );
    assert!(!data_path.exists());
}

#[test]
fn unrecognized_partial_site_is_blocked_without_filling_missing_files() {
    let workspace = tempfile::tempdir().expect("create setup workspace");
    let config_path = workspace.path().join("config");
    let data_path = workspace.path().join("data");
    std::fs::create_dir_all(&config_path).expect("create partial config directory");
    std::fs::write(config_path.join("global.yaml"), "site_name: custom\n")
        .expect("write custom partial config");

    let plan = parse_json_output(&invoke_setup(&config_path, &data_path, &[]));

    assert_eq!(
        plan.pointer("/data/site_state"),
        Some(&serde_json::json!("blocked"))
    );
    assert!(
        plan.pointer("/data/blockers")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|blockers| !blockers.is_empty())
    );
    assert!(!config_path.join("io/io.yaml").exists());
    assert!(!data_path.exists());
}

#[tokio::test]
async fn empty_database_is_safe_partial_and_is_initialized_on_apply() {
    let workspace = tempfile::tempdir().expect("create setup workspace");
    let config_path = workspace.path().join("config");
    let data_path = workspace.path().join("data");
    copy_all_safe_config_files(&config_path);
    std::fs::create_dir_all(&data_path).expect("create data directory");
    std::fs::write(data_path.join("aether.db"), []).expect("create empty database file");
    let entries_before_plan = std::fs::read_dir(&data_path)
        .expect("list data before plan")
        .map(|entry| entry.expect("read data entry").file_name())
        .collect::<Vec<_>>();

    let plan = parse_json_output(&invoke_setup(&config_path, &data_path, &[]));

    let entries_after_plan = std::fs::read_dir(&data_path)
        .expect("list data after plan")
        .map(|entry| entry.expect("read data entry").file_name())
        .collect::<Vec<_>>();
    assert_eq!(entries_after_plan, entries_before_plan);
    assert!(!data_path.join("aether.db-wal").exists());
    assert!(!data_path.join("aether.db-shm").exists());
    assert_eq!(
        plan.pointer("/data/site_state"),
        Some(&serde_json::json!("safe_partial"))
    );
    let apply = invoke_setup(
        &config_path,
        &data_path,
        &["apply", "--plan-id", plan_id(&plan)],
    );
    assert!(
        apply.status.success(),
        "safe partial apply failed: {}",
        String::from_utf8_lossy(&apply.stderr)
    );
    assert_eq!(
        configured_entity_counts(&data_path.join("aether.db")).await,
        (0, 0, 0)
    );

    let ready = parse_json_output(&invoke_setup(&config_path, &data_path, &[]));
    assert_eq!(
        ready.pointer("/data/site_state"),
        Some(&serde_json::json!("safe_ready"))
    );
}

#[tokio::test]
async fn incomplete_domain_sync_metadata_is_not_reported_safe_ready() {
    let workspace = tempfile::tempdir().expect("create setup workspace");
    let config_path = workspace.path().join("config");
    let data_path = workspace.path().join("data");
    copy_all_safe_config_files(&config_path);
    let initialized = invoke_init(&config_path, &data_path);
    assert!(initialized.status.success());

    let options = SqliteConnectOptions::new().filename(data_path.join("aether.db"));
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("open initialized database");
    sqlx::query(
        "INSERT INTO sync_metadata (service, last_sync) VALUES ('global', CURRENT_TIMESTAMP)",
    )
    .execute(&pool)
    .await
    .expect("mark only one domain synchronized");
    pool.close().await;

    let plan = parse_json_output(&invoke_setup(&config_path, &data_path, &[]));
    assert_eq!(
        plan.pointer("/data/site_state"),
        Some(&serde_json::json!("safe_partial"))
    );
}

#[tokio::test]
async fn older_fully_synced_schema_is_safe_partial_until_apply_migrates_it() {
    let workspace = tempfile::tempdir().expect("create setup workspace");
    let config_path = workspace.path().join("config");
    let data_path = workspace.path().join("data");
    copy_all_safe_config_files(&config_path);
    let initialized = invoke_init(&config_path, &data_path);
    assert!(initialized.status.success());

    let options = SqliteConnectOptions::new().filename(data_path.join("aether.db"));
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("open initialized database");
    sqlx::query("PRAGMA user_version = 5")
        .execute(&pool)
        .await
        .expect("downgrade version fixture");
    sqlx::query(
        "INSERT INTO sync_metadata (service, last_sync) VALUES \
         ('global', CURRENT_TIMESTAMP), \
         ('io', CURRENT_TIMESTAMP), \
         ('automation', CURRENT_TIMESTAMP)",
    )
    .execute(&pool)
    .await
    .expect("mark every domain synchronized");
    pool.close().await;

    let plan = parse_json_output(&invoke_setup(&config_path, &data_path, &[]));
    assert_eq!(
        plan.pointer("/data/site_state"),
        Some(&serde_json::json!("safe_partial"))
    );
    let applied = invoke_setup(
        &config_path,
        &data_path,
        &["apply", "--plan-id", plan_id(&plan)],
    );
    assert!(
        applied.status.success(),
        "schema migration setup failed: {}",
        String::from_utf8_lossy(&applied.stderr)
    );

    let ready = parse_json_output(&invoke_setup(&config_path, &data_path, &[]));
    assert_eq!(
        ready.pointer("/data/site_state"),
        Some(&serde_json::json!("safe_ready"))
    );
}

#[tokio::test]
async fn setup_plan_observes_commissioned_rows_still_in_the_live_wal() {
    let workspace = tempfile::tempdir().expect("create setup workspace");
    let config_path = workspace.path().join("config");
    let data_path = workspace.path().join("data");
    let fresh_plan = parse_json_output(&invoke_setup(&config_path, &data_path, &[]));
    let applied = invoke_setup(
        &config_path,
        &data_path,
        &["apply", "--plan-id", plan_id(&fresh_plan)],
    );
    assert!(
        applied.status.success(),
        "setup apply failed: {}",
        String::from_utf8_lossy(&applied.stderr)
    );

    let database_file = data_path.join("aether.db");
    let options = SqliteConnectOptions::new().filename(&database_file);
    let writer = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("open setup database for live WAL fixture");
    sqlx::query("PRAGMA wal_autocheckpoint = 0")
        .execute(&writer)
        .await
        .expect("disable automatic WAL checkpointing");
    sqlx::query(
        "INSERT INTO channels (channel_id, name, protocol, enabled, config) \
         VALUES (99, 'live-wal-channel', 'virtual', 1, '{}')",
    )
    .execute(&writer)
    .await
    .expect("commit commissioned channel to WAL");
    let visible_channels: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM channels")
        .fetch_one(&writer)
        .await
        .expect("count channel through live writer");
    assert_eq!(visible_channels, 1);

    let wal_file = data_path.join("aether.db-wal");
    assert!(
        std::fs::metadata(&wal_file).expect("live WAL exists").len() > 0,
        "commissioned row must remain in the WAL for this regression test"
    );
    let mut entries_before = std::fs::read_dir(&data_path)
        .expect("list data before setup plan")
        .map(|entry| entry.expect("read data entry").file_name())
        .collect::<Vec<_>>();
    entries_before.sort();
    let database_before = std::fs::read(&database_file).expect("read database before setup plan");
    let wal_before = std::fs::read(&wal_file).expect("read WAL before setup plan");

    let plan_output = invoke_setup(&config_path, &data_path, &[]);
    assert!(
        plan_output.status.success(),
        "setup plan failed: {}",
        String::from_utf8_lossy(&plan_output.stderr)
    );
    let plan = parse_json_output(&plan_output);

    assert_eq!(
        plan.pointer("/data/site_state"),
        Some(&serde_json::json!("existing"))
    );
    assert_eq!(
        std::fs::read(&database_file).expect("read unchanged database"),
        database_before
    );
    assert_eq!(
        std::fs::read(&wal_file).expect("read unchanged WAL"),
        wal_before
    );
    let mut entries_after = std::fs::read_dir(&data_path)
        .expect("list data after setup plan")
        .map(|entry| entry.expect("read data entry").file_name())
        .collect::<Vec<_>>();
    entries_after.sort();
    assert_eq!(entries_after, entries_before);

    writer.close().await;
}
