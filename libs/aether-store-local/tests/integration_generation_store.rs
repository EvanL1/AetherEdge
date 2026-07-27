use std::sync::Arc;

use aether_domain::{GatewayIdentity, IntegrationId, SnapshotDigest};
use aether_ports::{IntegrationTopologyGenerationStore, PortErrorKind};
use aether_store_local::FileIntegrationTopologyGenerationStore;
use aether_testkit::assert_integration_topology_generation_store;
use tempfile::tempdir;

fn gateway(value: &str) -> GatewayIdentity {
    GatewayIdentity::new(value).expect("valid gateway")
}

fn integration(value: &str) -> IntegrationId {
    IntegrationId::new(value).expect("valid integration")
}

fn digest(first: char) -> SnapshotDigest {
    SnapshotDigest::new(format!("sha256:{first}{}", "1".repeat(63))).expect("valid digest")
}

#[tokio::test]
async fn file_store_passes_the_shared_generation_conformance_suite() {
    let root = tempdir().expect("temp directory");
    let store = FileIntegrationTopologyGenerationStore::open(
        root.path().join("integration-generations.json"),
    )
    .expect("open generation store");
    assert_integration_topology_generation_store(&store)
        .await
        .expect("generation-store conformance");
}

#[tokio::test]
async fn generation_is_stable_for_one_digest_and_increments_after_restart() {
    let root = tempdir().expect("temp directory");
    let path = root.path().join("integration-generations.json");
    {
        let store =
            FileIntegrationTopologyGenerationStore::open(&path).expect("open generation store");
        let first = store
            .reserve_generation(
                &gateway("gateway-home"),
                &integration("home-assistant.home"),
                &digest('a'),
            )
            .await
            .expect("first generation");
        let replay = store
            .reserve_generation(
                &gateway("gateway-home"),
                &integration("home-assistant.home"),
                &digest('a'),
            )
            .await
            .expect("same digest");
        assert_eq!(first.get(), 1);
        assert_eq!(replay, first);
    }

    let reopened =
        FileIntegrationTopologyGenerationStore::open(&path).expect("reopen generation store");
    let next = reopened
        .reserve_generation(
            &gateway("gateway-home"),
            &integration("home-assistant.home"),
            &digest('b'),
        )
        .await
        .expect("changed topology");
    assert_eq!(next.get(), 2);
}

#[tokio::test]
async fn scopes_are_independent_and_concurrent_same_digest_reservations_converge() {
    let root = tempdir().expect("temp directory");
    let store = Arc::new(
        FileIntegrationTopologyGenerationStore::open(
            root.path().join("integration-generations.json"),
        )
        .expect("open generation store"),
    );

    let mut tasks = Vec::new();
    for _ in 0..32 {
        let store = Arc::clone(&store);
        tasks.push(tokio::spawn(async move {
            store
                .reserve_generation(
                    &gateway("gateway-home"),
                    &integration("home-assistant.home"),
                    &digest('a'),
                )
                .await
                .expect("concurrent reservation")
        }));
    }
    for task in tasks {
        assert_eq!(task.await.expect("task").get(), 1);
    }

    let other_integration = store
        .reserve_generation(
            &gateway("gateway-home"),
            &integration("home-assistant.office"),
            &digest('b'),
        )
        .await
        .expect("other integration");
    let other_gateway = store
        .reserve_generation(
            &gateway("gateway-office"),
            &integration("home-assistant.home"),
            &digest('b'),
        )
        .await
        .expect("other gateway");
    assert_eq!(other_integration.get(), 1);
    assert_eq!(other_gateway.get(), 1);
}

#[test]
fn store_refuses_two_process_owners_and_releases_the_lock_on_drop() {
    let root = tempdir().expect("temp directory");
    let path = root.path().join("integration-generations.json");
    let first =
        FileIntegrationTopologyGenerationStore::open(&path).expect("first generation store");
    let error = FileIntegrationTopologyGenerationStore::open(&path)
        .expect_err("second writer must be rejected");
    assert_eq!(error.kind(), PortErrorKind::Conflict);
    drop(first);
    FileIntegrationTopologyGenerationStore::open(&path).expect("lock is released");
}

#[test]
fn store_fails_closed_on_corrupt_or_exhausted_state() {
    let root = tempdir().expect("temp directory");
    let corrupt_path = root.path().join("corrupt.json");
    std::fs::write(&corrupt_path, b"{not-json").expect("write corrupt fixture");
    let error = FileIntegrationTopologyGenerationStore::open(&corrupt_path)
        .expect_err("corrupt state must fail");
    assert_eq!(error.kind(), PortErrorKind::Permanent);

    let exhausted_path = root.path().join("exhausted.json");
    std::fs::write(
        &exhausted_path,
        format!(
            r#"{{
              "schema":"aether.edge.integration-generations.v1",
              "entries":[{{
                "gateway_id":"gateway-home",
                "integration_id":"home-assistant.home",
                "generation":{},
                "snapshot_digest":"{}"
              }}]
            }}"#,
            u64::MAX,
            digest('a').as_str()
        ),
    )
    .expect("write exhausted fixture");
    let store =
        FileIntegrationTopologyGenerationStore::open(&exhausted_path).expect("valid state opens");
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let error = runtime
        .block_on(store.reserve_generation(
            &gateway("gateway-home"),
            &integration("home-assistant.home"),
            &digest('b'),
        ))
        .expect_err("generation must not wrap");
    assert_eq!(error.kind(), PortErrorKind::Permanent);
}
