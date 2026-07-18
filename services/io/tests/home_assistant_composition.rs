use std::collections::BTreeMap;

use aether_domain::{
    EntityId, EntityPointDescriptor, EntityRecord, GatewayIdentity, IntegrationId,
    IntegrationObservation, IntegrationPointKey, IntegrationPointKind, IntegrationSnapshot,
    IntegrationTopologySnapshot, ObservedValue, ObservedValueType, SnapshotDigest, TimestampMs,
    TopologyGeneration,
};
use aether_io::home_assistant::{
    HomeAssistantCloudLinkOriginModel, HomeAssistantRuntimeConfig, HomeAssistantStartupError,
    InMemoryIntegrationProjection, start_home_assistant_integration_with_config,
};
use aether_ports::{IntegrationProjectionQuery, IntegrationProjectionSink, PortErrorKind};
use tokio_util::sync::CancellationToken;

const ENABLED: &str = "AETHER_HOME_ASSISTANT_ENABLED";
const ORIGIN: &str = "AETHER_HOME_ASSISTANT_ORIGIN";
const SECRET_REF: &str = "AETHER_HOME_ASSISTANT_ACCESS_TOKEN_REF";
const PLAINTEXT_TOKEN: &str = "AETHER_HOME_ASSISTANT_ACCESS_TOKEN";
const GATEWAY_ID: &str = "AETHER_GATEWAY_ID";
const INTEGRATION_ID: &str = "AETHER_HOME_ASSISTANT_INTEGRATION_ID";
const GENERATION_STORE_PATH: &str = "AETHER_HOME_ASSISTANT_GENERATION_STORE_PATH";
const CLOUDLINK_ENABLED: &str = "AETHER_HOME_ASSISTANT_CLOUDLINK_ENABLED";
#[cfg(feature = "home-assistant-cloudlink")]
const CLOUDLINK_ORIGIN_MODEL: &str = "AETHER_HOME_ASSISTANT_CLOUDLINK_ORIGIN_MODEL";
#[cfg(feature = "home-assistant-cloudlink")]
const CLOUDLINK_CLOUD_KEY_ID: &str = "AETHER_HOME_ASSISTANT_CLOUDLINK_CLOUD_KEY_ID";
#[cfg(feature = "home-assistant-cloudlink")]
const CLOUDLINK_CLOUD_PUBLIC_KEY_REF: &str = "AETHER_HOME_ASSISTANT_CLOUDLINK_CLOUD_PUBLIC_KEY_REF";
#[cfg(feature = "home-assistant-cloudlink")]
const CLOUDLINK_GATEWAY_KEY_ID: &str = "AETHER_HOME_ASSISTANT_CLOUDLINK_GATEWAY_KEY_ID";
#[cfg(feature = "home-assistant-cloudlink")]
const CLOUDLINK_GATEWAY_SIGNING_KEY_REF: &str =
    "AETHER_HOME_ASSISTANT_CLOUDLINK_GATEWAY_SIGNING_KEY_REF";
#[cfg(feature = "home-assistant-cloudlink")]
const CLOUDLINK_CHALLENGE_LEDGER_PATH: &str =
    "AETHER_HOME_ASSISTANT_CLOUDLINK_CHALLENGE_LEDGER_PATH";
const CONTROL_ENABLED: &str = "AETHER_HOME_ASSISTANT_CONTROL_ENABLED";
#[cfg(feature = "home-assistant-integration-control")]
const CONTROL_CLOUD_EXTENSION: &str = "AETHER_HOME_ASSISTANT_CONTROL_CLOUD_EXTENSION";
#[cfg(feature = "home-assistant-integration-control")]
const CONTROL_LEDGER_PATH: &str = "AETHER_HOME_ASSISTANT_CONTROL_LEDGER_PATH";
#[cfg(feature = "home-assistant-integration-control")]
const CONTROL_POLICY_PATH: &str = "AETHER_HOME_ASSISTANT_CONTROL_POLICY_PATH";
#[cfg(feature = "home-assistant-integration-control")]
const CONTROL_AUDIT_PATH: &str = "AETHER_HOME_ASSISTANT_CONTROL_AUDIT_PATH";
#[cfg(feature = "home-assistant-integration-control")]
const CONTROL_CLOUD_KEY_ID: &str = "AETHER_HOME_ASSISTANT_CONTROL_CLOUD_KEY_ID";
#[cfg(feature = "home-assistant-integration-control")]
const CONTROL_CLOUD_PUBLIC_KEY_REF: &str = "AETHER_HOME_ASSISTANT_CONTROL_CLOUD_PUBLIC_KEY_REF";
#[cfg(feature = "home-assistant-integration-control")]
const CONTROL_EDGE_KEY_ID: &str = "AETHER_HOME_ASSISTANT_CONTROL_EDGE_KEY_ID";
#[cfg(feature = "home-assistant-integration-control")]
const CONTROL_EDGE_SIGNING_KEY_REF: &str = "AETHER_HOME_ASSISTANT_CONTROL_EDGE_SIGNING_KEY_REF";
#[cfg(feature = "home-assistant-cloudlink")]
const CLOUDLINK_RUNTIME_CONFIG_DIR: &str = "AETHER_HOME_ASSISTANT_CLOUDLINK_RUNTIME_CONFIG_DIR";
#[cfg(feature = "home-assistant-cloudlink")]
const CLOUDLINK_CLOUD_EXTENSION: &str = "AETHER_HOME_ASSISTANT_CLOUDLINK_CLOUD_EXTENSION";
#[cfg(feature = "home-assistant-cloudlink")]
const CLOUDLINK_TOPOLOGY_SPOOL_PATH: &str = "AETHER_HOME_ASSISTANT_CLOUDLINK_TOPOLOGY_SPOOL_PATH";
#[cfg(feature = "home-assistant-cloudlink")]
const CLOUDLINK_OBSERVATION_SPOOL_PATH: &str =
    "AETHER_HOME_ASSISTANT_CLOUDLINK_OBSERVATION_SPOOL_PATH";
#[cfg(feature = "home-assistant-cloudlink")]
const CLOUDLINK_MQTT_BROKER_HOST: &str = "AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_BROKER_HOST";
#[cfg(feature = "home-assistant-cloudlink")]
const CLOUDLINK_MQTT_BROKER_PORT: &str = "AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_BROKER_PORT";
#[cfg(feature = "home-assistant-cloudlink")]
const CLOUDLINK_MQTT_CLIENT_ID: &str = "AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_CLIENT_ID";
#[cfg(feature = "home-assistant-cloudlink")]
const CLOUDLINK_MQTT_TOPIC_PREFIX: &str = "AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_TOPIC_PREFIX";
#[cfg(feature = "home-assistant-cloudlink")]
const CLOUDLINK_MQTT_USERNAME: &str = "AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_USERNAME";
#[cfg(feature = "home-assistant-cloudlink")]
const CLOUDLINK_MQTT_PASSWORD_REF: &str = "AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_PASSWORD_REF";
#[cfg(feature = "home-assistant-cloudlink")]
const CLOUDLINK_CREDENTIAL_ID: &str = "AETHER_HOME_ASSISTANT_CLOUDLINK_CREDENTIAL_ID";
#[cfg(feature = "home-assistant-cloudlink")]
const CLOUDLINK_CREDENTIAL_GENERATION: &str =
    "AETHER_HOME_ASSISTANT_CLOUDLINK_CREDENTIAL_GENERATION";
#[cfg(feature = "home-assistant-cloudlink")]
const CLOUDLINK_SESSION_EPOCH_PATH: &str = "AETHER_HOME_ASSISTANT_CLOUDLINK_SESSION_EPOCH_PATH";
const TEST_GENERATION_STORE_PATH: &str = "/var/lib/aether/home-assistant-topology-generations.json";

fn config(
    values: &[(&str, &str)],
) -> Result<HomeAssistantRuntimeConfig, HomeAssistantStartupError> {
    let values = values.iter().copied().collect::<BTreeMap<_, _>>();
    HomeAssistantRuntimeConfig::from_lookup(|name| {
        values.get(name).map(|value| (*value).to_owned())
    })
}

#[cfg(feature = "home-assistant-cloudlink")]
fn cloudlink_values(root: &std::path::Path) -> BTreeMap<String, String> {
    BTreeMap::from([
        (ENABLED.to_owned(), "true".to_owned()),
        (
            ORIGIN.to_owned(),
            "http://homeassistant.local:8123".to_owned(),
        ),
        (SECRET_REF.to_owned(), "env:HOME_ASSISTANT_TOKEN".to_owned()),
        (
            GATEWAY_ID.to_owned(),
            "33333333-3333-4333-8333-333333333333".to_owned(),
        ),
        (
            GENERATION_STORE_PATH.to_owned(),
            root.join("generations.json").to_string_lossy().into_owned(),
        ),
        (CLOUDLINK_ENABLED.to_owned(), "true".to_owned()),
        (
            CLOUDLINK_ORIGIN_MODEL.to_owned(),
            "gateway-signed".to_owned(),
        ),
        (
            CLOUDLINK_CLOUD_KEY_ID.to_owned(),
            "development-cloud-key-1".to_owned(),
        ),
        (
            CLOUDLINK_CLOUD_PUBLIC_KEY_REF.to_owned(),
            "env:AETHER_TEST_CLOUDLINK_CLOUD_PUBLIC_KEY".to_owned(),
        ),
        (
            CLOUDLINK_GATEWAY_KEY_ID.to_owned(),
            "development-gateway-key-17".to_owned(),
        ),
        (
            CLOUDLINK_GATEWAY_SIGNING_KEY_REF.to_owned(),
            "env:AETHER_TEST_CLOUDLINK_GATEWAY_SIGNING_KEY".to_owned(),
        ),
        (
            CLOUDLINK_CHALLENGE_LEDGER_PATH.to_owned(),
            root.join("challenge-ledger.json")
                .to_string_lossy()
                .into_owned(),
        ),
        (
            CLOUDLINK_RUNTIME_CONFIG_DIR.to_owned(),
            root.to_string_lossy().into_owned(),
        ),
        (
            CLOUDLINK_CLOUD_EXTENSION.to_owned(),
            "aether.cloudlink.integration.v1alpha1".to_owned(),
        ),
        (
            CLOUDLINK_TOPOLOGY_SPOOL_PATH.to_owned(),
            root.join("topology.spool").to_string_lossy().into_owned(),
        ),
        (
            CLOUDLINK_OBSERVATION_SPOOL_PATH.to_owned(),
            root.join("observations.spool")
                .to_string_lossy()
                .into_owned(),
        ),
        (
            CLOUDLINK_MQTT_BROKER_HOST.to_owned(),
            "localhost".to_owned(),
        ),
        (CLOUDLINK_MQTT_BROKER_PORT.to_owned(), "8883".to_owned()),
        (
            CLOUDLINK_MQTT_CLIENT_ID.to_owned(),
            "aether-edge-home-test".to_owned(),
        ),
        (
            CLOUDLINK_MQTT_TOPIC_PREFIX.to_owned(),
            "aether-test".to_owned(),
        ),
        (
            CLOUDLINK_MQTT_USERNAME.to_owned(),
            "edge-home-test".to_owned(),
        ),
        (
            CLOUDLINK_MQTT_PASSWORD_REF.to_owned(),
            "env:AETHER_TEST_CLOUDLINK_MQTT_PASSWORD".to_owned(),
        ),
        (
            CLOUDLINK_CREDENTIAL_ID.to_owned(),
            "home-edge-connector".to_owned(),
        ),
        (CLOUDLINK_CREDENTIAL_GENERATION.to_owned(), "1".to_owned()),
        (
            CLOUDLINK_SESSION_EPOCH_PATH.to_owned(),
            root.join("session-epoch").to_string_lossy().into_owned(),
        ),
    ])
}

#[cfg(feature = "home-assistant-cloudlink")]
fn cloudlink_config(root: &std::path::Path) -> HomeAssistantRuntimeConfig {
    let values = cloudlink_values(root);
    HomeAssistantRuntimeConfig::from_lookup(|name| values.get(name).cloned())
        .expect("CloudLink configuration")
}

#[cfg(feature = "home-assistant-integration-control")]
fn integration_control_values(root: &std::path::Path) -> BTreeMap<String, String> {
    BTreeMap::from([
        (ENABLED.to_owned(), "true".to_owned()),
        (
            ORIGIN.to_owned(),
            "http://homeassistant.local:8123".to_owned(),
        ),
        (SECRET_REF.to_owned(), "env:HOME_ASSISTANT_TOKEN".to_owned()),
        (
            GATEWAY_ID.to_owned(),
            "33333333-3333-4333-8333-333333333333".to_owned(),
        ),
        (INTEGRATION_ID.to_owned(), "home-assistant.home".to_owned()),
        (
            GENERATION_STORE_PATH.to_owned(),
            root.join("generations.json").to_string_lossy().into_owned(),
        ),
        (CLOUDLINK_ENABLED.to_owned(), "true".to_owned()),
        (
            CLOUDLINK_ORIGIN_MODEL.to_owned(),
            "gateway-signed".to_owned(),
        ),
        (
            CLOUDLINK_CLOUD_KEY_ID.to_owned(),
            "development-cloud-key-1".to_owned(),
        ),
        (
            CLOUDLINK_CLOUD_PUBLIC_KEY_REF.to_owned(),
            "env:AETHER_TEST_CONTROL_SESSION_CLOUD_KEY".to_owned(),
        ),
        (
            CLOUDLINK_GATEWAY_KEY_ID.to_owned(),
            "development-gateway-key-17".to_owned(),
        ),
        (
            CLOUDLINK_GATEWAY_SIGNING_KEY_REF.to_owned(),
            "env:AETHER_TEST_CONTROL_SESSION_GATEWAY_KEY".to_owned(),
        ),
        (
            CLOUDLINK_CHALLENGE_LEDGER_PATH.to_owned(),
            root.join("challenge-ledger.json")
                .to_string_lossy()
                .into_owned(),
        ),
        (
            CLOUDLINK_RUNTIME_CONFIG_DIR.to_owned(),
            root.to_string_lossy().into_owned(),
        ),
        (
            CLOUDLINK_CLOUD_EXTENSION.to_owned(),
            "aether.cloudlink.integration.v1alpha1".to_owned(),
        ),
        (
            CLOUDLINK_TOPOLOGY_SPOOL_PATH.to_owned(),
            root.join("topology.spool").to_string_lossy().into_owned(),
        ),
        (
            CLOUDLINK_OBSERVATION_SPOOL_PATH.to_owned(),
            root.join("observations.spool")
                .to_string_lossy()
                .into_owned(),
        ),
        (
            CLOUDLINK_MQTT_BROKER_HOST.to_owned(),
            "localhost".to_owned(),
        ),
        (CLOUDLINK_MQTT_BROKER_PORT.to_owned(), "8883".to_owned()),
        (
            CLOUDLINK_MQTT_CLIENT_ID.to_owned(),
            "aether-edge-control-test".to_owned(),
        ),
        (
            CLOUDLINK_MQTT_TOPIC_PREFIX.to_owned(),
            "aether-test".to_owned(),
        ),
        (
            CLOUDLINK_MQTT_USERNAME.to_owned(),
            "edge-control-test".to_owned(),
        ),
        (
            CLOUDLINK_MQTT_PASSWORD_REF.to_owned(),
            "env:AETHER_TEST_CONTROL_MQTT_PASSWORD".to_owned(),
        ),
        (
            CLOUDLINK_CREDENTIAL_ID.to_owned(),
            "edge-control-credential".to_owned(),
        ),
        (CLOUDLINK_CREDENTIAL_GENERATION.to_owned(), "3".to_owned()),
        (
            CLOUDLINK_SESSION_EPOCH_PATH.to_owned(),
            root.join("session-epoch").to_string_lossy().into_owned(),
        ),
        (CONTROL_ENABLED.to_owned(), "true".to_owned()),
        (
            CONTROL_CLOUD_EXTENSION.to_owned(),
            "aether.cloudlink.integration-control.v1alpha1".to_owned(),
        ),
        (
            CONTROL_LEDGER_PATH.to_owned(),
            root.join("control-ledger.json")
                .to_string_lossy()
                .into_owned(),
        ),
        (
            CONTROL_POLICY_PATH.to_owned(),
            root.join("control-policy.json")
                .to_string_lossy()
                .into_owned(),
        ),
        (
            CONTROL_AUDIT_PATH.to_owned(),
            root.join("control-audit.jsonl")
                .to_string_lossy()
                .into_owned(),
        ),
        (
            CONTROL_CLOUD_KEY_ID.to_owned(),
            "cloud-control-key-1".to_owned(),
        ),
        (
            CONTROL_CLOUD_PUBLIC_KEY_REF.to_owned(),
            "env:AETHER_TEST_CONTROL_CLOUD_KEY".to_owned(),
        ),
        (
            CONTROL_EDGE_KEY_ID.to_owned(),
            "development-gateway-key-17".to_owned(),
        ),
        (
            CONTROL_EDGE_SIGNING_KEY_REF.to_owned(),
            "env:AETHER_TEST_CONTROL_SESSION_GATEWAY_KEY".to_owned(),
        ),
    ])
}

#[test]
fn home_assistant_is_disabled_when_no_configuration_is_present() {
    assert_eq!(
        config(&[]).expect("default configuration"),
        HomeAssistantRuntimeConfig::Disabled
    );
}

#[test]
fn enabled_configuration_accepts_only_an_external_secret_reference() {
    let parsed = config(&[
        (ENABLED, "true"),
        (ORIGIN, "http://homeassistant.local:8123"),
        (SECRET_REF, "env:HOME_ASSISTANT_TOKEN"),
        (GATEWAY_ID, "home-edge"),
        (INTEGRATION_ID, "home-assistant-main"),
        (GENERATION_STORE_PATH, TEST_GENERATION_STORE_PATH),
    ])
    .expect("valid Home Assistant configuration");
    let enabled = parsed.enabled().expect("integration is enabled");

    assert_eq!(enabled.origin(), "http://homeassistant.local:8123");
    assert_eq!(
        enabled.access_token_ref().as_str(),
        "env:HOME_ASSISTANT_TOKEN"
    );
    assert_eq!(enabled.gateway_id().as_str(), "home-edge");
    assert_eq!(enabled.integration_id().as_str(), "home-assistant-main");
    assert_eq!(
        enabled.generation_store_path(),
        std::path::Path::new(TEST_GENERATION_STORE_PATH)
    );
    assert!(enabled.cloudlink().is_none());
    assert!(enabled.integration_control().is_none());
}

#[test]
fn integration_control_is_default_off_and_cannot_run_without_cloudlink() {
    let error = config(&[
        (ENABLED, "true"),
        (ORIGIN, "http://homeassistant.local:8123"),
        (SECRET_REF, "env:HOME_ASSISTANT_TOKEN"),
        (GATEWAY_ID, "33333333-3333-4333-8333-333333333333"),
        (GENERATION_STORE_PATH, TEST_GENERATION_STORE_PATH),
        (CONTROL_ENABLED, "true"),
    ])
    .expect_err("governed control requires the authenticated CloudLink composition");

    assert!(matches!(
        error,
        HomeAssistantStartupError::IntegrationControlRequiresCloudLink
    ));
}

#[test]
fn integration_control_cannot_implicitly_enable_home_assistant() {
    let error = config(&[(CONTROL_ENABLED, "true")])
        .expect_err("control cannot implicitly enable its read-only provider");

    assert!(matches!(
        error,
        HomeAssistantStartupError::IntegrationControlRequiresHomeAssistant
    ));
}

#[cfg(feature = "home-assistant-integration-control")]
#[test]
fn integration_control_requires_every_trust_policy_and_persistence_setting() {
    let root = tempfile::tempdir().expect("temporary runtime directory");
    let complete = integration_control_values(root.path());
    let parsed = HomeAssistantRuntimeConfig::from_lookup(|name| complete.get(name).cloned())
        .expect("complete control configuration");
    let control = parsed
        .enabled()
        .expect("Home Assistant")
        .integration_control()
        .expect("governed control");
    assert_eq!(
        control.ledger_path(),
        root.path().join("control-ledger.json")
    );
    assert_eq!(
        control.policy_path(),
        root.path().join("control-policy.json")
    );
    assert_eq!(
        control.audit_path(),
        root.path().join("control-audit.jsonl")
    );

    for required in [
        CONTROL_CLOUD_EXTENSION,
        CONTROL_LEDGER_PATH,
        CONTROL_POLICY_PATH,
        CONTROL_AUDIT_PATH,
        CONTROL_CLOUD_KEY_ID,
        CONTROL_CLOUD_PUBLIC_KEY_REF,
    ] {
        let mut missing = complete.clone();
        missing.remove(required);
        assert!(
            matches!(
                HomeAssistantRuntimeConfig::from_lookup(|name| missing.get(name).cloned()),
                Err(HomeAssistantStartupError::MissingSetting { name }) if name == required
            ),
            "missing {required} must fail startup configuration"
        );
    }

    let mut without_legacy_aliases = complete.clone();
    without_legacy_aliases.remove(CONTROL_EDGE_KEY_ID);
    without_legacy_aliases.remove(CONTROL_EDGE_SIGNING_KEY_REF);
    HomeAssistantRuntimeConfig::from_lookup(|name| without_legacy_aliases.get(name).cloned())
        .expect("deprecated receipt-key aliases are optional");

    let mut trusted_legacy_harness = complete.clone();
    trusted_legacy_harness.insert(
        CLOUDLINK_ORIGIN_MODEL.to_owned(),
        "trusted-connector-broker-attestation".to_owned(),
    );
    for gateway_signed_only in [
        CLOUDLINK_CLOUD_KEY_ID,
        CLOUDLINK_CLOUD_PUBLIC_KEY_REF,
        CLOUDLINK_GATEWAY_KEY_ID,
        CLOUDLINK_GATEWAY_SIGNING_KEY_REF,
        CLOUDLINK_CHALLENGE_LEDGER_PATH,
    ] {
        trusted_legacy_harness.remove(gateway_signed_only);
    }
    HomeAssistantRuntimeConfig::from_lookup(|name| trusted_legacy_harness.get(name).cloned())
        .expect("explicit legacy receipt signer remains available only to the test connector");

    let mut conflicting_alias = complete;
    conflicting_alias.insert(
        CONTROL_EDGE_KEY_ID.to_owned(),
        "different-receipt-key".to_owned(),
    );
    assert!(matches!(
        HomeAssistantRuntimeConfig::from_lookup(|name| conflicting_alias.get(name).cloned()),
        Err(HomeAssistantStartupError::IntegrationControlReceiptSigningKeyConflict)
    ));
}

#[cfg(feature = "home-assistant-integration-control")]
#[tokio::test]
async fn missing_control_manifest_token_disables_cloudlink_but_preserves_local_sync() {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use ed25519_dalek::SigningKey;

    let root = tempfile::tempdir().expect("temporary runtime directory");
    aether_runtime_catalog::KernelRuntimeManifest::from_io_features(
        env!("CARGO_PKG_VERSION"),
        "aarch64-unknown-linux-musl",
        ["home-assistant-cloudlink"],
    )
    .expect("read-only runtime manifest")
    .write_to_config_directory(root.path())
    .expect("write runtime manifest");
    let values = integration_control_values(root.path());
    let config = HomeAssistantRuntimeConfig::from_lookup(|name| values.get(name).cloned())
        .expect("control configuration");

    // SAFETY: the uniquely named session variables are owned by this test.
    unsafe {
        let cloud = SigningKey::from_bytes(&[7_u8; 32]);
        std::env::set_var(
            "AETHER_TEST_CONTROL_SESSION_CLOUD_KEY",
            URL_SAFE_NO_PAD.encode(cloud.verifying_key().to_bytes()),
        );
        std::env::set_var(
            "AETHER_TEST_CONTROL_SESSION_GATEWAY_KEY",
            URL_SAFE_NO_PAD.encode([9_u8; 32]),
        );
    }
    let shutdown = CancellationToken::new();
    let runtime = start_home_assistant_integration_with_config(config, shutdown.clone())
        .expect("missing control protocol rejects only the CloudLink extension")
        .expect("local Home Assistant remains active");
    shutdown.cancel();
    runtime.shutdown().await.expect("bounded local shutdown");
    // SAFETY: paired cleanup for variables owned by this test.
    unsafe {
        std::env::remove_var("AETHER_TEST_CONTROL_SESSION_CLOUD_KEY");
        std::env::remove_var("AETHER_TEST_CONTROL_SESSION_GATEWAY_KEY");
    }
}

#[test]
fn cloudlink_enablement_fails_closed_when_its_runtime_manifest_setting_is_missing() {
    let error = config(&[
        (ENABLED, "true"),
        (ORIGIN, "http://homeassistant.local:8123"),
        (SECRET_REF, "env:HOME_ASSISTANT_TOKEN"),
        (GATEWAY_ID, "33333333-3333-4333-8333-333333333333"),
        (GENERATION_STORE_PATH, TEST_GENERATION_STORE_PATH),
        (CLOUDLINK_ENABLED, "true"),
        (
            "AETHER_HOME_ASSISTANT_CLOUDLINK_ORIGIN_MODEL",
            "gateway-signed",
        ),
        (
            "AETHER_HOME_ASSISTANT_CLOUDLINK_CLOUD_KEY_ID",
            "cloud-key-1",
        ),
        (
            "AETHER_HOME_ASSISTANT_CLOUDLINK_CLOUD_PUBLIC_KEY_REF",
            "env:AETHER_TEST_CLOUDLINK_CLOUD_PUBLIC_KEY",
        ),
        (
            "AETHER_HOME_ASSISTANT_CLOUDLINK_GATEWAY_KEY_ID",
            "gateway-key-1",
        ),
        (
            "AETHER_HOME_ASSISTANT_CLOUDLINK_GATEWAY_SIGNING_KEY_REF",
            "env:AETHER_TEST_CLOUDLINK_GATEWAY_SIGNING_KEY",
        ),
        (
            "AETHER_HOME_ASSISTANT_CLOUDLINK_CHALLENGE_LEDGER_PATH",
            "/var/lib/aether/cloudlink-challenges.json",
        ),
    ])
    .expect_err("CloudLink cannot inherit implicit paths or rollout state");

    assert!(matches!(
        error,
        HomeAssistantStartupError::MissingSetting {
            name: "AETHER_HOME_ASSISTANT_CLOUDLINK_RUNTIME_CONFIG_DIR"
        }
    ));
}

#[cfg(feature = "home-assistant-cloudlink")]
#[test]
fn cloudlink_origin_is_explicit_and_gateway_signed_requires_every_authentication_setting() {
    let root = tempfile::tempdir().expect("temporary runtime directory");
    let complete = cloudlink_values(root.path());
    let parsed = HomeAssistantRuntimeConfig::from_lookup(|name| complete.get(name).cloned())
        .expect("complete Gateway-signed configuration");
    let cloudlink = parsed
        .enabled()
        .expect("Home Assistant")
        .cloudlink()
        .expect("CloudLink");
    assert_eq!(
        cloudlink.origin_model(),
        HomeAssistantCloudLinkOriginModel::GatewaySigned
    );
    assert_eq!(
        cloudlink.challenge_ledger_path(),
        Some(root.path().join("challenge-ledger.json").as_path())
    );

    for required in [
        CLOUDLINK_ORIGIN_MODEL,
        CLOUDLINK_CLOUD_KEY_ID,
        CLOUDLINK_CLOUD_PUBLIC_KEY_REF,
        CLOUDLINK_GATEWAY_KEY_ID,
        CLOUDLINK_GATEWAY_SIGNING_KEY_REF,
        CLOUDLINK_CHALLENGE_LEDGER_PATH,
    ] {
        let mut missing = complete.clone();
        missing.remove(required);
        assert!(
            matches!(
                HomeAssistantRuntimeConfig::from_lookup(|name| missing.get(name).cloned()),
                Err(HomeAssistantStartupError::MissingSetting { name }) if name == required
            ),
            "missing {required} must fail closed"
        );
    }
}

#[cfg(feature = "home-assistant-cloudlink")]
#[tokio::test]
async fn trusted_connector_is_test_only_and_never_the_implicit_or_production_origin() {
    let root = tempfile::tempdir().expect("temporary runtime directory");
    aether_runtime_catalog::KernelRuntimeManifest::from_io_features(
        env!("CARGO_PKG_VERSION"),
        "aarch64-unknown-linux-musl",
        ["home-assistant-cloudlink"],
    )
    .expect("runtime manifest")
    .write_to_config_directory(root.path())
    .expect("write runtime manifest");
    let mut values = cloudlink_values(root.path());
    values.insert(
        CLOUDLINK_ORIGIN_MODEL.to_owned(),
        "trusted-connector-broker-attestation".to_owned(),
    );
    for gateway_signed_only in [
        CLOUDLINK_CLOUD_KEY_ID,
        CLOUDLINK_CLOUD_PUBLIC_KEY_REF,
        CLOUDLINK_GATEWAY_KEY_ID,
        CLOUDLINK_GATEWAY_SIGNING_KEY_REF,
        CLOUDLINK_CHALLENGE_LEDGER_PATH,
    ] {
        values.remove(gateway_signed_only);
    }
    let config = HomeAssistantRuntimeConfig::from_lookup(|name| values.get(name).cloned())
        .expect("explicit test-only trusted connector configuration");
    assert_eq!(
        config
            .enabled()
            .expect("Home Assistant")
            .cloudlink()
            .expect("CloudLink")
            .origin_model(),
        HomeAssistantCloudLinkOriginModel::TrustedConnectorBrokerAttestation
    );

    let shutdown = CancellationToken::new();
    let runtime = start_home_assistant_integration_with_config(config, shutdown.clone())
        .expect("production rejects only the CloudLink extension")
        .expect("local Home Assistant runtime remains enabled");
    shutdown.cancel();
    runtime.shutdown().await.expect("bounded local shutdown");
}

#[cfg(feature = "home-assistant-cloudlink")]
#[tokio::test]
async fn malformed_cloudlink_key_material_does_not_disable_local_home_assistant() {
    let root = tempfile::tempdir().expect("temporary runtime directory");
    aether_runtime_catalog::KernelRuntimeManifest::from_io_features(
        env!("CARGO_PKG_VERSION"),
        "aarch64-unknown-linux-musl",
        ["home-assistant-cloudlink"],
    )
    .expect("runtime manifest")
    .write_to_config_directory(root.path())
    .expect("write runtime manifest");
    let mut values = cloudlink_values(root.path());
    values.insert(
        CLOUDLINK_CLOUD_PUBLIC_KEY_REF.to_owned(),
        "env:AETHER_TEST_MALFORMED_SESSION_CLOUD_KEY".to_owned(),
    );
    values.insert(
        CLOUDLINK_GATEWAY_SIGNING_KEY_REF.to_owned(),
        "env:AETHER_TEST_MALFORMED_SESSION_GATEWAY_KEY".to_owned(),
    );
    // SAFETY: these uniquely named variables are owned and removed by this test.
    unsafe {
        std::env::set_var("AETHER_TEST_MALFORMED_SESSION_CLOUD_KEY", "invalid");
        std::env::set_var("AETHER_TEST_MALFORMED_SESSION_GATEWAY_KEY", "invalid");
    }
    let config = HomeAssistantRuntimeConfig::from_lookup(|name| values.get(name).cloned())
        .expect("structurally complete configuration");

    let shutdown = CancellationToken::new();
    let runtime = start_home_assistant_integration_with_config(config, shutdown.clone())
        .expect("malformed CloudLink authentication rejects only the Cloud extension")
        .expect("local Home Assistant runtime remains enabled");
    shutdown.cancel();
    runtime.shutdown().await.expect("bounded local shutdown");
    // SAFETY: paired cleanup for the uniquely named variables above.
    unsafe {
        std::env::remove_var("AETHER_TEST_MALFORMED_SESSION_CLOUD_KEY");
        std::env::remove_var("AETHER_TEST_MALFORMED_SESSION_GATEWAY_KEY");
    }
}

#[cfg(feature = "home-assistant-cloudlink")]
#[tokio::test]
async fn explicit_cloud_first_configuration_reaches_the_real_mqtt_composition() {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use ed25519_dalek::SigningKey;

    let root = tempfile::tempdir().expect("temporary runtime directory");
    aether_runtime_catalog::KernelRuntimeManifest::from_io_features(
        env!("CARGO_PKG_VERSION"),
        "aarch64-unknown-linux-musl",
        ["home-assistant-cloudlink"],
    )
    .expect("runtime manifest")
    .write_to_config_directory(root.path())
    .expect("write runtime manifest");
    let shutdown = CancellationToken::new();
    // SAFETY: this test owns a uniquely named process variable and removes it
    // before returning; production code still receives only a secret reference.
    unsafe {
        std::env::set_var(
            "AETHER_TEST_CLOUDLINK_MQTT_PASSWORD",
            "test-broker-password",
        );
        let cloud = SigningKey::from_bytes(&[7_u8; 32]);
        std::env::set_var(
            "AETHER_TEST_CLOUDLINK_CLOUD_PUBLIC_KEY",
            URL_SAFE_NO_PAD.encode(cloud.verifying_key().to_bytes()),
        );
        std::env::set_var(
            "AETHER_TEST_CLOUDLINK_GATEWAY_SIGNING_KEY",
            URL_SAFE_NO_PAD.encode([9_u8; 32]),
        );
    }

    let runtime = start_home_assistant_integration_with_config(
        cloudlink_config(root.path()),
        shutdown.clone(),
    )
    .expect("Cloud-first composition")
    .expect("enabled runtime");

    shutdown.cancel();
    runtime.shutdown().await.expect("bounded shutdown");
    // SAFETY: paired cleanup for the uniquely named variable set above.
    unsafe {
        std::env::remove_var("AETHER_TEST_CLOUDLINK_MQTT_PASSWORD");
        std::env::remove_var("AETHER_TEST_CLOUDLINK_CLOUD_PUBLIC_KEY");
        std::env::remove_var("AETHER_TEST_CLOUDLINK_GATEWAY_SIGNING_KEY");
    }
}

#[cfg(feature = "home-assistant-cloudlink")]
#[tokio::test]
async fn local_home_assistant_snapshot_continues_while_cloudlink_is_offline() {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use ed25519_dalek::SigningKey;
    use futures::{SinkExt as _, StreamExt as _};
    use serde_json::{Value, json};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio_tungstenite::{accept_async, tungstenite::Message};

    const TOKEN_VARIABLE: &str = "AETHER_TEST_LOCAL_FIRST_HOME_ASSISTANT_TOKEN";
    const TOKEN: &str = "local-first-test-token";

    let root = tempfile::tempdir().expect("temporary runtime directory");
    aether_runtime_catalog::KernelRuntimeManifest::from_io_features(
        env!("CARGO_PKG_VERSION"),
        "aarch64-unknown-linux-musl",
        ["home-assistant-cloudlink"],
    )
    .expect("runtime manifest")
    .write_to_config_directory(root.path())
    .expect("write runtime manifest");

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback Home Assistant");
    let home_assistant_origin = format!(
        "http://{}",
        listener.local_addr().expect("Home Assistant address")
    );
    let unavailable_cloud = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("reserve an unavailable CloudLink port");
    let unavailable_cloud_port = unavailable_cloud
        .local_addr()
        .expect("unavailable CloudLink address")
        .port();
    drop(unavailable_cloud);

    let (snapshot_served_tx, snapshot_served_rx) = oneshot::channel();
    let home_assistant = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("Home Assistant client");
        let mut socket = accept_async(stream)
            .await
            .expect("Home Assistant WebSocket");
        socket
            .send(Message::Text(
                json!({"type": "auth_required"}).to_string().into(),
            ))
            .await
            .expect("auth challenge");
        let authentication = socket
            .next()
            .await
            .expect("authentication frame")
            .expect("valid authentication frame");
        assert_eq!(
            serde_json::from_str::<Value>(authentication.to_text().expect("authentication text"))
                .expect("authentication JSON"),
            json!({"type": "auth", "access_token": TOKEN})
        );
        socket
            .send(Message::Text(json!({"type": "auth_ok"}).to_string().into()))
            .await
            .expect("auth accepted");

        for _ in 0..2 {
            let command = socket
                .next()
                .await
                .expect("subscription frame")
                .expect("valid subscription frame");
            let command: Value =
                serde_json::from_str(command.to_text().expect("subscription text"))
                    .expect("subscription JSON");
            socket
                .send(Message::Text(
                    json!({
                        "id": command["id"],
                        "type": "result",
                        "success": true,
                        "result": null
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .expect("subscription accepted");
        }
        for expected_type in [
            "config/area_registry/list",
            "config/device_registry/list",
            "config/entity_registry/list",
            "get_states",
        ] {
            let command = socket
                .next()
                .await
                .expect("snapshot frame")
                .expect("valid snapshot frame");
            let command: Value = serde_json::from_str(command.to_text().expect("snapshot text"))
                .expect("snapshot JSON");
            assert_eq!(command["type"], expected_type);
            socket
                .send(Message::Text(
                    json!({
                        "id": command["id"],
                        "type": "result",
                        "success": true,
                        "result": []
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .expect("snapshot response");
        }
        let _ = snapshot_served_tx.send(());
    });

    let mut values = cloudlink_values(root.path());
    values.insert(ORIGIN.to_owned(), home_assistant_origin);
    values.insert(SECRET_REF.to_owned(), format!("env:{TOKEN_VARIABLE}"));
    values.insert(
        CLOUDLINK_MQTT_BROKER_HOST.to_owned(),
        "127.0.0.1".to_owned(),
    );
    values.insert(
        CLOUDLINK_MQTT_BROKER_PORT.to_owned(),
        unavailable_cloud_port.to_string(),
    );
    // SAFETY: this test owns uniquely named variables and removes them before returning.
    unsafe {
        std::env::set_var(TOKEN_VARIABLE, TOKEN);
        std::env::set_var(
            "AETHER_TEST_CLOUDLINK_MQTT_PASSWORD",
            "offline-cloudlink-test-password",
        );
        let cloud = SigningKey::from_bytes(&[8_u8; 32]);
        std::env::set_var(
            "AETHER_TEST_CLOUDLINK_CLOUD_PUBLIC_KEY",
            URL_SAFE_NO_PAD.encode(cloud.verifying_key().to_bytes()),
        );
        std::env::set_var(
            "AETHER_TEST_CLOUDLINK_GATEWAY_SIGNING_KEY",
            URL_SAFE_NO_PAD.encode([9_u8; 32]),
        );
    }
    let config = HomeAssistantRuntimeConfig::from_lookup(|name| values.get(name).cloned())
        .expect("local-first configuration");
    let shutdown = CancellationToken::new();
    let runtime = start_home_assistant_integration_with_config(config, shutdown.clone())
        .expect("Cloud failure cannot reject local composition")
        .expect("local Home Assistant runtime");

    tokio::time::timeout(std::time::Duration::from_secs(3), snapshot_served_rx)
        .await
        .expect("local snapshot request deadline")
        .expect("local snapshot served");
    let generation_path = root.path().join("generations.json");
    let generation_persisted = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if std::fs::read(&generation_path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .and_then(|document| document["entries"].as_array().cloned())
                .is_some_and(|entries| !entries.is_empty())
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(
        generation_persisted.is_ok(),
        "the local snapshot must be committed without a CloudLink session"
    );

    shutdown.cancel();
    runtime.shutdown().await.expect("bounded local shutdown");
    home_assistant.await.expect("Home Assistant test server");
    // SAFETY: paired cleanup for variables owned by this test.
    unsafe {
        std::env::remove_var(TOKEN_VARIABLE);
        std::env::remove_var("AETHER_TEST_CLOUDLINK_MQTT_PASSWORD");
        std::env::remove_var("AETHER_TEST_CLOUDLINK_CLOUD_PUBLIC_KEY");
        std::env::remove_var("AETHER_TEST_CLOUDLINK_GATEWAY_SIGNING_KEY");
    }
}

#[cfg(feature = "home-assistant-cloudlink")]
#[test]
fn cloudlink_configuration_diagnostics_hide_authentication_identifiers_and_references() {
    let root = tempfile::tempdir().expect("temporary runtime directory");
    let parsed = cloudlink_config(root.path());
    let diagnostics = format!("{parsed:?}");
    for sensitive in [
        "home-edge-connector",
        "development-cloud-key-1",
        "development-gateway-key-17",
        "AETHER_TEST_CLOUDLINK_CLOUD_PUBLIC_KEY",
        "AETHER_TEST_CLOUDLINK_GATEWAY_SIGNING_KEY",
    ] {
        assert!(!diagnostics.contains(sensitive));
    }
}

#[test]
fn enabled_configuration_rejects_plaintext_credentials_without_rendering_them() {
    const SECRET: &str = "must-never-appear-in-errors";
    let error = config(&[
        (ENABLED, "true"),
        (ORIGIN, "http://homeassistant.local:8123"),
        (SECRET_REF, "env:HOME_ASSISTANT_TOKEN"),
        (PLAINTEXT_TOKEN, SECRET),
        (GATEWAY_ID, "home-edge"),
        (GENERATION_STORE_PATH, TEST_GENERATION_STORE_PATH),
    ])
    .expect_err("plaintext credentials must be rejected");

    assert!(matches!(
        error,
        HomeAssistantStartupError::PlaintextCredentialForbidden
    ));
    assert!(!error.to_string().contains(SECRET));
}

#[test]
fn enabled_configuration_fails_closed_when_required_values_are_missing() {
    let error = config(&[(ENABLED, "true")]).expect_err("origin must be required");

    assert!(matches!(
        error,
        HomeAssistantStartupError::MissingSetting {
            name: "AETHER_HOME_ASSISTANT_ORIGIN"
        }
    ));
}

#[test]
fn enabled_configuration_rejects_a_non_environment_secret_reference() {
    let error = config(&[
        (ENABLED, "true"),
        (ORIGIN, "http://homeassistant.local:8123"),
        (SECRET_REF, "literal-token"),
        (GATEWAY_ID, "home-edge"),
        (GENERATION_STORE_PATH, TEST_GENERATION_STORE_PATH),
    ])
    .expect_err("only environment-backed secret references are composed");

    assert!(matches!(
        error,
        HomeAssistantStartupError::UnsupportedSecretReference
    ));
}

#[test]
fn enabled_configuration_requires_an_absolute_generation_store_path() {
    let missing = config(&[
        (ENABLED, "true"),
        (ORIGIN, "http://homeassistant.local:8123"),
        (SECRET_REF, "env:HOME_ASSISTANT_TOKEN"),
        (GATEWAY_ID, "home-edge"),
    ])
    .expect_err("persistent generation state must be required");
    assert!(matches!(
        missing,
        HomeAssistantStartupError::MissingSetting {
            name: "AETHER_HOME_ASSISTANT_GENERATION_STORE_PATH"
        }
    ));

    let relative = config(&[
        (ENABLED, "true"),
        (ORIGIN, "http://homeassistant.local:8123"),
        (SECRET_REF, "env:HOME_ASSISTANT_TOKEN"),
        (GATEWAY_ID, "home-edge"),
        (
            GENERATION_STORE_PATH,
            "data/home-assistant-generations.json",
        ),
    ])
    .expect_err("relative generation state must be rejected");
    assert!(matches!(
        relative,
        HomeAssistantStartupError::InvalidGenerationStorePath
    ));
}

#[test]
fn disabled_configuration_preserves_the_existing_startup_path() {
    let runtime = start_home_assistant_integration_with_config(
        HomeAssistantRuntimeConfig::Disabled,
        CancellationToken::new(),
    )
    .expect("disabled integration cannot fail startup");

    assert!(runtime.is_none());
}

#[cfg(not(feature = "home-assistant"))]
#[test]
fn enabled_configuration_fails_when_the_optional_adapter_is_not_compiled() {
    let enabled = config(&[
        (ENABLED, "true"),
        (ORIGIN, "http://homeassistant.local:8123"),
        (SECRET_REF, "env:HOME_ASSISTANT_TOKEN"),
        (GATEWAY_ID, "home-edge"),
        (GENERATION_STORE_PATH, TEST_GENERATION_STORE_PATH),
    ])
    .expect("valid enabled configuration");

    let error = start_home_assistant_integration_with_config(enabled, CancellationToken::new())
        .expect_err("missing adapter feature must fail startup");
    assert!(matches!(
        error,
        HomeAssistantStartupError::FeatureNotCompiled
    ));
}

#[cfg(feature = "home-assistant")]
#[test]
fn enabled_configuration_validates_the_origin_before_spawning() {
    let enabled = config(&[
        (ENABLED, "true"),
        (ORIGIN, "http://user:password@homeassistant.local:8123"),
        (SECRET_REF, "env:HOME_ASSISTANT_TOKEN"),
        (GATEWAY_ID, "home-edge"),
        (GENERATION_STORE_PATH, TEST_GENERATION_STORE_PATH),
    ])
    .expect("composition configuration");

    let error = start_home_assistant_integration_with_config(enabled, CancellationToken::new())
        .expect_err("credential-bearing origin must fail before spawning");
    assert!(matches!(
        error,
        HomeAssistantStartupError::InvalidConnectionConfiguration(_)
    ));
}

#[cfg(feature = "home-assistant")]
#[test]
fn enabled_configuration_fails_before_spawning_when_generation_state_is_unavailable() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let blocked_parent = directory.path().join("not-a-directory");
    std::fs::write(&blocked_parent, b"occupied").expect("create blocking file");
    let generation_path = blocked_parent.join("generations.json");
    let generation_path = generation_path.to_str().expect("UTF-8 test path");
    let enabled = config(&[
        (ENABLED, "true"),
        (ORIGIN, "http://homeassistant.local:8123"),
        (SECRET_REF, "env:HOME_ASSISTANT_TOKEN"),
        (GATEWAY_ID, "home-edge"),
        (GENERATION_STORE_PATH, generation_path),
    ])
    .expect("composition configuration");

    let error = start_home_assistant_integration_with_config(enabled, CancellationToken::new())
        .expect_err("unavailable generation state must fail before spawning");
    assert!(matches!(
        error,
        HomeAssistantStartupError::GenerationStoreUnavailable(_)
    ));
}

fn snapshot(sequence: u64, value: bool) -> IntegrationSnapshot {
    snapshot_with_digest(sequence, value, 7)
}

fn snapshot_with_digest(sequence: u64, value: bool, digest_seed: u64) -> IntegrationSnapshot {
    let gateway_id = GatewayIdentity::new("home-edge").expect("gateway identity");
    let integration_id = IntegrationId::new("home-assistant-main").expect("integration identity");
    let entity_id = EntityId::new("switch-kitchen").expect("entity identity");
    let point_key = IntegrationPointKey::new("is_on").expect("point key");
    let entity = EntityRecord::new(
        entity_id.clone(),
        "Kitchen switch",
        "switch",
        vec![
            EntityPointDescriptor::new(
                point_key.clone(),
                "Is on",
                IntegrationPointKind::State,
                ObservedValueType::Boolean,
                None,
            )
            .expect("point descriptor"),
        ],
        None,
        None,
        vec![],
    )
    .expect("entity");
    let topology = IntegrationTopologySnapshot::new(
        gateway_id.clone(),
        integration_id.clone(),
        TopologyGeneration::new(7).expect("generation"),
        TimestampMs::new(10),
        SnapshotDigest::new(format!("sha256:{digest_seed:064x}")).expect("digest"),
        vec![],
        vec![],
        vec![entity],
    )
    .expect("topology");
    let observation = IntegrationObservation::available(
        gateway_id,
        integration_id,
        entity_id,
        point_key,
        ObservedValue::boolean(value),
        TimestampMs::new(10 + sequence),
        sequence,
        None,
    )
    .expect("observation");
    IntegrationSnapshot::new(topology, vec![observation]).expect("snapshot")
}

#[tokio::test]
async fn composition_projection_accepts_an_idempotent_topology_generation_refresh() {
    let projection = InMemoryIntegrationProjection::default();
    let initial = snapshot(1, false);
    let gateway_id = initial.topology().gateway_id().clone();
    let integration_id = initial.topology().integration_id().clone();
    projection
        .replace_snapshot(initial)
        .await
        .expect("initialize projection");

    projection
        .replace_snapshot(snapshot(2, true))
        .await
        .expect("same topology generation and digest must refresh current state");

    let projected = projection
        .snapshot(&gateway_id, &integration_id)
        .await
        .expect("query projection")
        .expect("projection exists");
    assert_eq!(
        projected.observations()[0].value(),
        Some(&ObservedValue::boolean(true))
    );
    assert_eq!(projected.observations()[0].point_key().as_str(), "is_on");
}

#[tokio::test]
async fn composition_projection_rejects_a_digest_change_without_a_new_generation() {
    let projection = InMemoryIntegrationProjection::default();
    let initial = snapshot(1, false);
    let gateway_id = initial.topology().gateway_id().clone();
    let integration_id = initial.topology().integration_id().clone();
    projection
        .replace_snapshot(initial.clone())
        .await
        .expect("initialize projection");

    let error = projection
        .replace_snapshot(snapshot_with_digest(2, true, 8))
        .await
        .expect_err("a changed topology digest must advance generation");
    assert_eq!(error.kind(), PortErrorKind::Conflict);
    assert_eq!(
        projection
            .snapshot(&gateway_id, &integration_id)
            .await
            .expect("query projection"),
        Some(initial)
    );
}

#[tokio::test]
async fn composition_projection_atomically_replaces_and_advances_a_snapshot() {
    let projection = InMemoryIntegrationProjection::default();
    let initial = snapshot(1, false);
    let gateway_id = initial.topology().gateway_id().clone();
    let integration_id = initial.topology().integration_id().clone();

    let receipt = projection
        .replace_snapshot(initial)
        .await
        .expect("replace projection");
    assert_eq!(receipt.sequence(), None);

    let (_, observations) = snapshot(2, true).into_parts();
    let receipt = projection
        .apply_observation(
            TopologyGeneration::new(7).expect("generation"),
            observations.into_iter().next().expect("observation"),
        )
        .await
        .expect("advance projection");
    assert_eq!(receipt.sequence(), Some(2));

    let projected = projection
        .snapshot(&gateway_id, &integration_id)
        .await
        .expect("query projection")
        .expect("projection exists");
    assert_eq!(
        projected.observations()[0].value(),
        Some(&ObservedValue::boolean(true))
    );
    assert_eq!(projected.observations()[0].point_key().as_str(), "is_on");
}

#[tokio::test]
async fn composition_projection_rejects_sequence_gaps_without_mutating_state() {
    let projection = InMemoryIntegrationProjection::default();
    let initial = snapshot(1, false);
    let gateway_id = initial.topology().gateway_id().clone();
    let integration_id = initial.topology().integration_id().clone();
    projection
        .replace_snapshot(initial.clone())
        .await
        .expect("replace projection");

    let (_, observations) = snapshot(3, true).into_parts();
    let error = projection
        .apply_observation(
            TopologyGeneration::new(7).expect("generation"),
            observations.into_iter().next().expect("observation"),
        )
        .await
        .expect_err("sequence gap must fail closed");

    assert_eq!(error.kind(), PortErrorKind::Conflict);
    assert_eq!(
        projection
            .snapshot(&gateway_id, &integration_id)
            .await
            .expect("query projection"),
        Some(initial)
    );
}
