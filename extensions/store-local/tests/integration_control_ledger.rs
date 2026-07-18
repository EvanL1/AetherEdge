#![cfg(feature = "integration-control")]

use aether_integration_control::{
    ActionReceiptPayload, IntegrationControlCodec, IntegrationControlLedger, LedgerClaim,
    LedgerClaimOutcome, LedgerEntryState, LedgerJobKey, SpooledActionReceipt,
};
use aether_ports::{CloudLinkDurableAck, CloudLinkSessionBinding, DurableAckOutcome};
use aether_store_local::{FileIntegrationControlAudit, FileIntegrationControlLedger};
use tempfile::tempdir;

const OFFER: &[u8] = include_bytes!(
    "../../../crates/aether-integration-control/tests/fixtures/integration-control/v1alpha1/action-offer.valid.json"
);
const RECEIPT: &[u8] = include_bytes!(
    "../../../crates/aether-integration-control/tests/fixtures/integration-control/v1alpha1/action-receipt-provider-accepted.valid.json"
);

fn claim() -> LedgerClaim {
    let offer = IntegrationControlCodec::decode_offer(OFFER).expect("offer");
    LedgerClaim::new(
        LedgerJobKey::new(offer.gateway_id(), offer.job_id()).expect("key"),
        offer.intent_digest(),
        offer.intent().target().clone(),
        offer.intent().value(),
        "audit-control-1",
    )
    .expect("claim")
}

fn receipt() -> ActionReceiptPayload {
    IntegrationControlCodec::decode_receipt_envelope(RECEIPT)
        .expect("receipt envelope")
        .payload()
        .clone()
}

fn durable_ack(record: &SpooledActionReceipt, receipt_id: &str) -> CloudLinkDurableAck {
    CloudLinkDurableAck::new(
        CloudLinkSessionBinding::new("44444444-4444-4444-8444-444444444444", 7),
        "integration-control-receipts",
        record.stream_epoch(),
        record.position(),
        record.batch_id(),
        record.digest(),
        receipt_id,
    )
}

#[tokio::test]
async fn completed_receipt_and_ack_state_survive_restart_without_losing_job_deduplication() {
    let root = tempdir().expect("temp directory");
    let path = root.path().join("integration-control-ledger.json");
    let key = claim().key().clone();
    {
        let ledger = FileIntegrationControlLedger::open(&path).expect("ledger");
        assert_eq!(
            ledger.claim(claim()).await.expect("claim"),
            LedgerClaimOutcome::Claimed
        );
        ledger
            .complete(&key, claim().intent_digest(), receipt())
            .await
            .expect("complete");
        assert_eq!(ledger.pending_receipts(16).await.expect("pending").len(), 1);
    }

    {
        let ledger = FileIntegrationControlLedger::open(&path).expect("reopen");
        let entry = ledger.find(&key).await.expect("find").expect("stored job");
        assert!(matches!(entry.state(), LedgerEntryState::Complete(_)));
        let pending = ledger.pending_receipts(16).await.expect("pending");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].payload(), &receipt());
        let ack = durable_ack(&pending[0], "cloud-receipt-1");
        assert_eq!(
            ledger.acknowledge(&ack).await.expect("ack"),
            DurableAckOutcome::Applied { removed: 1 }
        );
        assert_eq!(
            ledger.acknowledge(&ack).await.expect("duplicate ack"),
            DurableAckOutcome::Duplicate
        );
        ledger
            .complete(&key, claim().intent_digest(), receipt())
            .await
            .expect("idempotent completion");
        assert!(
            ledger
                .pending_receipts(16)
                .await
                .expect("pending after idempotent completion")
                .is_empty()
        );
    }

    {
        let ledger = FileIntegrationControlLedger::open(&path).expect("reopen after ack");
        assert!(
            ledger
                .pending_receipts(16)
                .await
                .expect("pending")
                .is_empty()
        );
        assert!(matches!(
            ledger.find(&key).await.expect("find").expect("job").state(),
            LedgerEntryState::Complete(_)
        ));
        ledger.requeue(&key).await.expect("explicit replay");
        let replayed = ledger.pending_receipts(16).await.expect("pending");
        assert_eq!(replayed.len(), 1);
        assert_eq!(replayed[0].payload(), &receipt());
        assert_eq!(replayed[0].position(), 2);
    }

    {
        let ledger = FileIntegrationControlLedger::open(&path).expect("reopen after replay");
        let replayed = ledger.pending_receipts(16).await.expect("pending");
        assert_eq!(replayed.len(), 1);
        assert_eq!(replayed[0].position(), 2);
    }
}

#[tokio::test]
async fn mismatched_durable_ack_is_rejected_without_removing_the_receipt() {
    let root = tempdir().expect("temp directory");
    let path = root.path().join("integration-control-ledger.json");
    let ledger = FileIntegrationControlLedger::open(&path).expect("ledger");
    let key = claim().key().clone();
    assert_eq!(
        ledger.claim(claim()).await.expect("claim"),
        LedgerClaimOutcome::Claimed
    );
    ledger
        .complete(&key, claim().intent_digest(), receipt())
        .await
        .expect("complete");
    let pending = ledger.pending_receipts(16).await.expect("pending");
    let invalid = CloudLinkDurableAck::new(
        CloudLinkSessionBinding::new("44444444-4444-4444-8444-444444444444", 7),
        "integration-control-receipts",
        pending[0].stream_epoch(),
        pending[0].position(),
        pending[0].batch_id(),
        format!("sha256:{}", "f".repeat(64)),
        "cloud-receipt-invalid",
    );

    assert!(ledger.acknowledge(&invalid).await.is_err());
    assert_eq!(ledger.pending_receipts(16).await.expect("pending"), pending);
}

#[tokio::test]
async fn interrupted_claim_survives_restart_and_different_digest_is_rejected() {
    let root = tempdir().expect("temp directory");
    let path = root.path().join("integration-control-ledger.json");
    {
        let ledger = FileIntegrationControlLedger::open(&path).expect("ledger");
        assert_eq!(
            ledger.claim(claim()).await.expect("claim"),
            LedgerClaimOutcome::Claimed
        );
    }

    let ledger = FileIntegrationControlLedger::open(&path).expect("reopen");
    assert_eq!(
        ledger.interrupted().await.expect("interrupted"),
        vec![claim()]
    );
    let changed = LedgerClaim::new(
        claim().key().clone(),
        format!("sha256:{}", "f".repeat(64)),
        claim().target().clone(),
        false,
        "audit-control-2",
    )
    .expect("changed claim");
    assert_eq!(
        ledger.claim(changed).await.expect("conflict"),
        LedgerClaimOutcome::DigestConflict
    );
}

#[cfg(unix)]
#[test]
fn sensitive_control_files_reject_permissive_modes_and_symbolic_link_parents() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let root = tempdir().expect("temp directory");
    let ledger_path = root.path().join("ledger.json");
    drop(FileIntegrationControlLedger::open(&ledger_path).expect("initial ledger"));
    let mut permissions = std::fs::metadata(&ledger_path)
        .expect("ledger metadata")
        .permissions();
    permissions.set_mode(0o644);
    std::fs::set_permissions(&ledger_path, permissions).expect("set permissive ledger mode");
    assert!(FileIntegrationControlLedger::open(&ledger_path).is_err());

    let audit_path = root.path().join("audit.jsonl");
    drop(FileIntegrationControlAudit::open(&audit_path).expect("initial audit"));
    let mut permissions = std::fs::metadata(&audit_path)
        .expect("audit metadata")
        .permissions();
    permissions.set_mode(0o640);
    std::fs::set_permissions(&audit_path, permissions).expect("set permissive audit mode");
    assert!(FileIntegrationControlAudit::open(&audit_path).is_err());

    let real_parent = root.path().join("real-parent");
    std::fs::create_dir(&real_parent).expect("real parent");
    let linked_parent = root.path().join("linked-parent");
    symlink(&real_parent, &linked_parent).expect("parent symlink");
    assert!(
        FileIntegrationControlLedger::open(linked_parent.join("ledger.json")).is_err(),
        "a symbolic-link parent must not redirect durable control state"
    );
    assert!(
        FileIntegrationControlAudit::open(linked_parent.join("audit.jsonl")).is_err(),
        "a symbolic-link parent must not redirect audit evidence"
    );
}
