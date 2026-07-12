use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aether_domain::ChannelId;
use aether_io::{ChannelRuntimeLifecycle, SqliteChannelMutator, core::config::ChannelConfig};
use aether_ports::{
    ChannelDefinition, ChannelDesiredStateObservation, ChannelLoggingPolicy, ChannelMutation,
    ChannelMutator, ChannelParameterValue, ChannelPatch, ChannelReconciler,
    ChannelReconciliationScope, ChannelRevision, ChannelRuntimeProjection, PortError,
    PortErrorKind, PortResult,
};
use async_trait::async_trait;
use sqlx::sqlite::SqlitePoolOptions;
use tokio::sync::Notify;

#[derive(Default)]
struct ActivationPause {
    entered: Notify,
    release: Notify,
}

#[derive(Default)]
struct FakeRuntime {
    active: Mutex<HashSet<u32>>,
    configs: Mutex<HashMap<u32, ChannelConfig>>,
    calls: Mutex<Vec<String>>,
    fail_ensure: Mutex<bool>,
    fail_fence: Mutex<bool>,
    activation_pause: Mutex<Option<Arc<ActivationPause>>>,
    fence_pause: Mutex<Option<Arc<ActivationPause>>>,
}

impl FakeRuntime {
    fn set_active(&self, channel_id: u32, active: bool) {
        let mut channels = self.active.lock().expect("active lock");
        if active {
            channels.insert(channel_id);
        } else {
            channels.remove(&channel_id);
        }
    }

    fn is_active(&self, channel_id: u32) -> bool {
        self.active
            .lock()
            .expect("active lock")
            .contains(&channel_id)
    }

    fn config(&self, channel_id: u32) -> Option<ChannelConfig> {
        self.configs
            .lock()
            .expect("config lock")
            .get(&channel_id)
            .cloned()
    }

    fn set_runtime_name(&self, channel_id: u32, name: &str) {
        self.configs
            .lock()
            .expect("config lock")
            .get_mut(&channel_id)
            .expect("runtime config")
            .core
            .name = name.to_owned();
    }

    fn fail_next_ensure(&self) {
        *self.fail_ensure.lock().expect("ensure failure lock") = true;
    }

    fn fail_next_fence(&self) {
        *self.fail_fence.lock().expect("fence failure lock") = true;
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("calls lock").clone()
    }

    fn pause_next_activation(&self) -> Arc<ActivationPause> {
        let pause = Arc::new(ActivationPause::default());
        *self.activation_pause.lock().expect("activation pause lock") = Some(Arc::clone(&pause));
        pause
    }

    fn pause_next_fence(&self) -> Arc<ActivationPause> {
        let pause = Arc::new(ActivationPause::default());
        *self.fence_pause.lock().expect("fence pause lock") = Some(Arc::clone(&pause));
        pause
    }
}

#[async_trait]
impl ChannelRuntimeLifecycle for FakeRuntime {
    fn validate(&self, config: &ChannelConfig) -> PortResult<()> {
        if config.core.protocol == "rejected" {
            Err(PortError::new(
                PortErrorKind::InvalidData,
                "injected protocol schema rejection",
            ))
        } else {
            let mut validation = common::ValidationResult::new(common::ValidationLevel::Schema);
            config.validate(&mut validation, 0);
            if validation.is_valid {
                Ok(())
            } else {
                Err(PortError::new(
                    PortErrorKind::InvalidData,
                    "injected runtime schema rejection",
                ))
            }
        }
    }

    fn is_present(&self, channel_id: ChannelId) -> bool {
        self.is_active(channel_id.get())
    }

    fn channel_ids(&self) -> Vec<ChannelId> {
        self.active
            .lock()
            .expect("active lock")
            .iter()
            .copied()
            .map(ChannelId::new)
            .collect()
    }

    async fn activate(&self, config: Arc<ChannelConfig>) -> PortResult<()> {
        self.calls
            .lock()
            .expect("calls lock")
            .push(format!("activate:{}", config.id()));
        if std::mem::take(&mut *self.fail_ensure.lock().expect("ensure failure lock")) {
            return Err(PortError::new(
                PortErrorKind::Unavailable,
                "injected activation failure",
            ));
        }
        let pause = self
            .activation_pause
            .lock()
            .expect("activation pause lock")
            .take();
        if let Some(pause) = pause {
            pause.entered.notify_one();
            pause.release.notified().await;
        }
        self.configs
            .lock()
            .expect("config lock")
            .insert(config.id(), (*config).clone());
        self.set_active(config.id(), true);
        Ok(())
    }

    async fn fence(&self, channel_id: ChannelId) -> PortResult<()> {
        self.calls
            .lock()
            .expect("calls lock")
            .push(format!("fence:{}", channel_id.get()));
        if std::mem::take(&mut *self.fail_fence.lock().expect("fence failure lock")) {
            return Err(PortError::new(
                PortErrorKind::Unavailable,
                "injected fence failure",
            ));
        }
        self.set_active(channel_id.get(), false);
        let pause = self.fence_pause.lock().expect("fence pause lock").take();
        if let Some(pause) = pause {
            pause.entered.notify_one();
            pause.release.notified().await;
        }
        Ok(())
    }
}

async fn test_pool() -> sqlx::SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("in-memory database");
    common::test_utils::schema::init_io_schema(&pool)
        .await
        .expect("io schema");
    common::test_utils::schema::init_automation_schema(&pool)
        .await
        .expect("automation schema");
    sqlx::query(
        "CREATE TABLE json_point_mappings (\
             id INTEGER PRIMARY KEY AUTOINCREMENT,\
             channel_id INTEGER NOT NULL REFERENCES channels(channel_id) ON DELETE CASCADE\
         )",
    )
    .execute(&pool)
    .await
    .expect("json mapping schema");
    pool
}

fn definition(channel_id: u32, enabled: bool) -> ChannelDefinition {
    ChannelDefinition::new(
        Some(ChannelId::new(channel_id)),
        format!("channel-{channel_id}"),
        "virtual",
        BTreeMap::from([
            (
                "credential".to_string(),
                ChannelParameterValue::String("never-log-secret".to_string()),
            ),
            (
                "nested".to_string(),
                ChannelParameterValue::Object(BTreeMap::from([(
                    "retry".to_string(),
                    ChannelParameterValue::Integer(3),
                )])),
            ),
        ]),
    )
    .with_description("test channel")
    .with_logging(
        ChannelLoggingPolicy::default()
            .with_enabled(true)
            .with_level("debug")
            .with_file("/var/log/aether/channel.log"),
    )
    .with_enabled(enabled)
}

fn auto_definition(name: &str) -> ChannelDefinition {
    ChannelDefinition::new(None, name, "virtual", BTreeMap::new())
}

fn protocol_definition(
    channel_id: u32,
    protocol: &str,
    parameters: BTreeMap<String, ChannelParameterValue>,
    enabled: bool,
) -> ChannelDefinition {
    ChannelDefinition::new(
        Some(ChannelId::new(channel_id)),
        format!("channel-{channel_id}"),
        protocol,
        parameters,
    )
    .with_enabled(enabled)
}

fn adapter(pool: sqlx::SqlitePool, runtime: Arc<FakeRuntime>) -> SqliteChannelMutator {
    SqliteChannelMutator::with_runtime(pool, runtime)
}

#[tokio::test]
async fn create_defaults_to_stopped_and_persists_typed_config_at_revision_one() {
    let pool = test_pool().await;
    let runtime = Arc::new(FakeRuntime::default());
    let mutator = adapter(pool.clone(), Arc::clone(&runtime));

    let receipt = mutator
        .mutate(ChannelMutation::create(definition(7, false)))
        .await
        .expect("create disabled channel");

    assert_eq!(receipt.resulting_revision(), ChannelRevision::new(1));
    assert!(!receipt.desired_enabled());
    assert_eq!(
        receipt.runtime_projection(),
        ChannelRuntimeProjection::Stopped
    );
    assert!(!receipt.reconciliation_required());
    assert!(runtime.calls().is_empty());

    let (enabled, revision, config): (bool, i64, String) =
        sqlx::query_as("SELECT enabled, revision, config FROM channels WHERE channel_id = 7")
            .fetch_one(&pool)
            .await
            .expect("persisted channel");
    assert!(!enabled);
    assert_eq!(revision, 1);
    let config: serde_json::Value = serde_json::from_str(&config).expect("valid config json");
    assert_eq!(config["description"], "test channel");
    assert_eq!(config["parameters"]["credential"], "never-log-secret");
    assert_eq!(config["parameters"]["nested"]["retry"], 3);
    assert_eq!(config["logging"]["enabled"], true);
    assert_eq!(config["logging"]["level"], "debug");
    assert_eq!(config["logging"]["file"], "/var/log/aether/channel.log");
}

#[tokio::test]
async fn governed_modbus_create_rejects_fallback_and_truncation_inputs_before_commit() {
    let pool = test_pool().await;
    let runtime = Arc::new(FakeRuntime::default());
    let mutator = adapter(pool.clone(), Arc::clone(&runtime));
    let invalid = [
        protocol_definition(
            30,
            "modbus_tcp",
            BTreeMap::from([
                ("host".to_owned(), ChannelParameterValue::Integer(123)),
                ("port".to_owned(), ChannelParameterValue::Integer(502)),
            ]),
            true,
        ),
        protocol_definition(
            31,
            "modbus_tcp",
            BTreeMap::from([
                (
                    "host".to_owned(),
                    ChannelParameterValue::String("edge".to_owned()),
                ),
                ("port".to_owned(), ChannelParameterValue::Integer(-1)),
            ]),
            true,
        ),
        protocol_definition(
            32,
            "sunspec_tcp",
            BTreeMap::from([
                (
                    "host".to_owned(),
                    ChannelParameterValue::String("edge".to_owned()),
                ),
                ("port".to_owned(), ChannelParameterValue::Integer(65_536)),
            ]),
            true,
        ),
        protocol_definition(
            33,
            "modbus_rtu",
            BTreeMap::from([
                ("device".to_owned(), ChannelParameterValue::Bool(false)),
                (
                    "baud_rate".to_owned(),
                    ChannelParameterValue::Integer(9_600),
                ),
            ]),
            true,
        ),
        protocol_definition(
            34,
            "modbus_rtu",
            BTreeMap::from([
                (
                    "device".to_owned(),
                    ChannelParameterValue::String("/dev/ttyUSB0".to_owned()),
                ),
                ("baud_rate".to_owned(), ChannelParameterValue::Integer(-1)),
            ]),
            true,
        ),
        protocol_definition(
            35,
            "sunspec_rtu",
            BTreeMap::from([
                (
                    "device".to_owned(),
                    ChannelParameterValue::String("/dev/ttyUSB0".to_owned()),
                ),
                (
                    "baud_rate".to_owned(),
                    ChannelParameterValue::Integer(4_294_967_296),
                ),
            ]),
            true,
        ),
    ];

    for definition in invalid {
        let error = mutator
            .mutate(ChannelMutation::create(definition))
            .await
            .expect_err("invalid endpoint must fail before desired-state commit");
        assert_eq!(error.kind(), PortErrorKind::InvalidData);
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM channels WHERE channel_id >= 30")
            .fetch_one(&pool)
            .await
            .expect("channel count"),
        0
    );
    assert!(runtime.calls().is_empty());
}

#[tokio::test]
async fn governed_virtual_mutations_reject_zero_poll_interval_before_commit_or_activation() {
    let pool = test_pool().await;
    let runtime = Arc::new(FakeRuntime::default());
    let mutator = adapter(pool.clone(), Arc::clone(&runtime));
    let poll_zero = BTreeMap::from([(
        "poll_interval_ms".to_owned(),
        ChannelParameterValue::Integer(0),
    )]);

    let create_error = mutator
        .mutate(ChannelMutation::create(protocol_definition(
            36,
            "virtual",
            poll_zero.clone(),
            true,
        )))
        .await
        .expect_err("zero poll create must fail before commit");
    assert_eq!(create_error.kind(), PortErrorKind::InvalidData);

    mutator
        .mutate(ChannelMutation::create(protocol_definition(
            37,
            "virtual",
            BTreeMap::new(),
            true,
        )))
        .await
        .expect("valid enabled channel");
    let calls_before_update = runtime.calls();
    let update_error = mutator
        .mutate(ChannelMutation::update_with_revision(
            ChannelId::new(37),
            ChannelRevision::new(1),
            ChannelPatch::new().with_parameters(poll_zero),
        ))
        .await
        .expect_err("zero poll update must fail before commit");
    assert_eq!(update_error.kind(), PortErrorKind::InvalidData);
    assert_eq!(runtime.calls(), calls_before_update);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT revision FROM channels WHERE channel_id = 37")
            .fetch_one(&pool)
            .await
            .expect("unchanged update revision"),
        1
    );

    mutator
        .mutate(ChannelMutation::create(protocol_definition(
            38,
            "virtual",
            BTreeMap::new(),
            false,
        )))
        .await
        .expect("valid disabled channel");
    sqlx::query(
        "UPDATE channels \
         SET config = '{\"parameters\":{\"poll_interval_ms\":0}}' \
         WHERE channel_id = 38",
    )
    .execute(&pool)
    .await
    .expect("stage invalid legacy desired state");
    let enable_error = mutator
        .mutate(ChannelMutation::enable_with_revision(
            ChannelId::new(38),
            ChannelRevision::new(2),
        ))
        .await
        .expect_err("zero poll enable must fail before desired-state commit");
    assert_eq!(enable_error.kind(), PortErrorKind::InvalidData);
    assert_eq!(
        sqlx::query_as::<_, (bool, i64)>(
            "SELECT enabled, revision FROM channels WHERE channel_id = 38"
        )
        .fetch_one(&pool)
        .await
        .expect("unchanged disabled desired state"),
        (false, 2)
    );
}

#[tokio::test]
async fn missing_revision_schema_is_permanent_and_never_starts_a_runtime() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("in-memory database");
    sqlx::query(
        "CREATE TABLE channels (\
             channel_id INTEGER PRIMARY KEY,\
             name TEXT NOT NULL UNIQUE,\
             protocol TEXT,\
             enabled INTEGER NOT NULL DEFAULT 0,\
             config TEXT,\
             created_at TEXT DEFAULT CURRENT_TIMESTAMP,\
             updated_at TEXT DEFAULT CURRENT_TIMESTAMP\
         )",
    )
    .execute(&pool)
    .await
    .expect("legacy schema");
    let runtime = Arc::new(FakeRuntime::default());
    let mutator = adapter(pool, Arc::clone(&runtime));

    let error = mutator
        .mutate(ChannelMutation::create(definition(19, true)))
        .await
        .expect_err("adapter requires schema v9");

    assert_eq!(error.kind(), PortErrorKind::Permanent);
    assert!(runtime.calls().is_empty());
}

#[tokio::test]
async fn runtime_protocol_validation_happens_before_persistence() {
    let pool = test_pool().await;
    let runtime = Arc::new(FakeRuntime::default());
    let mutator = adapter(pool.clone(), Arc::clone(&runtime));
    let definition = ChannelDefinition::new(
        Some(ChannelId::new(20)),
        "invalid-channel",
        "rejected",
        BTreeMap::new(),
    );

    let error = mutator
        .mutate(ChannelMutation::create(definition))
        .await
        .expect_err("runtime schema must reject channel");

    assert_eq!(error.kind(), PortErrorKind::InvalidData);
    assert!(runtime.calls().is_empty());
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM channels WHERE channel_id = 20")
        .fetch_one(&pool)
        .await
        .expect("channel count");
    assert_eq!(count, 0);
}

#[tokio::test]
async fn concurrent_auto_id_creates_are_serialized_and_receive_distinct_ids() {
    let pool = test_pool().await;
    let runtime = Arc::new(FakeRuntime::default());
    let mutator = Arc::new(adapter(pool.clone(), Arc::clone(&runtime)));
    let first = {
        let mutator = Arc::clone(&mutator);
        tokio::spawn(async move {
            mutator
                .mutate(ChannelMutation::create(auto_definition("auto-one")))
                .await
        })
    };
    let second = {
        let mutator = Arc::clone(&mutator);
        tokio::spawn(async move {
            mutator
                .mutate(ChannelMutation::create(auto_definition("auto-two")))
                .await
        })
    };

    let first = first.await.expect("first task").expect("first create");
    let second = second.await.expect("second task").expect("second create");
    assert_ne!(first.channel_id(), second.channel_id());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM channels")
            .fetch_one(&pool)
            .await
            .expect("channel count"),
        2
    );
    assert!(
        runtime.calls().is_empty(),
        "disabled creates must not activate"
    );
}

#[tokio::test]
async fn auto_id_allocation_uses_lowest_free_identity_without_reusing_tombstones() {
    let pool = test_pool().await;
    let runtime = Arc::new(FakeRuntime::default());
    let mutator = adapter(pool, runtime);
    mutator
        .mutate(ChannelMutation::create(definition(1, false)))
        .await
        .expect("create identity that will be tombstoned");
    mutator
        .mutate(ChannelMutation::delete_with_revision(
            ChannelId::new(1),
            ChannelRevision::new(1),
        ))
        .await
        .expect("tombstone identity one");
    mutator
        .mutate(ChannelMutation::create(definition(2, false)))
        .await
        .expect("create lower identity");
    mutator
        .mutate(ChannelMutation::create(definition(4, false)))
        .await
        .expect("create upper identity");

    let receipt = mutator
        .mutate(ChannelMutation::create(auto_definition("auto-after-gap")))
        .await
        .expect("allocate after current maximum");

    assert_eq!(receipt.channel_id(), ChannelId::new(3));
}

#[tokio::test]
async fn explicit_zero_channel_identity_remains_compatible_while_auto_allocation_starts_at_one() {
    let pool = test_pool().await;
    let runtime = Arc::new(FakeRuntime::default());
    let mutator = adapter(pool.clone(), runtime);

    let explicit = mutator
        .mutate(ChannelMutation::create(definition(0, false)))
        .await
        .expect("explicit zero remains a valid historical identity");
    assert_eq!(explicit.channel_id(), ChannelId::new(0));
    let automatic = mutator
        .mutate(ChannelMutation::create(auto_definition("automatic-one")))
        .await
        .expect("automatic allocation");
    assert_eq!(automatic.channel_id(), ChannelId::new(1));
}

#[tokio::test]
async fn manual_duplicate_identifier_and_name_are_conflicts_without_orphans() {
    let pool = test_pool().await;
    let runtime = Arc::new(FakeRuntime::default());
    let mutator = adapter(pool.clone(), Arc::clone(&runtime));
    mutator
        .mutate(ChannelMutation::create(definition(18, false)))
        .await
        .expect("create baseline channel");

    let duplicate_id = ChannelDefinition::new(
        Some(ChannelId::new(18)),
        "different-name",
        "virtual",
        BTreeMap::new(),
    );
    let error = mutator
        .mutate(ChannelMutation::create(duplicate_id))
        .await
        .expect_err("duplicate ID must conflict");
    assert_eq!(error.kind(), PortErrorKind::Conflict);

    let duplicate_name = ChannelDefinition::new(
        Some(ChannelId::new(19)),
        "channel-18",
        "virtual",
        BTreeMap::new(),
    );
    let error = mutator
        .mutate(ChannelMutation::create(duplicate_name))
        .await
        .expect_err("duplicate name must conflict");
    assert_eq!(error.kind(), PortErrorKind::Conflict);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM channels")
            .fetch_one(&pool)
            .await
            .expect("channel count"),
        1
    );
    assert!(runtime.calls().is_empty());
}

#[tokio::test]
async fn committed_activation_failure_is_an_accepted_degraded_projection() {
    let pool = test_pool().await;
    let runtime = Arc::new(FakeRuntime::default());
    runtime.fail_next_ensure();
    let mutator = adapter(pool.clone(), Arc::clone(&runtime));

    let receipt = mutator
        .mutate(ChannelMutation::create(definition(8, true)))
        .await
        .expect("desired state commit is accepted");

    assert_eq!(
        receipt.runtime_projection(),
        ChannelRuntimeProjection::Degraded
    );
    assert!(receipt.reconciliation_required());
    assert!(!runtime.is_active(8));
    let desired_enabled: bool =
        sqlx::query_scalar("SELECT enabled FROM channels WHERE channel_id = 8")
            .fetch_one(&pool)
            .await
            .expect("desired state");
    assert!(desired_enabled);
}

#[tokio::test]
async fn revision_trigger_covers_legacy_updates_without_double_incrementing_cas_updates() {
    let pool = test_pool().await;
    let runtime = Arc::new(FakeRuntime::default());
    let mutator = adapter(pool.clone(), runtime);
    mutator
        .mutate(ChannelMutation::create(definition(9, false)))
        .await
        .expect("create channel");

    sqlx::query("UPDATE channels SET name = 'legacy-writer' WHERE channel_id = 9")
        .execute(&pool)
        .await
        .expect("legacy update");
    let revision: i64 = sqlx::query_scalar("SELECT revision FROM channels WHERE channel_id = 9")
        .fetch_one(&pool)
        .await
        .expect("legacy revision");
    assert_eq!(revision, 2);

    let stale = mutator
        .mutate(ChannelMutation::update_with_revision(
            ChannelId::new(9),
            ChannelRevision::new(1),
            ChannelPatch::new().with_name("stale"),
        ))
        .await
        .expect_err("stale revision must conflict");
    assert_eq!(stale.kind(), PortErrorKind::Conflict);

    let receipt = mutator
        .mutate(ChannelMutation::update_with_revision(
            ChannelId::new(9),
            ChannelRevision::new(2),
            ChannelPatch::new().with_name("governed-writer"),
        ))
        .await
        .expect("current CAS update");
    assert_eq!(receipt.resulting_revision(), ChannelRevision::new(3));
    let revision: i64 = sqlx::query_scalar("SELECT revision FROM channels WHERE channel_id = 9")
        .fetch_one(&pool)
        .await
        .expect("governed revision");
    assert_eq!(revision, 3, "explicit +1 must not be bumped twice");
}

#[tokio::test]
async fn revisionless_compatibility_updates_are_serialized_by_channel() {
    let pool = test_pool().await;
    let runtime = Arc::new(FakeRuntime::default());
    let mutator = adapter(pool.clone(), runtime);
    mutator
        .mutate(ChannelMutation::create(definition(21, false)))
        .await
        .expect("create channel");

    let first = {
        let mutator = mutator.clone();
        tokio::spawn(async move {
            mutator
                .mutate(ChannelMutation::update(
                    ChannelId::new(21),
                    ChannelPatch::new().with_name("revisionless-a"),
                ))
                .await
        })
    };
    let second = {
        let mutator = mutator.clone();
        tokio::spawn(async move {
            mutator
                .mutate(ChannelMutation::update(
                    ChannelId::new(21),
                    ChannelPatch::new().with_name("revisionless-b"),
                ))
                .await
        })
    };

    first.await.expect("first task").expect("first update");
    second.await.expect("second task").expect("second update");
    let revision: i64 = sqlx::query_scalar("SELECT revision FROM channels WHERE channel_id = 21")
        .fetch_one(&pool)
        .await
        .expect("resulting revision");
    assert_eq!(revision, 3);
}

#[tokio::test]
async fn enable_reconciles_runtime_drift_even_when_desired_value_is_unchanged() {
    let pool = test_pool().await;
    let runtime = Arc::new(FakeRuntime::default());
    let mutator = adapter(pool, Arc::clone(&runtime));
    mutator
        .mutate(ChannelMutation::create(definition(10, true)))
        .await
        .expect("create enabled channel");
    runtime.set_active(10, false);

    let receipt = mutator
        .mutate(ChannelMutation::enable_with_revision(
            ChannelId::new(10),
            ChannelRevision::new(1),
        ))
        .await
        .expect("repair drift");

    assert!(runtime.is_active(10));
    assert_eq!(receipt.resulting_revision(), ChannelRevision::new(1));
    assert_eq!(
        receipt.runtime_projection(),
        ChannelRuntimeProjection::Active
    );
}

#[tokio::test]
async fn same_state_enable_rebuilds_a_present_but_stale_runtime_projection() {
    let pool = test_pool().await;
    let runtime = Arc::new(FakeRuntime::default());
    let mutator = adapter(pool.clone(), Arc::clone(&runtime));
    mutator
        .mutate(ChannelMutation::create(definition(18, true)))
        .await
        .expect("create enabled channel");
    runtime.set_runtime_name(18, "stale-runtime-name");
    let receipt = mutator
        .mutate(ChannelMutation::enable_with_revision(
            ChannelId::new(18),
            ChannelRevision::new(1),
        ))
        .await
        .expect("rebuild stale runtime");

    assert_eq!(receipt.resulting_revision(), ChannelRevision::new(1));
    assert_eq!(
        receipt.runtime_projection(),
        ChannelRuntimeProjection::Active
    );
    assert_eq!(
        runtime.config(18).expect("rebuilt runtime").core.name,
        "channel-18"
    );
    assert_eq!(
        runtime.calls(),
        vec!["activate:18", "fence:18", "activate:18"]
    );
    let revision: i64 = sqlx::query_scalar("SELECT revision FROM channels WHERE channel_id = 18")
        .fetch_one(&pool)
        .await
        .expect("desired revision");
    assert_eq!(revision, 1);
}

#[tokio::test]
async fn same_state_reconcile_reports_latest_desired_fact_when_legacy_writer_races() {
    let pool = test_pool().await;
    let runtime = Arc::new(FakeRuntime::default());
    let mutator = Arc::new(adapter(pool.clone(), Arc::clone(&runtime)));
    mutator
        .mutate(ChannelMutation::create(definition(25, true)))
        .await
        .expect("create enabled channel");
    let pause = runtime.pause_next_activation();

    let reconcile = {
        let mutator = Arc::clone(&mutator);
        tokio::spawn(async move {
            mutator
                .mutate(ChannelMutation::enable_with_revision(
                    ChannelId::new(25),
                    ChannelRevision::new(1),
                ))
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(2), pause.entered.notified())
        .await
        .expect("same-state activation reached pause");
    sqlx::query("UPDATE channels SET name = 'external-new-name' WHERE channel_id = 25")
        .execute(&pool)
        .await
        .expect("racing legacy desired update");
    pause.release.notify_one();

    let receipt = tokio::time::timeout(Duration::from_secs(2), reconcile)
        .await
        .expect("same-state reconcile completed after release")
        .expect("reconcile task")
        .expect("accepted reconcile");
    assert_eq!(receipt.resulting_revision(), ChannelRevision::new(2));
    assert!(receipt.desired_enabled());
    assert_eq!(
        receipt.runtime_projection(),
        ChannelRuntimeProjection::Degraded
    );
    assert!(receipt.reconciliation_required());
    assert!(!runtime.is_active(25));
    assert_eq!(
        runtime
            .config(25)
            .expect("last runtime projection")
            .core
            .name,
        "channel-25",
        "the stale runtime must be reported as degraded, never active"
    );
}

#[tokio::test]
async fn create_reports_latest_desired_fact_when_legacy_writer_races_activation() {
    let pool = test_pool().await;
    let runtime = Arc::new(FakeRuntime::default());
    let mutator = Arc::new(adapter(pool.clone(), Arc::clone(&runtime)));
    let pause = runtime.pause_next_activation();

    let create = {
        let mutator = Arc::clone(&mutator);
        tokio::spawn(async move {
            mutator
                .mutate(ChannelMutation::create(definition(27, true)))
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(2), pause.entered.notified())
        .await
        .expect("create activation reached pause");
    sqlx::query("UPDATE channels SET name = 'external-create-name' WHERE channel_id = 27")
        .execute(&pool)
        .await
        .expect("racing legacy desired update");
    pause.release.notify_one();

    let receipt = tokio::time::timeout(Duration::from_secs(2), create)
        .await
        .expect("create completed after release")
        .expect("create task")
        .expect("accepted create");
    assert_eq!(receipt.resulting_revision(), ChannelRevision::new(2));
    assert!(receipt.desired_enabled());
    assert_eq!(
        receipt.runtime_projection(),
        ChannelRuntimeProjection::Degraded
    );
    assert!(!runtime.is_active(27));
    assert_eq!(
        runtime
            .config(27)
            .expect("last runtime projection")
            .core
            .name,
        "channel-27"
    );
}

#[tokio::test]
async fn update_reports_latest_desired_fact_when_legacy_writer_races_activation() {
    let pool = test_pool().await;
    let runtime = Arc::new(FakeRuntime::default());
    let mutator = Arc::new(adapter(pool.clone(), Arc::clone(&runtime)));
    mutator
        .mutate(ChannelMutation::create(definition(28, true)))
        .await
        .expect("create enabled channel");
    let pause = runtime.pause_next_activation();

    let update = {
        let mutator = Arc::clone(&mutator);
        tokio::spawn(async move {
            mutator
                .mutate(ChannelMutation::update_with_revision(
                    ChannelId::new(28),
                    ChannelRevision::new(1),
                    ChannelPatch::new().with_name("governed-update-name"),
                ))
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(2), pause.entered.notified())
        .await
        .expect("update activation reached pause");
    sqlx::query("UPDATE channels SET name = 'external-update-name' WHERE channel_id = 28")
        .execute(&pool)
        .await
        .expect("racing legacy desired update");
    pause.release.notify_one();

    let receipt = tokio::time::timeout(Duration::from_secs(2), update)
        .await
        .expect("update completed after release")
        .expect("update task")
        .expect("accepted update");
    assert_eq!(receipt.resulting_revision(), ChannelRevision::new(3));
    assert!(receipt.desired_enabled());
    assert_eq!(
        receipt.runtime_projection(),
        ChannelRuntimeProjection::Degraded
    );
    assert!(!runtime.is_active(28));
    assert_eq!(
        runtime
            .config(28)
            .expect("last runtime projection")
            .core
            .name,
        "governed-update-name"
    );
}

#[tokio::test]
async fn enable_reports_latest_desired_fact_when_legacy_writer_races_activation() {
    let pool = test_pool().await;
    let runtime = Arc::new(FakeRuntime::default());
    let mutator = Arc::new(adapter(pool.clone(), Arc::clone(&runtime)));
    mutator
        .mutate(ChannelMutation::create(definition(29, false)))
        .await
        .expect("create disabled channel");
    let pause = runtime.pause_next_activation();

    let enable = {
        let mutator = Arc::clone(&mutator);
        tokio::spawn(async move {
            mutator
                .mutate(ChannelMutation::enable_with_revision(
                    ChannelId::new(29),
                    ChannelRevision::new(1),
                ))
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(2), pause.entered.notified())
        .await
        .expect("enable activation reached pause");
    sqlx::query("UPDATE channels SET name = 'external-enable-name' WHERE channel_id = 29")
        .execute(&pool)
        .await
        .expect("racing legacy desired update");
    pause.release.notify_one();

    let receipt = tokio::time::timeout(Duration::from_secs(2), enable)
        .await
        .expect("enable completed after release")
        .expect("enable task")
        .expect("accepted enable");
    assert_eq!(receipt.resulting_revision(), ChannelRevision::new(3));
    assert!(receipt.desired_enabled());
    assert_eq!(
        receipt.runtime_projection(),
        ChannelRuntimeProjection::Degraded
    );
    assert!(!runtime.is_active(29));
    assert_eq!(
        runtime
            .config(29)
            .expect("last runtime projection")
            .core
            .name,
        "channel-29"
    );
}

#[tokio::test]
async fn runtime_validation_blocks_enable_but_never_blocks_safe_disable() {
    let pool = test_pool().await;
    let runtime = Arc::new(FakeRuntime::default());
    let mutator = adapter(pool.clone(), Arc::clone(&runtime));
    mutator
        .mutate(ChannelMutation::create(definition(23, true)))
        .await
        .expect("create enabled channel");
    sqlx::query("UPDATE channels SET protocol = 'rejected' WHERE channel_id = 23")
        .execute(&pool)
        .await
        .expect("inject adapter validation failure");

    let disabled = mutator
        .mutate(ChannelMutation::disable_with_revision(
            ChannelId::new(23),
            ChannelRevision::new(2),
        ))
        .await
        .expect("safe disable must bypass activation validation");
    assert_eq!(disabled.resulting_revision(), ChannelRevision::new(3));
    assert_eq!(
        disabled.runtime_projection(),
        ChannelRuntimeProjection::Stopped
    );
    assert!(!runtime.is_active(23));

    let error = mutator
        .mutate(ChannelMutation::enable_with_revision(
            ChannelId::new(23),
            ChannelRevision::new(3),
        ))
        .await
        .expect_err("invalid runtime config must block enable");
    assert_eq!(error.kind(), PortErrorKind::InvalidData);
    let state: (bool, i64) =
        sqlx::query_as("SELECT enabled, revision FROM channels WHERE channel_id = 23")
            .fetch_one(&pool)
            .await
            .expect("unchanged disabled desired state");
    assert_eq!(state, (false, 3));
}

#[tokio::test]
async fn malformed_persisted_config_never_blocks_safe_disable() {
    let pool = test_pool().await;
    let runtime = Arc::new(FakeRuntime::default());
    let mutator = adapter(pool.clone(), Arc::clone(&runtime));
    mutator
        .mutate(ChannelMutation::create(definition(30, true)))
        .await
        .expect("create enabled channel");
    sqlx::query("UPDATE channels SET config = '{' WHERE channel_id = 30")
        .execute(&pool)
        .await
        .expect("inject malformed legacy config");

    let receipt = mutator
        .mutate(ChannelMutation::disable_with_revision(
            ChannelId::new(30),
            ChannelRevision::new(2),
        ))
        .await
        .expect("safe disable must not decode activation config");

    assert_eq!(receipt.resulting_revision(), ChannelRevision::new(3));
    assert!(!receipt.desired_enabled());
    assert_eq!(
        receipt.runtime_projection(),
        ChannelRuntimeProjection::Stopped
    );
    assert!(!runtime.is_active(30));
}

#[tokio::test]
async fn malformed_persisted_config_never_blocks_safe_delete() {
    let pool = test_pool().await;
    let runtime = Arc::new(FakeRuntime::default());
    let mutator = adapter(pool.clone(), Arc::clone(&runtime));
    mutator
        .mutate(ChannelMutation::create(definition(31, true)))
        .await
        .expect("create enabled channel");
    sqlx::query("UPDATE channels SET config = '{' WHERE channel_id = 31")
        .execute(&pool)
        .await
        .expect("inject malformed legacy config");

    let receipt = mutator
        .mutate(ChannelMutation::delete_with_revision(
            ChannelId::new(31),
            ChannelRevision::new(2),
        ))
        .await
        .expect("safe delete must not decode activation config");

    assert_eq!(receipt.resulting_revision(), ChannelRevision::new(3));
    assert_eq!(
        receipt.runtime_projection(),
        ChannelRuntimeProjection::Removed
    );
    assert!(!runtime.is_active(31));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM channels WHERE channel_id = 31")
            .fetch_one(&pool)
            .await
            .expect("channel count"),
        0
    );
}

#[tokio::test]
async fn update_commits_desired_config_and_reports_runtime_reconcile_failure() {
    let pool = test_pool().await;
    let runtime = Arc::new(FakeRuntime::default());
    let mutator = adapter(pool.clone(), Arc::clone(&runtime));
    mutator
        .mutate(ChannelMutation::create(definition(11, true)))
        .await
        .expect("create enabled channel");
    runtime.fail_next_ensure();

    let receipt = mutator
        .mutate(ChannelMutation::update_with_revision(
            ChannelId::new(11),
            ChannelRevision::new(1),
            ChannelPatch::new().with_name("updated-channel"),
        ))
        .await
        .expect("committed update");

    assert_eq!(
        receipt.runtime_projection(),
        ChannelRuntimeProjection::Degraded
    );
    assert!(receipt.reconciliation_required());
    assert!(!runtime.is_active(11));
    let name: String = sqlx::query_scalar("SELECT name FROM channels WHERE channel_id = 11")
        .fetch_one(&pool)
        .await
        .expect("updated name");
    assert_eq!(name, "updated-channel");
}

#[tokio::test]
async fn update_merges_parameter_keys_replaces_logging_and_restores_typed_runtime_config() {
    let pool = test_pool().await;
    let runtime = Arc::new(FakeRuntime::default());
    let mutator = adapter(pool.clone(), Arc::clone(&runtime));
    mutator
        .mutate(ChannelMutation::create(definition(17, false)))
        .await
        .expect("create disabled channel");

    let receipt = mutator
        .mutate(ChannelMutation::update_with_revision(
            ChannelId::new(17),
            ChannelRevision::new(1),
            ChannelPatch::new()
                .with_parameters(BTreeMap::from([(
                    "port".to_owned(),
                    ChannelParameterValue::Integer(502),
                )]))
                .with_logging(
                    ChannelLoggingPolicy::default()
                        .with_enabled(true)
                        .with_level("warn"),
                ),
        ))
        .await
        .expect("merge channel parameter keys");
    assert_eq!(receipt.resulting_revision(), ChannelRevision::new(2));

    let config_json: String =
        sqlx::query_scalar("SELECT config FROM channels WHERE channel_id = 17")
            .fetch_one(&pool)
            .await
            .expect("stored config");
    let config: serde_json::Value = serde_json::from_str(&config_json).expect("config JSON");
    assert_eq!(config["parameters"]["credential"], "never-log-secret");
    assert_eq!(config["parameters"]["nested"]["retry"], 3);
    assert_eq!(config["parameters"]["port"], 502);
    assert_eq!(config["logging"]["enabled"], true);
    assert_eq!(config["logging"]["level"], "warn");
    assert!(config["logging"]["file"].is_null());

    let enabled = mutator
        .mutate(ChannelMutation::enable_with_revision(
            ChannelId::new(17),
            ChannelRevision::new(2),
        ))
        .await
        .expect("activate persisted typed config");
    assert_eq!(enabled.resulting_revision(), ChannelRevision::new(3));
    let runtime_config = runtime.config(17).expect("runtime config");
    assert_eq!(runtime_config.parameters["credential"], "never-log-secret");
    assert_eq!(runtime_config.parameters["port"], 502);
    assert!(runtime_config.logging.enabled);
    assert_eq!(runtime_config.logging.level.as_deref(), Some("warn"));
    assert_eq!(runtime_config.logging.file, None);
}

#[tokio::test]
async fn disable_fences_first_and_restores_runtime_when_database_rejects_commit() {
    let pool = test_pool().await;
    let runtime = Arc::new(FakeRuntime::default());
    let mutator = adapter(pool.clone(), Arc::clone(&runtime));
    mutator
        .mutate(ChannelMutation::create(definition(12, true)))
        .await
        .expect("create enabled channel");
    sqlx::query(
        "CREATE TRIGGER reject_channel_disable BEFORE UPDATE OF enabled ON channels \
         WHEN OLD.channel_id = 12 AND NEW.enabled = 0 \
         BEGIN SELECT RAISE(ABORT, 'injected desired-state failure'); END",
    )
    .execute(&pool)
    .await
    .expect("failure trigger");

    let error = mutator
        .mutate(ChannelMutation::disable_with_revision(
            ChannelId::new(12),
            ChannelRevision::new(1),
        ))
        .await
        .expect_err("database failure must reject mutation");

    assert_eq!(error.kind(), PortErrorKind::Permanent);
    assert!(
        runtime.is_active(12),
        "previous desired runtime must be restored"
    );
    assert_eq!(
        runtime.calls(),
        vec!["activate:12", "fence:12", "activate:12"]
    );
    let enabled: bool = sqlx::query_scalar("SELECT enabled FROM channels WHERE channel_id = 12")
        .fetch_one(&pool)
        .await
        .expect("unchanged desired state");
    assert!(enabled);
}

#[tokio::test]
async fn disable_conflict_never_restores_stale_runtime_over_newer_disabled_desired_state() {
    let pool = test_pool().await;
    let runtime = Arc::new(FakeRuntime::default());
    let mutator = Arc::new(adapter(pool.clone(), Arc::clone(&runtime)));
    mutator
        .mutate(ChannelMutation::create(definition(32, true)))
        .await
        .expect("create enabled channel");
    let pause = runtime.pause_next_fence();

    let disable = {
        let mutator = Arc::clone(&mutator);
        tokio::spawn(async move {
            mutator
                .mutate(ChannelMutation::disable_with_revision(
                    ChannelId::new(32),
                    ChannelRevision::new(1),
                ))
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(2), pause.entered.notified())
        .await
        .expect("disable fence reached pause");
    sqlx::query("UPDATE channels SET enabled = 0 WHERE channel_id = 32")
        .execute(&pool)
        .await
        .expect("newer legacy disable");
    pause.release.notify_one();

    let error = tokio::time::timeout(Duration::from_secs(2), disable)
        .await
        .expect("disable completed after release")
        .expect("disable task")
        .expect_err("stale CAS must conflict");
    assert_eq!(error.kind(), PortErrorKind::Conflict);
    assert!(!runtime.is_active(32), "stale runtime must stay fenced");
    assert_eq!(
        sqlx::query_as::<_, (bool, i64)>(
            "SELECT enabled, revision FROM channels WHERE channel_id = 32",
        )
        .fetch_one(&pool)
        .await
        .expect("latest desired state"),
        (false, 2)
    );
}

#[tokio::test]
async fn delete_conflict_never_restores_runtime_for_a_recreated_identity() {
    let pool = test_pool().await;
    let runtime = Arc::new(FakeRuntime::default());
    let mutator = Arc::new(adapter(pool.clone(), Arc::clone(&runtime)));
    mutator
        .mutate(ChannelMutation::create(definition(33, true)))
        .await
        .expect("create enabled channel");
    let pause = runtime.pause_next_fence();

    let delete = {
        let mutator = Arc::clone(&mutator);
        tokio::spawn(async move {
            mutator
                .mutate(ChannelMutation::delete_with_revision(
                    ChannelId::new(33),
                    ChannelRevision::new(1),
                ))
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(2), pause.entered.notified())
        .await
        .expect("delete fence reached pause");
    sqlx::query("DELETE FROM channels WHERE channel_id = 33")
        .execute(&pool)
        .await
        .expect("external delete");
    sqlx::query(
        "INSERT INTO channels (channel_id, name, protocol, enabled, config) \
         VALUES (33, 'replacement', 'virtual', 0, '{}')",
    )
    .execute(&pool)
    .await
    .expect("external replacement");
    pause.release.notify_one();

    let error = tokio::time::timeout(Duration::from_secs(2), delete)
        .await
        .expect("delete completed after release")
        .expect("delete task")
        .expect_err("old entity delete must conflict with replacement");
    assert_eq!(error.kind(), PortErrorKind::Conflict);
    assert!(
        !runtime.is_active(33),
        "the old runtime must not be restored for a replacement identity"
    );
    assert_eq!(
        sqlx::query_as::<_, (String, bool, i64)>(
            "SELECT name, enabled, revision FROM channels WHERE channel_id = 33",
        )
        .fetch_one(&pool)
        .await
        .expect("replacement desired state"),
        ("replacement".to_owned(), false, 3)
    );
}

#[tokio::test]
async fn delete_refuses_action_routes_without_fencing_or_cascading_them() {
    let pool = test_pool().await;
    let runtime = Arc::new(FakeRuntime::default());
    let mutator = adapter(pool.clone(), Arc::clone(&runtime));
    mutator
        .mutate(ChannelMutation::create(definition(13, true)))
        .await
        .expect("create enabled channel");
    sqlx::query(
        "INSERT INTO instances (instance_id, instance_name, product_name) \
         VALUES (1, 'instance', 'ExampleDevice')",
    )
    .execute(&pool)
    .await
    .expect("instance");
    sqlx::query(
        "INSERT INTO action_routing \
         (instance_id, instance_name, action_id, channel_id, channel_type, channel_point_id) \
         VALUES (1, 'instance', 1, 13, 'C', 1)",
    )
    .execute(&pool)
    .await
    .expect("action route");
    let calls_before = runtime.calls();

    let error = mutator
        .mutate(ChannelMutation::delete_with_revision(
            ChannelId::new(13),
            ChannelRevision::new(1),
        ))
        .await
        .expect_err("action route must block deletion");

    assert_eq!(error.kind(), PortErrorKind::Conflict);
    assert_eq!(
        runtime.calls(),
        calls_before,
        "conflict must not fence runtime"
    );
    assert!(runtime.is_active(13));
    let channels: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM channels WHERE channel_id = 13")
        .fetch_one(&pool)
        .await
        .expect("channel count");
    let routes: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM action_routing")
        .fetch_one(&pool)
        .await
        .expect("route count");
    assert_eq!((channels, routes), (1, 1));
}

#[tokio::test]
async fn delete_is_atomic_for_owned_rows_and_leaves_no_runtime_zombie() {
    let pool = test_pool().await;
    let runtime = Arc::new(FakeRuntime::default());
    let mutator = adapter(pool.clone(), Arc::clone(&runtime));
    mutator
        .mutate(ChannelMutation::create(definition(14, true)))
        .await
        .expect("create enabled channel");
    sqlx::query(
        "INSERT INTO telemetry_points \
         (point_id, channel_id, signal_name, data_type) VALUES (1, 14, 'measurement', 'f64')",
    )
    .execute(&pool)
    .await
    .expect("point");
    sqlx::query(
        "INSERT INTO instances (instance_id, instance_name, product_name) \
         VALUES (2, 'measurement-instance', 'ExampleDevice')",
    )
    .execute(&pool)
    .await
    .expect("instance");
    sqlx::query(
        "INSERT INTO measurement_routing \
         (instance_id, instance_name, channel_id, channel_type, channel_point_id, measurement_id) \
         VALUES (2, 'measurement-instance', 14, 'T', 1, 1)",
    )
    .execute(&pool)
    .await
    .expect("measurement route");
    sqlx::query(
        "CREATE TABLE point_mappings (\
             mapping_id INTEGER PRIMARY KEY AUTOINCREMENT,\
             channel_id INTEGER NOT NULL\
         )",
    )
    .execute(&pool)
    .await
    .expect("legacy measurement mapping schema");
    sqlx::query("INSERT INTO point_mappings (channel_id) VALUES (14)")
        .execute(&pool)
        .await
        .expect("legacy measurement mapping");
    sqlx::query(
        "CREATE TABLE channel_routing (\
             source_channel_id INTEGER NOT NULL,\
             target_channel_id INTEGER NOT NULL\
         )",
    )
    .execute(&pool)
    .await
    .expect("channel routing schema");
    sqlx::query(
        "INSERT INTO channel_routing (source_channel_id, target_channel_id) VALUES (14, 99)",
    )
    .execute(&pool)
    .await
    .expect("channel routing row");

    let receipt = mutator
        .mutate(ChannelMutation::delete_with_revision(
            ChannelId::new(14),
            ChannelRevision::new(1),
        ))
        .await
        .expect("delete channel");

    assert_eq!(
        receipt.runtime_projection(),
        ChannelRuntimeProjection::Removed
    );
    assert!(!runtime.is_active(14));
    for table in [
        "channels",
        "telemetry_points",
        "measurement_routing",
        "point_mappings",
        "channel_routing",
    ] {
        let predicate = if table == "channel_routing" {
            "source_channel_id = 14 OR target_channel_id = 14"
        } else {
            "channel_id = 14"
        };
        let count: i64 =
            sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table} WHERE {predicate}"))
                .fetch_one(&pool)
                .await
                .expect("owned row count");
        assert_eq!(count, 0, "orphan rows remain in {table}");
    }
}

#[tokio::test]
async fn delete_and_recreate_advances_revision_so_old_cas_tokens_cannot_hit_new_entity() {
    let pool = test_pool().await;
    let runtime = Arc::new(FakeRuntime::default());
    let mutator = adapter(pool.clone(), runtime);
    let created = mutator
        .mutate(ChannelMutation::create(definition(26, false)))
        .await
        .expect("create first entity");
    assert_eq!(created.resulting_revision(), ChannelRevision::new(1));

    let deleted = mutator
        .mutate(ChannelMutation::delete_with_revision(
            ChannelId::new(26),
            ChannelRevision::new(1),
        ))
        .await
        .expect("delete first entity");
    assert_eq!(deleted.resulting_revision(), ChannelRevision::new(2));
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT last_revision FROM channel_revision_tombstones WHERE channel_id = 26",
        )
        .fetch_one(&pool)
        .await
        .expect("durable tombstone"),
        2
    );

    let recreated = mutator
        .mutate(ChannelMutation::create(definition(26, false)))
        .await
        .expect("recreate identity as a new entity");
    assert_eq!(recreated.resulting_revision(), ChannelRevision::new(3));

    for stale in [ChannelRevision::new(1), ChannelRevision::new(2)] {
        let error = mutator
            .mutate(ChannelMutation::update_with_revision(
                ChannelId::new(26),
                stale,
                ChannelPatch::new().with_name("must-not-hit-new-entity"),
            ))
            .await
            .expect_err("old CAS token must not match recreated entity");
        assert_eq!(error.kind(), PortErrorKind::Conflict);
    }
    let state: (String, i64) =
        sqlx::query_as("SELECT name, revision FROM channels WHERE channel_id = 26")
            .fetch_one(&pool)
            .await
            .expect("recreated state");
    assert_eq!(state, ("channel-26".to_owned(), 3));
}

#[tokio::test]
async fn delete_database_failure_rolls_back_owned_rows_and_restores_runtime() {
    let pool = test_pool().await;
    let runtime = Arc::new(FakeRuntime::default());
    let mutator = adapter(pool.clone(), Arc::clone(&runtime));
    mutator
        .mutate(ChannelMutation::create(definition(22, true)))
        .await
        .expect("create enabled channel");
    sqlx::query(
        "INSERT INTO telemetry_points \
         (point_id, channel_id, signal_name, data_type) VALUES (1, 22, 'measurement', 'f64')",
    )
    .execute(&pool)
    .await
    .expect("point");
    sqlx::query(
        "CREATE TRIGGER reject_channel_delete BEFORE DELETE ON channels \
         WHEN OLD.channel_id = 22 \
         BEGIN SELECT RAISE(ABORT, 'injected channel delete failure'); END",
    )
    .execute(&pool)
    .await
    .expect("failure trigger");

    let error = mutator
        .mutate(ChannelMutation::delete_with_revision(
            ChannelId::new(22),
            ChannelRevision::new(1),
        ))
        .await
        .expect_err("delete must roll back");

    assert_eq!(error.kind(), PortErrorKind::Permanent);
    assert!(runtime.is_active(22));
    assert_eq!(
        runtime.calls(),
        vec!["activate:22", "fence:22", "activate:22"]
    );
    for table in ["channels", "telemetry_points"] {
        let count: i64 = sqlx::query_scalar(&format!(
            "SELECT COUNT(*) FROM {table} WHERE channel_id = 22"
        ))
        .fetch_one(&pool)
        .await
        .expect("rolled-back row count");
        assert_eq!(count, 1, "{table} deletion escaped rollback");
    }
}

#[tokio::test]
async fn fence_failure_prevents_disable_commit() {
    let pool = test_pool().await;
    let runtime = Arc::new(FakeRuntime::default());
    let mutator = adapter(pool.clone(), Arc::clone(&runtime));
    mutator
        .mutate(ChannelMutation::create(definition(15, true)))
        .await
        .expect("create enabled channel");
    runtime.fail_next_fence();

    let error = mutator
        .mutate(ChannelMutation::disable_with_revision(
            ChannelId::new(15),
            ChannelRevision::new(1),
        ))
        .await
        .expect_err("unfenced runtime must prevent commit");

    assert_eq!(error.kind(), PortErrorKind::Unavailable);
    let (enabled, revision): (bool, i64) =
        sqlx::query_as("SELECT enabled, revision FROM channels WHERE channel_id = 15")
            .fetch_one(&pool)
            .await
            .expect("desired state");
    assert!(enabled);
    assert_eq!(revision, 1);
}

#[tokio::test]
async fn exhausted_revision_fails_permanently_without_mutating_state() {
    let pool = test_pool().await;
    let runtime = Arc::new(FakeRuntime::default());
    let mutator = adapter(pool.clone(), runtime);
    mutator
        .mutate(ChannelMutation::create(definition(16, false)))
        .await
        .expect("create channel");
    sqlx::query("UPDATE channels SET revision = 9223372036854775807 WHERE channel_id = 16")
        .execute(&pool)
        .await
        .expect("exhaust revision");

    let error = mutator
        .mutate(ChannelMutation::update_with_revision(
            ChannelId::new(16),
            ChannelRevision::new(i64::MAX as u64),
            ChannelPatch::new().with_name("must-not-commit"),
        ))
        .await
        .expect_err("revision exhaustion");

    assert_eq!(error.kind(), PortErrorKind::Permanent);
    let name: String = sqlx::query_scalar("SELECT name FROM channels WHERE channel_id = 16")
        .fetch_one(&pool)
        .await
        .expect("unchanged channel");
    assert_eq!(name, "channel-16");
}

#[tokio::test]
async fn bulk_reconciliation_converges_desired_channels_and_orphan_runtime() {
    let pool = test_pool().await;
    let runtime = Arc::new(FakeRuntime::default());
    let adapter = adapter(pool, Arc::clone(&runtime));
    adapter
        .mutate(ChannelMutation::create(definition(7, true)))
        .await
        .expect("enabled channel");
    adapter
        .mutate(ChannelMutation::create(definition(8, false)))
        .await
        .expect("disabled channel");
    runtime.set_runtime_name(7, "stale-runtime");
    runtime.set_active(9, true);

    let receipt = adapter
        .reconcile(ChannelReconciliationScope::All)
        .await
        .expect("bulk reconciliation");

    assert_eq!(receipt.scope(), ChannelReconciliationScope::All);
    assert_eq!(
        receipt
            .items()
            .iter()
            .map(|item| item.channel_id().get())
            .collect::<Vec<_>>(),
        vec![7, 8, 9]
    );
    assert_eq!(
        receipt.items()[0].desired(),
        ChannelDesiredStateObservation::present(ChannelRevision::new(1), true)
    );
    assert_eq!(
        receipt.items()[0].runtime_projection(),
        ChannelRuntimeProjection::Active
    );
    assert_eq!(
        receipt.items()[1].runtime_projection(),
        ChannelRuntimeProjection::Stopped
    );
    assert_eq!(
        receipt.items()[2].desired(),
        ChannelDesiredStateObservation::absent(None)
    );
    assert_eq!(
        receipt.items()[2].runtime_projection(),
        ChannelRuntimeProjection::Removed
    );
    assert_eq!(
        runtime.config(7).expect("fresh config").core.name,
        "channel-7"
    );
    assert!(runtime.is_active(7));
    assert!(!runtime.is_active(8));
    assert!(!runtime.is_active(9));
}

#[tokio::test]
async fn invalid_desired_runtime_is_degraded_without_stopping_other_channels() {
    let pool = test_pool().await;
    let runtime = Arc::new(FakeRuntime::default());
    let adapter = adapter(pool.clone(), Arc::clone(&runtime));
    for (channel_id, protocol) in [(10_i64, "rejected"), (11, "virtual")] {
        sqlx::query(
            "INSERT INTO channels \
             (channel_id, name, protocol, enabled, config, revision) \
             VALUES (?, ?, ?, 1, '{}', 1)",
        )
        .bind(channel_id)
        .bind(format!("channel-{channel_id}"))
        .bind(protocol)
        .execute(&pool)
        .await
        .expect("desired channel");
    }

    let receipt = adapter
        .reconcile(ChannelReconciliationScope::All)
        .await
        .expect("bulk accepts per-channel degradation");

    assert_eq!(receipt.degraded_count(), 1);
    assert_eq!(
        receipt.items()[0].runtime_projection(),
        ChannelRuntimeProjection::Degraded
    );
    assert_eq!(
        receipt.items()[1].runtime_projection(),
        ChannelRuntimeProjection::Active
    );
    assert!(!runtime.is_active(10));
    assert!(runtime.is_active(11));
}

#[tokio::test]
async fn single_reconciliation_and_mutation_share_the_channel_lifecycle_gate() {
    let pool = test_pool().await;
    let runtime = Arc::new(FakeRuntime::default());
    let adapter = Arc::new(adapter(pool, Arc::clone(&runtime)));
    adapter
        .mutate(ChannelMutation::create(definition(12, true)))
        .await
        .expect("channel");
    let pause = runtime.pause_next_activation();

    let reconcile = {
        let adapter = Arc::clone(&adapter);
        tokio::spawn(async move {
            adapter
                .reconcile(ChannelReconciliationScope::One(ChannelId::new(12)))
                .await
        })
    };
    pause.entered.notified().await;
    let update = {
        let adapter = Arc::clone(&adapter);
        tokio::spawn(async move {
            adapter
                .mutate(ChannelMutation::update_with_revision(
                    ChannelId::new(12),
                    ChannelRevision::new(1),
                    ChannelPatch::new().with_name("latest-name"),
                ))
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !update.is_finished(),
        "mutation escaped reconciliation gate"
    );

    pause.release.notify_one();
    reconcile
        .await
        .expect("reconciliation task")
        .expect("reconciliation");
    update.await.expect("update task").expect("update");

    assert_eq!(
        runtime.config(12).expect("latest config").core.name,
        "latest-name"
    );
}

#[tokio::test]
async fn one_absent_channel_reconciliation_reports_tombstone_and_fences_runtime() {
    let pool = test_pool().await;
    let runtime = Arc::new(FakeRuntime::default());
    let adapter = adapter(pool.clone(), Arc::clone(&runtime));
    sqlx::query(
        "INSERT INTO channel_revision_tombstones (channel_id, last_revision) VALUES (13, 4)",
    )
    .execute(&pool)
    .await
    .expect("tombstone");
    runtime.set_active(13, true);

    let receipt = adapter
        .reconcile(ChannelReconciliationScope::One(ChannelId::new(13)))
        .await
        .expect("single reconciliation");

    assert_eq!(receipt.items().len(), 1);
    assert_eq!(
        receipt.items()[0].desired(),
        ChannelDesiredStateObservation::absent(Some(ChannelRevision::new(4)))
    );
    assert_eq!(
        receipt.items()[0].runtime_projection(),
        ChannelRuntimeProjection::Removed
    );
    assert!(!runtime.is_active(13));
}
