use std::fs;

use aether_store_local::{
    CloudLinkChallengeLedgerError, CloudLinkChallengeReservation, FileCloudLinkChallengeLedger,
};

const CHALLENGE_ID: &str = "22222222-2222-4222-8222-222222222222";
const OTHER_CHALLENGE_ID: &str = "55555555-5555-4555-8555-555555555555";
const CHALLENGE: &[u8] = br#"{"challenge":"one"}"#;
const REQUEST: &[u8] = br#"{"request":"one"}"#;
const HELLO: &[u8] = br#"{"hello":"one"}"#;

fn expect_prepare(reservation: CloudLinkChallengeReservation) -> (Vec<u8>, Vec<u8>) {
    match reservation {
        CloudLinkChallengeReservation::Prepare { challenge, request } => (challenge, request),
        CloudLinkChallengeReservation::RetryHello(_) => {
            panic!("expected persisted inputs, not an already prepared hello")
        },
    }
}

#[test]
fn duplicate_pending_challenge_retries_the_exact_persisted_hello_after_restart() {
    let directory = tempfile::tempdir().expect("temporary ledger directory");
    let path = directory.path().join("challenge-ledger.json");
    {
        let ledger = FileCloudLinkChallengeLedger::open(&path, 8).expect("challenge ledger");
        assert_eq!(
            ledger
                .prepare_request(REQUEST, 1_900, 1_000)
                .expect("persist request before publish")
                .payload(),
            REQUEST
        );
        let (challenge, request) = expect_prepare(
            ledger
                .reserve(CHALLENGE_ID, 2_000, CHALLENGE, REQUEST, 1_000)
                .expect("new challenge"),
        );
        assert_eq!(challenge, CHALLENGE);
        assert_eq!(request, REQUEST);
        assert_eq!(
            ledger
                .store_hello(CHALLENGE_ID, HELLO)
                .expect("persist hello"),
            HELLO
        );
    }

    let ledger = FileCloudLinkChallengeLedger::open(&path, 8).expect("reopened ledger");
    let persisted_request = ledger
        .prepare_request(
            br#"{"request":"new-nonce-must-not-replace-the-old-request"}"#,
            1_901,
            1_001,
        )
        .expect("retry persisted request");
    let retried = ledger
        .reserve(
            CHALLENGE_ID,
            2_000,
            CHALLENGE,
            persisted_request.payload(),
            1_001,
        )
        .expect("exact duplicate challenge");
    assert!(matches!(
        retried,
        CloudLinkChallengeReservation::RetryHello(ref hello) if hello == HELLO
    ));
}

#[test]
fn completed_or_conflicting_challenge_replay_is_rejected_without_mutation() {
    let directory = tempfile::tempdir().expect("temporary ledger directory");
    let path = directory.path().join("challenge-ledger.json");
    let ledger = FileCloudLinkChallengeLedger::open(&path, 8).expect("challenge ledger");
    ledger
        .prepare_request(REQUEST, 1_900, 1_000)
        .expect("persist request");
    ledger
        .reserve(CHALLENGE_ID, 2_000, CHALLENGE, REQUEST, 1_000)
        .expect("new challenge");

    let conflict = ledger
        .reserve(
            CHALLENGE_ID,
            2_000,
            br#"{"challenge":"conflicting"}"#,
            REQUEST,
            1_001,
        )
        .expect_err("same challenge identity with different bytes must fail");
    assert_eq!(conflict, CloudLinkChallengeLedgerError::ConflictingReplay);

    ledger
        .store_hello(CHALLENGE_ID, HELLO)
        .expect("persist hello");
    ledger.complete(CHALLENGE_ID).expect("consume challenge");
    let completed = ledger
        .reserve(CHALLENGE_ID, 2_000, CHALLENGE, REQUEST, 1_002)
        .expect_err("completed challenge must never establish another session");
    assert_eq!(completed, CloudLinkChallengeLedgerError::CompletedReplay);
    let rollback = ledger
        .reserve(CHALLENGE_ID, 2_000, CHALLENGE, REQUEST, 900)
        .expect_err("wall-clock rollback must not resurrect a consumed challenge");
    assert_eq!(rollback, CloudLinkChallengeLedgerError::CompletedReplay);
}

#[test]
fn capacity_evicts_the_oldest_completed_record_without_waiting_for_wall_clock_expiry() {
    let directory = tempfile::tempdir().expect("temporary ledger directory");
    let path = directory.path().join("challenge-ledger.json");
    let ledger = FileCloudLinkChallengeLedger::open(&path, 1).expect("challenge ledger");
    ledger
        .prepare_request(REQUEST, 1_900, 1_000)
        .expect("persist first request");
    ledger
        .reserve(CHALLENGE_ID, 2_000, CHALLENGE, REQUEST, 1_000)
        .expect("first live challenge");
    ledger
        .store_hello(CHALLENGE_ID, HELLO)
        .expect("persist first hello");
    ledger
        .complete(CHALLENGE_ID)
        .expect("complete first request");
    let second_request = ledger
        .prepare_request(br#"{"request":"two"}"#, 2_900, 1_500)
        .expect("persist second request");

    let replacement = ledger
        .reserve(
            OTHER_CHALLENGE_ID,
            3_000,
            br#"{"challenge":"two"}"#,
            second_request.payload(),
            1_500,
        )
        .expect("oldest completed record is the only safe capacity victim");
    assert!(matches!(
        replacement,
        CloudLinkChallengeReservation::Prepare { .. }
    ));
    let document: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).expect("ledger bytes")).expect("ledger JSON");
    let identities = document["records"]
        .as_array()
        .expect("records")
        .iter()
        .filter_map(|record| record["challenge_id"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(identities, vec![OTHER_CHALLENGE_ID]);
}

#[test]
fn capacity_never_evicts_a_pending_challenge_and_ids_are_canonical_uuids() {
    let directory = tempfile::tempdir().expect("temporary ledger directory");
    let path = directory.path().join("challenge-ledger.json");
    let ledger = FileCloudLinkChallengeLedger::open(&path, 1).expect("challenge ledger");
    ledger
        .prepare_request(REQUEST, 1_900, 1_000)
        .expect("persist request");
    assert_eq!(
        ledger
            .reserve("NOT-A-UUID", 2_000, CHALLENGE, REQUEST, 1_000)
            .expect_err("challenge IDs are exact canonical lowercase UUIDs"),
        CloudLinkChallengeLedgerError::InvalidInput
    );
    ledger
        .reserve(CHALLENGE_ID, 2_000, CHALLENGE, REQUEST, 1_000)
        .expect("first pending challenge");
    assert_eq!(
        ledger
            .reserve(
                OTHER_CHALLENGE_ID,
                2_500,
                br#"{"challenge":"two"}"#,
                REQUEST,
                1_001,
            )
            .expect_err("pending challenge must not be evicted"),
        CloudLinkChallengeLedgerError::CapacityExceeded
    );
}

#[test]
fn completion_atomically_removes_raw_authentication_transcripts() {
    const SENSITIVE_CREDENTIAL: &str = "sensitive-credential-17";
    const SENSITIVE_NONCE: &str = "NNNNNNNNNNNNNNNNNNNNNNNNNNNNNNNNNNNNNNNNNNN";
    const SENSITIVE_CLOUD_SIGNATURE: &str = "cloud-signature-sensitive";
    const SENSITIVE_GATEWAY_SIGNATURE: &str = "gateway-signature-sensitive";

    let directory = tempfile::tempdir().expect("temporary ledger directory");
    let path = directory.path().join("challenge-ledger.json");
    {
        let ledger = FileCloudLinkChallengeLedger::open(&path, 8).expect("challenge ledger");
        let request = format!(
            r#"{{"credential_id":"{SENSITIVE_CREDENTIAL}","client_nonce":"{SENSITIVE_NONCE}"}}"#
        );
        let challenge = format!(
            r#"{{"cloud_nonce":"{SENSITIVE_NONCE}","cloud_signature":"{SENSITIVE_CLOUD_SIGNATURE}"}}"#
        );
        let hello = format!(r#"{{"gateway_signature":"{SENSITIVE_GATEWAY_SIGNATURE}"}}"#);
        ledger
            .prepare_request(request.as_bytes(), 1_900, 1_000)
            .expect("persist request");
        ledger
            .reserve(
                CHALLENGE_ID,
                2_000,
                challenge.as_bytes(),
                request.as_bytes(),
                1_000,
            )
            .expect("persist challenge");
        ledger
            .store_hello(CHALLENGE_ID, hello.as_bytes())
            .expect("persist hello");
        ledger.complete(CHALLENGE_ID).expect("complete challenge");
    }

    let persisted = fs::read_to_string(&path).expect("completed ledger");
    for sensitive in [
        SENSITIVE_CREDENTIAL,
        SENSITIVE_NONCE,
        SENSITIVE_CLOUD_SIGNATURE,
        SENSITIVE_GATEWAY_SIGNATURE,
    ] {
        assert!(
            !persisted.contains(sensitive),
            "completed state must erase raw authentication transcript material"
        );
    }
    let reopened = FileCloudLinkChallengeLedger::open(&path, 8).expect("reopen compact ledger");
    assert_eq!(
        reopened
            .reserve(
                CHALLENGE_ID,
                2_000,
                br#"{"cloud_nonce":"different","cloud_signature":"different"}"#,
                REQUEST,
                900,
            )
            .expect_err("completed identity remains replay protected"),
        CloudLinkChallengeLedgerError::ConflictingReplay
    );
}

#[test]
fn ledger_rejects_expired_input_and_conflicting_hello_rewrites() {
    let directory = tempfile::tempdir().expect("temporary ledger directory");
    let path = directory.path().join("challenge-ledger.json");
    let ledger = FileCloudLinkChallengeLedger::open(&path, 8).expect("challenge ledger");
    ledger
        .prepare_request(REQUEST, 1_900, 900)
        .expect("persist request");
    assert_eq!(
        ledger
            .reserve(CHALLENGE_ID, 1_000, CHALLENGE, REQUEST, 1_000)
            .expect_err("expiry equality is expired"),
        CloudLinkChallengeLedgerError::MessageExpired
    );
    ledger
        .reserve(CHALLENGE_ID, 2_000, CHALLENGE, REQUEST, 1_000)
        .expect("new challenge");
    ledger
        .store_hello(CHALLENGE_ID, HELLO)
        .expect("first hello");
    assert_eq!(
        ledger
            .store_hello(CHALLENGE_ID, br#"{"hello":"conflicting"}"#)
            .expect_err("one challenge cannot acquire a different hello"),
        CloudLinkChallengeLedgerError::ConflictingReplay
    );
}

#[cfg(unix)]
#[test]
fn ledger_and_lock_files_are_exactly_owner_read_write_and_rewrites_are_atomic() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir().expect("temporary ledger directory");
    let path = directory.path().join("challenge-ledger.json");
    let ledger = FileCloudLinkChallengeLedger::open(&path, 8).expect("challenge ledger");
    ledger
        .prepare_request(REQUEST, 1_900, 1_000)
        .expect("persist request");
    ledger
        .reserve(CHALLENGE_ID, 2_000, CHALLENGE, REQUEST, 1_000)
        .expect("persist challenge");

    assert_eq!(
        fs::metadata(&path)
            .expect("ledger metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let lock_path = directory.path().join("challenge-ledger.json.lock");
    assert_eq!(
        fs::metadata(&lock_path)
            .expect("lock metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    serde_json::from_slice::<serde_json::Value>(&fs::read(&path).expect("ledger bytes"))
        .expect("complete JSON after atomic replacement");
    assert!(
        fs::read_dir(directory.path())
            .expect("ledger directory")
            .all(|entry| !entry
                .expect("directory entry")
                .file_name()
                .to_string_lossy()
                .contains(".tmp-")),
        "an atomic rewrite must not leave a transcript-bearing temporary file"
    );
}

#[test]
fn request_is_persisted_before_publish_and_retried_exactly_after_a_crash() {
    let directory = tempfile::tempdir().expect("temporary ledger directory");
    let path = directory.path().join("challenge-ledger.json");
    {
        let ledger = FileCloudLinkChallengeLedger::open(&path, 8).expect("challenge ledger");
        assert_eq!(
            ledger
                .prepare_request(REQUEST, 1_900, 1_000)
                .expect("persist request before first publish")
                .payload(),
            REQUEST
        );
    }

    let ledger = FileCloudLinkChallengeLedger::open(&path, 8).expect("reopened ledger");
    let persisted = ledger
        .prepare_request(br#"{"request":"new-random-client-nonce"}"#, 1_901, 1_001)
        .expect("retry exact pending request");
    assert_eq!(persisted.payload(), REQUEST);
    assert_eq!(persisted.expires_at_ms(), 1_900);
    let (challenge, request) = expect_prepare(
        ledger
            .reserve(CHALLENGE_ID, 2_000, CHALLENGE, persisted.payload(), 1_002)
            .expect("accept challenge for original request"),
    );
    assert_eq!(challenge, CHALLENGE);
    assert_eq!(request, REQUEST);
    ledger
        .store_hello(CHALLENGE_ID, HELLO)
        .expect("persist hello");
    ledger.complete(CHALLENGE_ID).expect("complete request");

    let next = br#"{"request":"next-session"}"#;
    assert_eq!(
        ledger
            .prepare_request(next, 3_000, 2_001)
            .expect("completed session clears pending request")
            .payload(),
        next
    );
}

#[cfg(unix)]
#[test]
fn reopening_a_ledger_with_weak_permissions_fails_closed() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir().expect("temporary ledger directory");
    let path = directory.path().join("challenge-ledger.json");
    drop(FileCloudLinkChallengeLedger::open(&path, 8).expect("challenge ledger"));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("weaken test permissions");

    assert_eq!(
        FileCloudLinkChallengeLedger::open(&path, 8)
            .expect_err("weak permissions must fail closed"),
        CloudLinkChallengeLedgerError::InsecurePermissions
    );
}

#[cfg(unix)]
#[test]
fn newly_created_ledger_directory_is_owner_only() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = tempfile::tempdir().expect("temporary root");
    let parent = root.path().join("private-ledger");
    let path = parent.join("challenge-ledger.json");

    drop(FileCloudLinkChallengeLedger::open(&path, 8).expect("challenge ledger"));

    assert_eq!(
        fs::symlink_metadata(&parent)
            .expect("ledger parent metadata")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[cfg(unix)]
#[test]
fn group_or_other_writable_parent_directory_is_rejected() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = tempfile::tempdir().expect("temporary root");
    let parent = root.path().join("shared-ledger");
    fs::create_dir(&parent).expect("shared parent");
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o770)).expect("weaken parent");
    let path = parent.join("challenge-ledger.json");

    assert_eq!(
        FileCloudLinkChallengeLedger::open(&path, 8)
            .expect_err("shared writable parent must fail closed"),
        CloudLinkChallengeLedgerError::InsecurePermissions
    );
}

#[cfg(unix)]
#[test]
fn ledger_and_lock_symbolic_links_are_rejected() {
    use std::os::unix::fs::{OpenOptionsExt as _, symlink};

    let root = tempfile::tempdir().expect("temporary root");
    let outside = root.path().join("outside");
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true).mode(0o600);
    drop(options.open(&outside).expect("outside target"));

    let ledger_path = root.path().join("ledger.json");
    symlink(&outside, &ledger_path).expect("ledger symbolic link");
    assert!(
        matches!(
            FileCloudLinkChallengeLedger::open(&ledger_path, 8),
            Err(CloudLinkChallengeLedgerError::Corrupt | CloudLinkChallengeLedgerError::Storage)
        ),
        "ledger symbolic links must fail closed"
    );
    fs::remove_file(&ledger_path).expect("remove ledger symbolic link");
    fs::remove_file(root.path().join("ledger.json.lock"))
        .expect("remove lock created before ledger validation");

    let lock_path = root.path().join("ledger.json.lock");
    symlink(&outside, &lock_path).expect("lock symbolic link");
    assert!(
        matches!(
            FileCloudLinkChallengeLedger::open(&ledger_path, 8),
            Err(CloudLinkChallengeLedgerError::Corrupt | CloudLinkChallengeLedgerError::Storage)
        ),
        "lock symbolic links must fail closed"
    );
}

#[cfg(unix)]
#[test]
fn multiply_linked_ledger_or_lock_files_are_rejected() {
    let root = tempfile::tempdir().expect("temporary root");
    let ledger_path = root.path().join("ledger.json");
    drop(FileCloudLinkChallengeLedger::open(&ledger_path, 8).expect("challenge ledger"));
    let ledger_alias = root.path().join("ledger-alias.json");
    fs::hard_link(&ledger_path, &ledger_alias).expect("ledger hard link");
    assert_eq!(
        FileCloudLinkChallengeLedger::open(&ledger_path, 8)
            .expect_err("a hard-linked transcript ledger must fail closed"),
        CloudLinkChallengeLedgerError::InsecurePermissions
    );
    fs::remove_file(&ledger_alias).expect("remove ledger hard link");

    let lock_path = root.path().join("ledger.json.lock");
    let lock_alias = root.path().join("lock-alias");
    fs::hard_link(&lock_path, &lock_alias).expect("lock hard link");
    assert_eq!(
        FileCloudLinkChallengeLedger::open(&ledger_path, 8)
            .expect_err("a hard-linked process lock must fail closed"),
        CloudLinkChallengeLedgerError::InsecurePermissions
    );
}
