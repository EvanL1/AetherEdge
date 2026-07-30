//! Crash-safe local Gateway enrollment identity.

use std::fmt;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use aether_domain::TimestampMs;
use aether_ports::{
    ClaimedGatewayIdentity, ClaimedGatewayIdentitySource, CloudEndpointPolicy,
    ConfiguredGatewayIdentity, EnrollmentIdempotencyKey, GatewayEnrollmentPhase,
    GatewayEnrollmentStatus, GatewayEnrollmentTarget, GatewayIdentityError,
    GatewayIdentityInitialization, GatewayIdentityKeyGenerator, GatewayIdentityStore,
    GatewayPrivateKeySeed, GatewayPublicKey, GeneratedGatewayIdentityKey,
};
use async_trait::async_trait;
use base64::Engine as _;
use ed25519_dalek::SigningKey;
use rand_core::{OsRng, RngCore as _};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::gateway_identity_fs::{ExclusiveIdentityLock, IdentityLayout, StoredIdentityFiles};

const STATE_SCHEMA: &str = "aether.edge.gateway-enrollment-state.v1";

/// Ed25519 identity generator backed directly by the operating-system CSPRNG.
#[derive(Debug, Clone, Copy, Default)]
pub struct OsEd25519GatewayIdentityKeyGenerator;

impl GatewayIdentityKeyGenerator for OsEd25519GatewayIdentityKeyGenerator {
    fn generate(&self) -> Result<GeneratedGatewayIdentityKey, GatewayIdentityError> {
        let mut seed = Zeroizing::new([0_u8; 32]);
        OsRng
            .try_fill_bytes(seed.as_mut())
            .map_err(|_| GatewayIdentityError::GenerationFailed)?;
        let signing_key = SigningKey::from_bytes(&seed);
        let public_key = GatewayPublicKey::from_bytes(signing_key.verifying_key().to_bytes());
        Ok(GeneratedGatewayIdentityKey::new(
            GatewayPrivateKeySeed::from_zeroizing(seed),
            public_key,
        ))
    }
}

/// Owner-only, atomically updated Gateway identity store.
///
/// `new` validates only the absolute path syntax and has no filesystem side
/// effects. The first mutation atomically installs a complete `0700`
/// directory. Reads never create directories, lock files, or state, and they
/// verify that the persisted seed still derives the public identity without
/// exposing that seed to the caller.
pub struct FileGatewayIdentityStore {
    layout: IdentityLayout,
    transition_lock: Mutex<()>,
}

impl FileGatewayIdentityStore {
    /// Configures a store rooted at an absolute identity directory such as
    /// `<data>/uplink/identity`.
    pub fn new(identity_directory: impl AsRef<Path>) -> Result<Self, GatewayIdentityError> {
        Ok(Self {
            layout: IdentityLayout::new(identity_directory.as_ref())?,
            transition_lock: Mutex::new(()),
        })
    }

    fn serialize_state(
        identity: &ConfiguredGatewayIdentity,
        phase: StoredPhase,
    ) -> Result<Vec<u8>, GatewayIdentityError> {
        let target = identity.target();
        let document = StoredState {
            schema: STATE_SCHEMA.to_string(),
            cloud_origin: target.cloud_origin().to_string(),
            tenant_id: target.tenant_id().to_string(),
            project_id: target.project_id().to_string(),
            gateway_id: target.gateway_id().to_string(),
            public_key: base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(identity.public_key().as_bytes()),
            fingerprint: identity.fingerprint().as_str().to_string(),
            phase,
        };
        serde_json::to_vec(&document).map_err(|_| GatewayIdentityError::InvalidState)
    }

    fn load_internal(&self) -> Result<Option<ConfiguredGatewayIdentity>, GatewayIdentityError> {
        self.layout
            .read_secret_identity()?
            .map(decode_stored_identity)
            .transpose()
            .map(|loaded| loaded.map(|(identity, _private_seed)| identity))
    }

    fn transition_guard(&self) -> Result<MutexGuard<'_, ()>, GatewayIdentityError> {
        self.transition_lock
            .lock()
            .map_err(|_| GatewayIdentityError::Unavailable)
    }

    fn write_guard(&self) -> Result<ExclusiveIdentityLock, GatewayIdentityError> {
        self.layout.lock_for_write()
    }

    fn require_matching_identity(
        current: &ConfiguredGatewayIdentity,
        expected: &ConfiguredGatewayIdentity,
    ) -> Result<(), GatewayIdentityError> {
        if current.target() == expected.target() && current.public_key() == expected.public_key() {
            Ok(())
        } else {
            Err(GatewayIdentityError::Conflict)
        }
    }

    fn ensure_input_key_matches(
        identity: &ConfiguredGatewayIdentity,
        idempotency_key: &EnrollmentIdempotencyKey,
    ) -> Result<(), GatewayIdentityError> {
        if identity.validated_idempotency_key()? == *idempotency_key {
            Ok(())
        } else {
            Err(GatewayIdentityError::InvalidState)
        }
    }
}

impl fmt::Debug for FileGatewayIdentityStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FileGatewayIdentityStore")
            .field("identity_directory", &self.layout.directory())
            .finish_non_exhaustive()
    }
}

/// Read-only source of claimed Gateway identity material for `aether-uplink`.
///
/// This handle is intentionally separate from [`FileGatewayIdentityStore`].
/// Enrollment and status callers therefore do not receive private-key read
/// capability merely because they can update public enrollment state.
pub struct FileClaimedGatewayIdentitySource {
    layout: IdentityLayout,
}

impl FileClaimedGatewayIdentitySource {
    /// Configures a claimed-identity source over the shared identity layout.
    ///
    /// Construction validates only the absolute path syntax and has no
    /// filesystem side effects.
    pub fn new(identity_directory: impl AsRef<Path>) -> Result<Self, GatewayIdentityError> {
        Ok(Self {
            layout: IdentityLayout::new(identity_directory.as_ref())?,
        })
    }
}

impl fmt::Debug for FileClaimedGatewayIdentitySource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FileClaimedGatewayIdentitySource")
            .field("identity_directory", &self.layout.directory())
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl GatewayIdentityStore for FileGatewayIdentityStore {
    async fn load(&self) -> Result<GatewayEnrollmentStatus, GatewayIdentityError> {
        let _transition = self.transition_guard()?;
        Ok(match self.load_internal()? {
            Some(identity) => GatewayEnrollmentStatus::Configured(identity),
            None => GatewayEnrollmentStatus::Unconfigured,
        })
    }

    async fn persist_key_generated(
        &self,
        initialization: GatewayIdentityInitialization,
        recorded_at: TimestampMs,
    ) -> Result<(), GatewayIdentityError> {
        let _transition = self.transition_guard()?;
        let (target, generated_key) = initialization.into_parts();
        let (private_seed, public_key) = generated_key.into_parts();
        let derived_public = SigningKey::from_bytes(private_seed.expose())
            .verifying_key()
            .to_bytes();
        if derived_public != *public_key.as_bytes() {
            return Err(GatewayIdentityError::InvalidState);
        }
        let desired = ConfiguredGatewayIdentity::key_generated(target, public_key);
        let _file_lock = self.write_guard()?;
        if let Some(current) = self.load_internal()? {
            return Self::require_matching_identity(&current, &desired);
        }
        let state = Self::serialize_state(
            &desired,
            StoredPhase::KeyGenerated {
                recorded_at_ms: recorded_at.get(),
            },
        )?;
        self.layout.write_initial(private_seed.expose(), &state)
    }

    async fn mark_claim_pending(
        &self,
        identity: &ConfiguredGatewayIdentity,
        idempotency_key: &EnrollmentIdempotencyKey,
        recorded_at: TimestampMs,
    ) -> Result<(), GatewayIdentityError> {
        Self::ensure_input_key_matches(identity, idempotency_key)?;
        let _transition = self.transition_guard()?;
        let _file_lock = self.write_guard()?;
        let current = self
            .load_internal()?
            .ok_or(GatewayIdentityError::InvalidTransition)?;
        Self::require_matching_identity(&current, identity)?;

        match current.phase() {
            GatewayEnrollmentPhase::KeyGenerated => {},
            GatewayEnrollmentPhase::ClaimPending(pending) => {
                return if pending.idempotency_key() == idempotency_key {
                    Ok(())
                } else {
                    Err(GatewayIdentityError::Conflict)
                };
            },
            GatewayEnrollmentPhase::Claimed(claimed) => {
                return if claimed.idempotency_key() == idempotency_key {
                    Ok(())
                } else {
                    Err(GatewayIdentityError::Conflict)
                };
            },
        }

        let state = Self::serialize_state(
            &current,
            StoredPhase::ClaimPending {
                idempotency_key: idempotency_key.as_str().to_string(),
                recorded_at_ms: recorded_at.get(),
            },
        )?;
        self.layout.replace_state(&state)
    }

    async fn mark_claimed(
        &self,
        identity: &ConfiguredGatewayIdentity,
        idempotency_key: &EnrollmentIdempotencyKey,
        revision: u64,
        recorded_at: TimestampMs,
    ) -> Result<(), GatewayIdentityError> {
        Self::ensure_input_key_matches(identity, idempotency_key)?;
        ConfiguredGatewayIdentity::claimed(
            identity.target().clone(),
            *identity.public_key(),
            idempotency_key.clone(),
            revision,
        )?;
        let _transition = self.transition_guard()?;
        let _file_lock = self.write_guard()?;
        let current = self
            .load_internal()?
            .ok_or(GatewayIdentityError::InvalidTransition)?;
        Self::require_matching_identity(&current, identity)?;

        match current.phase() {
            GatewayEnrollmentPhase::KeyGenerated => {
                return Err(GatewayIdentityError::InvalidTransition);
            },
            GatewayEnrollmentPhase::ClaimPending(pending) => {
                if pending.idempotency_key() != idempotency_key {
                    return Err(GatewayIdentityError::Conflict);
                }
            },
            GatewayEnrollmentPhase::Claimed(claimed) => {
                return if claimed.idempotency_key() == idempotency_key
                    && claimed.revision() == revision
                {
                    Ok(())
                } else {
                    Err(GatewayIdentityError::Conflict)
                };
            },
        }

        let state = Self::serialize_state(
            &current,
            StoredPhase::Claimed {
                idempotency_key: idempotency_key.as_str().to_string(),
                revision,
                recorded_at_ms: recorded_at.get(),
            },
        )?;
        self.layout.replace_state(&state)
    }
}

#[async_trait]
impl ClaimedGatewayIdentitySource for FileClaimedGatewayIdentitySource {
    async fn load_claimed_identity(
        &self,
    ) -> Result<Option<ClaimedGatewayIdentity>, GatewayIdentityError> {
        let Some(loaded) = self
            .layout
            .read_secret_identity()?
            .map(decode_stored_identity)
            .transpose()?
        else {
            return Ok(None);
        };
        let (identity, private_seed) = loaded;
        let GatewayEnrollmentPhase::Claimed(claimed) = identity.phase() else {
            return Ok(None);
        };
        let claimed_identity = ClaimedGatewayIdentity::new(
            identity.target().clone(),
            GatewayPrivateKeySeed::from_zeroizing(private_seed),
            *identity.public_key(),
            claimed.idempotency_key().clone(),
            claimed.revision(),
        )?;
        Ok(Some(claimed_identity))
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredState {
    schema: String,
    cloud_origin: String,
    tenant_id: String,
    project_id: String,
    gateway_id: String,
    public_key: String,
    fingerprint: String,
    phase: StoredPhase,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
enum StoredPhase {
    KeyGenerated {
        #[serde(rename = "recordedAtMs")]
        recorded_at_ms: u64,
    },
    ClaimPending {
        #[serde(rename = "idempotencyKey")]
        idempotency_key: String,
        #[serde(rename = "recordedAtMs")]
        recorded_at_ms: u64,
    },
    Claimed {
        #[serde(rename = "idempotencyKey")]
        idempotency_key: String,
        revision: u64,
        #[serde(rename = "recordedAtMs")]
        recorded_at_ms: u64,
    },
}

fn decode_stored_identity(
    files: StoredIdentityFiles,
) -> Result<(ConfiguredGatewayIdentity, Zeroizing<[u8; 32]>), GatewayIdentityError> {
    let identity = decode_stored_state(&files.state)?;
    let derived_public = SigningKey::from_bytes(&files.seed)
        .verifying_key()
        .to_bytes();
    if derived_public != *identity.public_key().as_bytes() {
        return Err(GatewayIdentityError::CorruptState);
    }
    Ok((identity, files.seed))
}

fn decode_stored_state(state: &[u8]) -> Result<ConfiguredGatewayIdentity, GatewayIdentityError> {
    let document: StoredState =
        serde_json::from_slice(state).map_err(|_| GatewayIdentityError::CorruptState)?;
    if document.schema != STATE_SCHEMA {
        return Err(GatewayIdentityError::CorruptState);
    }
    let endpoint_policy = if document.cloud_origin.starts_with("http://") {
        CloudEndpointPolicy::AllowLoopbackHttp
    } else {
        CloudEndpointPolicy::Production
    };
    let target = GatewayEnrollmentTarget::new(
        &document.cloud_origin,
        &document.tenant_id,
        &document.project_id,
        &document.gateway_id,
        endpoint_policy,
    )
    .map_err(|_| GatewayIdentityError::CorruptState)?;
    if target.cloud_origin() != document.cloud_origin {
        return Err(GatewayIdentityError::CorruptState);
    }
    let public_key = decode_public_key(&document.public_key)?;
    if public_key.fingerprint().as_str() != document.fingerprint {
        return Err(GatewayIdentityError::CorruptState);
    }
    let identity = match document.phase {
        StoredPhase::KeyGenerated { recorded_at_ms } => {
            let _ = recorded_at_ms;
            ConfiguredGatewayIdentity::key_generated(target, public_key)
        },
        StoredPhase::ClaimPending {
            idempotency_key,
            recorded_at_ms,
        } => {
            let _ = recorded_at_ms;
            ConfiguredGatewayIdentity::claim_pending(
                target,
                public_key,
                decode_idempotency_key(&idempotency_key)?,
            )
            .map_err(|_| GatewayIdentityError::CorruptState)?
        },
        StoredPhase::Claimed {
            idempotency_key,
            revision,
            recorded_at_ms,
        } => {
            let _ = recorded_at_ms;
            ConfiguredGatewayIdentity::claimed(
                target,
                public_key,
                decode_idempotency_key(&idempotency_key)?,
                revision,
            )
            .map_err(|_| GatewayIdentityError::CorruptState)?
        },
    };
    Ok(identity)
}

fn decode_public_key(value: &str) -> Result<GatewayPublicKey, GatewayIdentityError> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| GatewayIdentityError::CorruptState)?;
    let raw: [u8; 32] = bytes
        .try_into()
        .map_err(|_| GatewayIdentityError::CorruptState)?;
    if base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw) != value {
        return Err(GatewayIdentityError::CorruptState);
    }
    Ok(GatewayPublicKey::from_bytes(raw))
}

fn decode_idempotency_key(value: &str) -> Result<EnrollmentIdempotencyKey, GatewayIdentityError> {
    let uuid = Uuid::parse_str(value).map_err(|_| GatewayIdentityError::CorruptState)?;
    if uuid.to_string() != value {
        return Err(GatewayIdentityError::CorruptState);
    }
    EnrollmentIdempotencyKey::new(uuid).map_err(|_| GatewayIdentityError::CorruptState)
}
