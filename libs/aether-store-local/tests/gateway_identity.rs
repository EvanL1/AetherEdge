#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use aether_application::EnrollGatewayWithAetherCloud;
use aether_domain::TimestampMs;
use aether_ports::{
    CloudEndpointPolicy, CloudEnrollmentClaim, CloudEnrollmentClient, CloudEnrollmentClientError,
    CloudEnrollmentReceipt, ConfiguredGatewayIdentity, EnrollmentIdempotencyKey,
    GatewayEnrollmentPhase, GatewayEnrollmentStatus, GatewayEnrollmentTarget, GatewayIdentityError,
    GatewayIdentityInitialization, GatewayIdentityKeyGenerator, GatewayIdentityStore,
    GatewayPrivateKeySeed, GatewayPublicKey, GeneratedGatewayIdentityKey, SecretMaterial,
};
use aether_store_local::{
    FileClaimedGatewayIdentitySource, FileGatewayIdentityStore, ManualClock,
    OsEd25519GatewayIdentityKeyGenerator,
};
use async_trait::async_trait;
use ed25519_dalek::SigningKey;
use sha2::{Digest as _, Sha256};
use tempfile::tempdir;
use uuid::Uuid;

const TENANT_ID: &str = "0190d8c5-a8dd-7c4e-8b2f-25cc70d02d31";
const PROJECT_ID: &str = "0190d8c5-a8dd-7c4e-8b2f-25cc70d02d32";
const GATEWAY_ID: &str = "0190d8c5-a8dd-7c4e-8b2f-25cc70d02d33";
const OTHER_GATEWAY_ID: &str = "0190d8c5-a8dd-7c4e-8b2f-25cc70d02d34";
const OTHER_IDEMPOTENCY_KEY: &str = "9fed195b-01bc-502d-8e5f-c7e804086868";
const SEED_FILE: &str = "gateway-identity.seed";
const STATE_FILE: &str = "gateway-enrollment.json";

fn target(gateway_id: &str) -> GatewayEnrollmentTarget {
    GatewayEnrollmentTarget::new(
        "https://cloud.example",
        TENANT_ID,
        PROJECT_ID,
        gateway_id,
        CloudEndpointPolicy::Production,
    )
    .expect("valid target")
}

fn key_from_seed(seed: [u8; 32]) -> GeneratedGatewayIdentityKey {
    let signing_key = SigningKey::from_bytes(&seed);
    GeneratedGatewayIdentityKey::new(
        GatewayPrivateKeySeed::from_bytes(seed),
        GatewayPublicKey::from_bytes(signing_key.verifying_key().to_bytes()),
    )
}

fn idempotency_key(value: &str) -> EnrollmentIdempotencyKey {
    EnrollmentIdempotencyKey::new(Uuid::parse_str(value).expect("UUID"))
        .expect("non-nil idempotency key")
}

fn initialization(gateway_id: &str, seed: [u8; 32]) -> GatewayIdentityInitialization {
    GatewayIdentityInitialization::new(target(gateway_id), key_from_seed(seed))
}

fn identity_directory(root: &Path) -> PathBuf {
    fs::canonicalize(root)
        .expect("canonical temporary root")
        .join("data")
        .join("uplink")
        .join("identity")
}

fn file_mode(path: &Path) -> u32 {
    fs::symlink_metadata(path)
        .expect("file metadata")
        .permissions()
        .mode()
        & 0o7777
}

fn directory_snapshot(path: &Path) -> Vec<(PathBuf, u32, Vec<u8>)> {
    if !path.exists() {
        return Vec::new();
    }
    let mut entries = fs::read_dir(path)
        .expect("read directory")
        .map(|entry| entry.expect("directory entry").path())
        .collect::<Vec<_>>();
    entries.sort();
    entries
        .into_iter()
        .map(|entry| {
            let metadata = fs::symlink_metadata(&entry).expect("entry metadata");
            let bytes = if metadata.file_type().is_file() {
                fs::read(&entry).expect("entry bytes")
            } else {
                Vec::new()
            };
            (entry, metadata.permissions().mode() & 0o7777, bytes)
        })
        .collect()
}

async fn persist_fixed(
    store: &FileGatewayIdentityStore,
    gateway_id: &str,
    seed: [u8; 32],
) -> ConfiguredGatewayIdentity {
    store
        .persist_key_generated(initialization(gateway_id, seed), TimestampMs::new(1_000))
        .await
        .expect("persist key-generated identity");
    let GatewayEnrollmentStatus::Configured(identity) = store.load().await.expect("load identity")
    else {
        panic!("identity must be configured");
    };
    identity
}

#[test]
fn os_key_generator_produces_matching_unique_ed25519_keys_and_sha256_fingerprints() {
    let generator = OsEd25519GatewayIdentityKeyGenerator;
    let first = generator.generate().expect("first key");
    let second = generator.generate().expect("second key");
    assert_ne!(first.public_key(), second.public_key());

    let first_debug = format!("{first:?}");
    let (seed, public_key) = first.into_parts();
    let signing_key = SigningKey::from_bytes(seed.expose());
    assert_eq!(
        signing_key.verifying_key().to_bytes(),
        *public_key.as_bytes()
    );
    let expected_fingerprint = format!("{:x}", Sha256::digest(public_key.as_bytes()));
    assert_eq!(public_key.fingerprint().as_str(), expected_fingerprint);
    assert!(first_debug.contains("[REDACTED]"));
    assert!(!format!("{seed:?}").contains(&hex(seed.expose())));
}

#[tokio::test]
async fn load_is_side_effect_free_and_initial_persistence_is_owner_only() {
    let root = tempdir().expect("temporary root");
    let identity_dir = identity_directory(root.path());
    let store = FileGatewayIdentityStore::new(&identity_dir).expect("store configuration");

    let before = directory_snapshot(root.path());
    assert_eq!(
        store.load().await.expect("unconfigured status"),
        GatewayEnrollmentStatus::Unconfigured
    );
    assert_eq!(directory_snapshot(root.path()), before);
    assert!(!identity_dir.exists());
    assert_eq!(
        FileGatewayIdentityStore::new("relative/identity")
            .expect_err("relative storage paths are unsafe"),
        GatewayIdentityError::InsecureStorage
    );

    persist_fixed(&store, GATEWAY_ID, [0x11; 32]).await;

    let seed_path = identity_dir.join(SEED_FILE);
    let state_path = identity_dir.join(STATE_FILE);
    assert_eq!(file_mode(&identity_dir), 0o700);
    assert_eq!(file_mode(&seed_path), 0o600);
    assert_eq!(file_mode(&state_path), 0o600);
    assert_eq!(fs::read(&seed_path).expect("seed").len(), 32);
    let state = fs::read_to_string(&state_path).expect("state JSON");
    assert!(!state.contains("private"));
    assert!(!state.contains("seed"));
    assert!(!state.contains("token"));

    let parent = identity_dir.parent().expect("identity parent");
    let lock = fs::read_dir(parent)
        .expect("identity parent")
        .map(|entry| entry.expect("entry").path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".enrollment.lock"))
        })
        .expect("cross-process lock");
    assert_eq!(file_mode(&lock), 0o600);

    let before_load = directory_snapshot(&identity_dir);
    store.load().await.expect("read-only reload");
    assert_eq!(directory_snapshot(&identity_dir), before_load);
    assert!(
        fs::read_dir(&identity_dir)
            .expect("identity directory")
            .all(|entry| !entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .contains(".tmp-"))
    );
}

#[tokio::test]
async fn pending_and_claimed_transitions_are_atomic_idempotent_and_never_downgrade() {
    let root = tempdir().expect("temporary root");
    let identity_dir = identity_directory(root.path());
    let store = FileGatewayIdentityStore::new(&identity_dir).expect("store configuration");
    let key_generated = persist_fixed(&store, GATEWAY_ID, [0x22; 32]).await;
    let state_path = identity_dir.join(STATE_FILE);
    let seed_path = identity_dir.join(SEED_FILE);
    let initial_state_inode = fs::metadata(&state_path).expect("state metadata").ino();
    let seed_inode = fs::metadata(&seed_path).expect("seed metadata").ino();
    let claim_key = key_generated.stable_idempotency_key();

    store
        .mark_claim_pending(&key_generated, &claim_key, TimestampMs::new(2_000))
        .await
        .expect("mark pending");
    let pending_bytes = fs::read(&state_path).expect("pending state");
    let pending_inode = fs::metadata(&state_path).expect("pending metadata").ino();
    assert_ne!(initial_state_inode, pending_inode);
    let GatewayEnrollmentStatus::Configured(pending) =
        store.load().await.expect("pending identity")
    else {
        panic!("pending identity must be configured");
    };
    assert!(matches!(
        pending.phase(),
        GatewayEnrollmentPhase::ClaimPending(_)
    ));

    store
        .mark_claim_pending(&pending, &claim_key, TimestampMs::new(9_999))
        .await
        .expect("same pending transition is idempotent");
    assert_eq!(fs::read(&state_path).expect("same pending"), pending_bytes);
    assert_eq!(
        store
            .mark_claim_pending(
                &pending,
                &idempotency_key(OTHER_IDEMPOTENCY_KEY),
                TimestampMs::new(3_000),
            )
            .await,
        Err(GatewayIdentityError::InvalidState)
    );

    store
        .mark_claimed(&pending, &claim_key, 17, TimestampMs::new(4_000))
        .await
        .expect("mark claimed");
    let claimed_bytes = fs::read(&state_path).expect("claimed state");
    let GatewayEnrollmentStatus::Configured(claimed) =
        store.load().await.expect("claimed identity")
    else {
        panic!("claimed identity must be configured");
    };
    assert_eq!(claimed.claimed_revision(), Some(17));
    assert_eq!(
        fs::metadata(&seed_path).expect("seed metadata").ino(),
        seed_inode
    );

    store
        .mark_claimed(&claimed, &claim_key, 17, TimestampMs::new(8_000))
        .await
        .expect("same claimed transition is idempotent");
    assert_eq!(fs::read(&state_path).expect("same claimed"), claimed_bytes);
    assert_eq!(
        store
            .mark_claimed(&claimed, &claim_key, 18, TimestampMs::new(8_001))
            .await,
        Err(GatewayIdentityError::Conflict)
    );
    store
        .persist_key_generated(
            initialization(GATEWAY_ID, [0x22; 32]),
            TimestampMs::new(8_002),
        )
        .await
        .expect("same initial identity does not downgrade claimed");
    assert_eq!(
        fs::read(&state_path).expect("claimed remains"),
        claimed_bytes
    );
    assert_eq!(
        store
            .persist_key_generated(
                initialization(OTHER_GATEWAY_ID, [0x33; 32]),
                TimestampMs::new(8_003),
            )
            .await,
        Err(GatewayIdentityError::Conflict)
    );
    assert_eq!(
        store
            .persist_key_generated(
                initialization(GATEWAY_ID, [0x34; 32]),
                TimestampMs::new(8_004),
            )
            .await,
        Err(GatewayIdentityError::Conflict)
    );
}

#[tokio::test]
async fn claimed_identity_source_returns_seed_only_after_claim_and_validates_the_key_pair() {
    use aether_ports::ClaimedGatewayIdentitySource as _;

    let root = tempdir().expect("temporary root");
    let identity_dir = identity_directory(root.path());
    let store = FileGatewayIdentityStore::new(&identity_dir).expect("store configuration");
    let claimed_source =
        FileClaimedGatewayIdentitySource::new(&identity_dir).expect("claimed source configuration");
    let generated = persist_fixed(&store, GATEWAY_ID, [0x44; 32]).await;
    assert!(
        claimed_source
            .load_claimed_identity()
            .await
            .expect("not claimed")
            .is_none()
    );
    let claim_key = generated.stable_idempotency_key();
    store
        .mark_claim_pending(&generated, &claim_key, TimestampMs::new(2_000))
        .await
        .expect("pending");
    let GatewayEnrollmentStatus::Configured(pending) =
        store.load().await.expect("pending identity")
    else {
        panic!("pending identity");
    };
    store
        .mark_claimed(&pending, &claim_key, 7, TimestampMs::new(3_000))
        .await
        .expect("claimed");

    let claimed = claimed_source
        .load_claimed_identity()
        .await
        .expect("claimed identity")
        .expect("claimed material");
    assert_eq!(claimed.private_seed().expose(), &[0x44; 32]);
    assert_eq!(claimed.target().gateway_id().to_string(), GATEWAY_ID);
    assert_eq!(claimed.revision(), 7);

    fs::write(identity_dir.join(SEED_FILE), [0x55; 32]).expect("corrupt seed");
    assert_eq!(store.load().await, Err(GatewayIdentityError::CorruptState));
    assert!(matches!(
        claimed_source.load_claimed_identity().await,
        Err(GatewayIdentityError::CorruptState)
    ));

    fs::write(identity_dir.join(SEED_FILE), [0x55; 31]).expect("truncate seed");
    assert_eq!(store.load().await, Err(GatewayIdentityError::CorruptState));
    assert!(matches!(
        claimed_source.load_claimed_identity().await,
        Err(GatewayIdentityError::CorruptState)
    ));
}

#[tokio::test]
async fn enrollment_status_rejects_same_length_seed_corruption_in_every_phase() {
    for phase in ["key-generated", "claim-pending", "claimed"] {
        let root = tempdir().expect("temporary root");
        let identity_dir = identity_directory(root.path());
        let store = FileGatewayIdentityStore::new(&identity_dir).expect("store configuration");
        let key_generated = persist_fixed(&store, GATEWAY_ID, [0x56; 32]).await;
        let claim_key = key_generated.stable_idempotency_key();

        if phase != "key-generated" {
            store
                .mark_claim_pending(&key_generated, &claim_key, TimestampMs::new(2_000))
                .await
                .expect("pending");
        }
        if phase == "claimed" {
            let GatewayEnrollmentStatus::Configured(pending) =
                store.load().await.expect("pending identity")
            else {
                panic!("pending identity");
            };
            store
                .mark_claimed(&pending, &claim_key, 9, TimestampMs::new(3_000))
                .await
                .expect("claimed");
        }

        fs::write(identity_dir.join(SEED_FILE), [0x58; 32]).expect("corrupt seed");
        assert_eq!(
            store.load().await,
            Err(GatewayIdentityError::CorruptState),
            "{phase}"
        );
    }
}

#[tokio::test]
async fn persisted_pending_and_claimed_keys_must_match_the_stable_derivation() {
    use aether_ports::ClaimedGatewayIdentitySource as _;

    for phase in ["claim-pending", "claimed"] {
        let root = tempdir().expect("temporary root");
        let identity_dir = identity_directory(root.path());
        let store = FileGatewayIdentityStore::new(&identity_dir).expect("store configuration");
        let claimed_source = FileClaimedGatewayIdentitySource::new(&identity_dir)
            .expect("claimed source configuration");
        let key_generated = persist_fixed(&store, GATEWAY_ID, [0x57; 32]).await;
        let claim_key = key_generated.stable_idempotency_key();
        store
            .mark_claim_pending(&key_generated, &claim_key, TimestampMs::new(2_000))
            .await
            .expect("pending");
        if phase == "claimed" {
            let GatewayEnrollmentStatus::Configured(pending) =
                store.load().await.expect("pending identity")
            else {
                panic!("pending identity");
            };
            store
                .mark_claimed(&pending, &claim_key, 9, TimestampMs::new(3_000))
                .await
                .expect("claimed");
        }

        let state_path = identity_dir.join(STATE_FILE);
        let mut document: serde_json::Value =
            serde_json::from_slice(&fs::read(&state_path).expect("state")).expect("state JSON");
        document["phase"]["idempotencyKey"] =
            serde_json::Value::String(OTHER_IDEMPOTENCY_KEY.to_string());
        fs::write(
            &state_path,
            serde_json::to_vec(&document).expect("tampered state"),
        )
        .expect("replace state");

        assert_eq!(
            store.load().await,
            Err(GatewayIdentityError::CorruptState),
            "{phase}"
        );
        assert!(
            matches!(
                claimed_source.load_claimed_identity().await,
                Err(GatewayIdentityError::CorruptState)
            ),
            "{phase}"
        );
    }
}

#[tokio::test]
async fn strict_state_decoder_rejects_unknown_fields_schema_and_noncanonical_key_data() {
    let fixtures = [
        "unknown-field",
        "unknown-phase-field",
        "unknown-schema",
        "bad-fingerprint",
        "padded-public-key",
    ];

    for case in fixtures {
        let root = tempdir().expect("temporary root");
        let identity_dir = identity_directory(root.path());
        let store = FileGatewayIdentityStore::new(&identity_dir).expect("store configuration");
        persist_fixed(&store, GATEWAY_ID, [0x66; 32]).await;
        let state_path = identity_dir.join(STATE_FILE);
        let mut document: serde_json::Value =
            serde_json::from_slice(&fs::read(&state_path).expect("state")).expect("state JSON");
        match case {
            "unknown-field" => {
                document
                    .as_object_mut()
                    .expect("state object")
                    .insert("unexpected".to_string(), serde_json::Value::Bool(true));
            },
            "unknown-phase-field" => {
                document["phase"]
                    .as_object_mut()
                    .expect("phase object")
                    .insert("unexpected".to_string(), serde_json::Value::Bool(true));
            },
            "unknown-schema" => {
                document["schema"] = serde_json::Value::String(
                    "aether.edge.gateway-enrollment-state.v2".to_string(),
                );
            },
            "bad-fingerprint" => {
                document["fingerprint"] = serde_json::Value::String("00".to_string());
            },
            "padded-public-key" => {
                document["publicKey"] = serde_json::Value::String("AAAAAAAAAAA=".to_string());
            },
            _ => panic!("unhandled fixture: {case}"),
        }
        fs::write(
            &state_path,
            serde_json::to_vec(&document).expect("fixture JSON"),
        )
        .expect("replace fixture");

        assert_eq!(
            store.load().await,
            Err(GatewayIdentityError::CorruptState),
            "{case}"
        );
    }

    let root = tempdir().expect("temporary root");
    let identity_dir = identity_directory(root.path());
    let store = FileGatewayIdentityStore::new(&identity_dir).expect("store configuration");
    persist_fixed(&store, GATEWAY_ID, [0x67; 32]).await;
    let state_path = identity_dir.join(STATE_FILE);
    fs::write(&state_path, vec![b'x'; 16 * 1024 + 1]).expect("oversized state");
    assert_eq!(store.load().await, Err(GatewayIdentityError::CorruptState));
}

#[tokio::test]
async fn unsafe_paths_files_permissions_symlinks_and_hardlinks_fail_closed() {
    let root = tempdir().expect("temporary root");
    let root_path = fs::canonicalize(root.path()).expect("canonical temporary root");

    let unsafe_parent = root_path.join("world-writable");
    fs::create_dir(&unsafe_parent).expect("unsafe parent");
    fs::set_permissions(&unsafe_parent, fs::Permissions::from_mode(0o777))
        .expect("unsafe permissions");
    let store =
        FileGatewayIdentityStore::new(unsafe_parent.join("identity")).expect("syntactic path");
    assert_eq!(
        store.load().await,
        Err(GatewayIdentityError::InsecureStorage)
    );
    fs::set_permissions(&unsafe_parent, fs::Permissions::from_mode(0o700))
        .expect("restore cleanup permissions");

    let outside = root_path.join("outside");
    fs::create_dir(&outside).expect("outside directory");
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o700)).expect("outside permissions");
    let linked_parent = root_path.join("linked-parent");
    symlink(&outside, &linked_parent).expect("ancestor symlink");
    let store =
        FileGatewayIdentityStore::new(linked_parent.join("identity")).expect("syntactic path");
    assert_eq!(
        store.load().await,
        Err(GatewayIdentityError::InsecureStorage)
    );

    let identity_dir = identity_directory(root.path());
    let store = FileGatewayIdentityStore::new(&identity_dir).expect("store configuration");
    persist_fixed(&store, GATEWAY_ID, [0x77; 32]).await;
    let seed_path = identity_dir.join(SEED_FILE);
    let hardlink = root_path.join("seed-copy");
    fs::hard_link(&seed_path, &hardlink).expect("seed hardlink");
    assert_eq!(
        store.load().await,
        Err(GatewayIdentityError::InsecureStorage)
    );
    fs::remove_file(&hardlink).expect("remove hardlink");

    fs::set_permissions(&seed_path, fs::Permissions::from_mode(0o644)).expect("weaken seed");
    assert_eq!(
        store.load().await,
        Err(GatewayIdentityError::InsecureStorage)
    );
    fs::set_permissions(&seed_path, fs::Permissions::from_mode(0o600)).expect("restore seed");

    fs::set_permissions(&seed_path, fs::Permissions::from_mode(0o4600))
        .expect("set special seed mode");
    if file_mode(&seed_path) & 0o4000 != 0 {
        assert_eq!(
            store.load().await,
            Err(GatewayIdentityError::InsecureStorage)
        );
    }
    fs::set_permissions(&seed_path, fs::Permissions::from_mode(0o600)).expect("restore seed");

    let state_path = identity_dir.join(STATE_FILE);
    fs::set_permissions(&state_path, fs::Permissions::from_mode(0o644)).expect("weaken state");
    assert_eq!(
        store.load().await,
        Err(GatewayIdentityError::InsecureStorage)
    );
    fs::set_permissions(&state_path, fs::Permissions::from_mode(0o600)).expect("restore state");

    fs::set_permissions(&identity_dir, fs::Permissions::from_mode(0o755))
        .expect("weaken identity directory");
    assert_eq!(
        store.load().await,
        Err(GatewayIdentityError::InsecureStorage)
    );
    fs::set_permissions(&identity_dir, fs::Permissions::from_mode(0o700))
        .expect("restore identity directory");

    fs::set_permissions(&identity_dir, fs::Permissions::from_mode(0o2700))
        .expect("set special identity directory mode");
    if file_mode(&identity_dir) & 0o2000 != 0 {
        assert_eq!(
            store.load().await,
            Err(GatewayIdentityError::InsecureStorage)
        );
    }
    fs::set_permissions(&identity_dir, fs::Permissions::from_mode(0o700))
        .expect("restore identity directory");

    let outside_state = root_path.join("outside-state");
    fs::write(&outside_state, b"outside").expect("outside state");
    fs::remove_file(&state_path).expect("remove state");
    symlink(&outside_state, &state_path).expect("state symlink");
    assert_eq!(
        store.load().await,
        Err(GatewayIdentityError::InsecureStorage)
    );
    fs::remove_file(&state_path).expect("remove state symlink");
    fs::create_dir(&state_path).expect("state directory");
    fs::set_permissions(&state_path, fs::Permissions::from_mode(0o700))
        .expect("state directory permissions");
    assert_eq!(
        store.load().await,
        Err(GatewayIdentityError::InsecureStorage)
    );

    let lock_case_parent = root_path.join("lock-case").join("uplink");
    fs::create_dir_all(&lock_case_parent).expect("lock parent");
    let lock_target = root_path.join("lock-target");
    fs::write(&lock_target, b"outside").expect("lock target");
    symlink(
        &lock_target,
        lock_case_parent.join(".identity.enrollment.lock"),
    )
    .expect("lock symlink");
    let lock_store =
        FileGatewayIdentityStore::new(lock_case_parent.join("identity")).expect("lock store");
    assert_eq!(
        lock_store
            .persist_key_generated(initialization(GATEWAY_ID, [0x78; 32]), TimestampMs::new(1),)
            .await,
        Err(GatewayIdentityError::InsecureStorage)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_process_style_initializers_never_overwrite_an_identity() {
    let root = tempdir().expect("temporary root");
    let identity_dir = identity_directory(root.path());
    let first = Arc::new(FileGatewayIdentityStore::new(&identity_dir).expect("first store"));
    let second = Arc::new(FileGatewayIdentityStore::new(&identity_dir).expect("second store"));

    let first_task = tokio::spawn(async move {
        first
            .persist_key_generated(initialization(GATEWAY_ID, [0x81; 32]), TimestampMs::new(1))
            .await
    });
    let second_task = tokio::spawn(async move {
        second
            .persist_key_generated(
                initialization(OTHER_GATEWAY_ID, [0x82; 32]),
                TimestampMs::new(2),
            )
            .await
    });
    let results = [
        first_task.await.expect("first task"),
        second_task.await.expect("second task"),
    ];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| **result == Err(GatewayIdentityError::Conflict))
            .count(),
        1
    );

    let store = FileGatewayIdentityStore::new(&identity_dir).expect("reopen store");
    let GatewayEnrollmentStatus::Configured(identity) =
        store.load().await.expect("winning identity")
    else {
        panic!("one complete identity must win");
    };
    assert!(matches!(
        identity.target().gateway_id().to_string().as_str(),
        GATEWAY_ID | OTHER_GATEWAY_ID
    ));
}

struct ObservingClient {
    store: Arc<FileGatewayIdentityStore>,
    observed_pending: AtomicBool,
}

#[async_trait]
impl CloudEnrollmentClient for ObservingClient {
    async fn claim(
        &self,
        claim: CloudEnrollmentClaim,
    ) -> Result<CloudEnrollmentReceipt, CloudEnrollmentClientError> {
        let status = self
            .store
            .load()
            .await
            .map_err(|_| CloudEnrollmentClientError::Unavailable)?;
        let GatewayEnrollmentStatus::Configured(identity) = status else {
            return Err(CloudEnrollmentClientError::InvalidConfiguration);
        };
        if !matches!(identity.phase(), GatewayEnrollmentPhase::ClaimPending(_)) {
            return Err(CloudEnrollmentClientError::InvalidConfiguration);
        }
        self.observed_pending.store(true, Ordering::Release);
        Ok(CloudEnrollmentReceipt::new(
            claim.target().gateway_id().to_string(),
            23,
        ))
    }
}

#[tokio::test]
async fn application_observes_durable_pending_state_before_http_and_never_persists_token() {
    let root = tempdir().expect("temporary root");
    let identity_dir = identity_directory(root.path());
    let store = Arc::new(FileGatewayIdentityStore::new(&identity_dir).expect("store"));
    let client = Arc::new(ObservingClient {
        store: Arc::clone(&store),
        observed_pending: AtomicBool::new(false),
    });
    let use_case = EnrollGatewayWithAetherCloud::new(
        client.clone(),
        Arc::new(OsEd25519GatewayIdentityKeyGenerator),
        store,
        Arc::new(ManualClock::new(TimestampMs::new(1_000))),
    );
    let token = "token-must-never-reach-local-storage";

    let result = use_case
        .enroll(
            target(GATEWAY_ID),
            SecretMaterial::new(token).expect("secret"),
        )
        .await
        .expect("enrollment");

    assert!(client.observed_pending.load(Ordering::Acquire));
    assert_eq!(result.identity().claimed_revision(), Some(23));
    for entry in fs::read_dir(&identity_dir).expect("identity files") {
        let path = entry.expect("entry").path();
        if path.is_file() {
            let bytes = fs::read(path).expect("identity bytes");
            assert!(
                !bytes
                    .windows(token.len())
                    .any(|window| window == token.as_bytes())
            );
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(encoded, "{byte:02x}").expect("format byte");
    }
    encoded
}
