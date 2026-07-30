//! Transport- and storage-neutral gateway enrollment capabilities.

use std::error::Error;
use std::fmt;

use aether_domain::TimestampMs;
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use url::{Origin, Url};
use uuid::Uuid;
use zeroize::{Zeroize as _, Zeroizing};

use crate::SecretMaterial;

/// Largest integer that every JSON implementation can represent exactly.
pub const MAX_CLOUD_ENROLLMENT_REVISION: u64 = 9_007_199_254_740_991;

const ENROLLMENT_IDEMPOTENCY_NAMESPACE: Uuid =
    Uuid::from_u128(0x2e01_d20b_3147_5a82_9599_23e2_0a9d_c172);

/// Network policy applied while validating a Cloud enrollment origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudEndpointPolicy {
    /// Require HTTPS for every Cloud origin.
    Production,
    /// Additionally allow HTTP on exactly `localhost` or `127.0.0.1`.
    AllowLoopbackHttp,
}

/// A normalized Cloud origin and canonical gateway enrollment scope.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GatewayEnrollmentTarget {
    cloud_origin: String,
    tenant_id: Uuid,
    project_id: Uuid,
    gateway_id: Uuid,
}

impl GatewayEnrollmentTarget {
    /// Validates and normalizes a Cloud origin and three non-nil canonical UUIDs.
    pub fn new(
        cloud_origin: &str,
        tenant_id: &str,
        project_id: &str,
        gateway_id: &str,
        endpoint_policy: CloudEndpointPolicy,
    ) -> Result<Self, GatewayEnrollmentTargetError> {
        let cloud_origin = normalize_cloud_origin(cloud_origin, endpoint_policy)?;
        let tenant_id = canonical_non_nil_uuid(tenant_id)
            .ok_or(GatewayEnrollmentTargetError::InvalidTenantId)?;
        let project_id = canonical_non_nil_uuid(project_id)
            .ok_or(GatewayEnrollmentTargetError::InvalidProjectId)?;
        let gateway_id = canonical_non_nil_uuid(gateway_id)
            .ok_or(GatewayEnrollmentTargetError::InvalidGatewayId)?;

        Ok(Self {
            cloud_origin,
            tenant_id,
            project_id,
            gateway_id,
        })
    }

    /// Returns the normalized origin without a trailing slash.
    #[must_use]
    pub fn cloud_origin(&self) -> &str {
        &self.cloud_origin
    }

    /// Returns the commissioned tenant identifier.
    #[must_use]
    pub const fn tenant_id(&self) -> Uuid {
        self.tenant_id
    }

    /// Returns the commissioned project identifier.
    #[must_use]
    pub const fn project_id(&self) -> Uuid {
        self.project_id
    }

    /// Returns the commissioned gateway identifier.
    #[must_use]
    pub const fn gateway_id(&self) -> Uuid {
        self.gateway_id
    }
}

fn normalize_cloud_origin(
    input: &str,
    endpoint_policy: CloudEndpointPolicy,
) -> Result<String, GatewayEnrollmentTargetError> {
    let parsed = Url::parse(input).map_err(|_| GatewayEnrollmentTargetError::InvalidCloudOrigin)?;
    if parsed.cannot_be_a_base()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(GatewayEnrollmentTargetError::InvalidCloudOrigin);
    }

    parsed
        .host_str()
        .ok_or(GatewayEnrollmentTargetError::InvalidCloudOrigin)?;
    match parsed.scheme() {
        "https" => {},
        "http"
            if endpoint_policy == CloudEndpointPolicy::AllowLoopbackHttp
                && has_exact_loopback_http_authority(input) => {},
        "http" => return Err(GatewayEnrollmentTargetError::InsecureCloudOrigin),
        _ => return Err(GatewayEnrollmentTargetError::InvalidCloudOrigin),
    }

    match parsed.origin() {
        Origin::Tuple(..) => Ok(parsed.origin().ascii_serialization()),
        Origin::Opaque(_) => Err(GatewayEnrollmentTargetError::InvalidCloudOrigin),
    }
}

fn has_exact_loopback_http_authority(input: &str) -> bool {
    let Some(authority) = input.strip_prefix("http://") else {
        return false;
    };
    let authority = authority.strip_suffix('/').unwrap_or(authority);
    ["localhost", "127.0.0.1"].into_iter().any(|host| {
        authority == host
            || authority
                .strip_prefix(host)
                .and_then(|suffix| suffix.strip_prefix(':'))
                .is_some_and(|port| {
                    !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit())
                })
    })
}

fn canonical_non_nil_uuid(input: &str) -> Option<Uuid> {
    let parsed = Uuid::parse_str(input).ok()?;
    (parsed != Uuid::nil() && parsed.to_string() == input).then_some(parsed)
}

/// Failure while constructing a strict enrollment target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayEnrollmentTargetError {
    /// The value is not a bare HTTP(S) origin.
    InvalidCloudOrigin,
    /// The origin uses HTTP outside the explicit loopback development policy.
    InsecureCloudOrigin,
    /// The tenant identifier is not a canonical non-nil UUID.
    InvalidTenantId,
    /// The project identifier is not a canonical non-nil UUID.
    InvalidProjectId,
    /// The gateway identifier is not a canonical non-nil UUID.
    InvalidGatewayId,
}

impl fmt::Display for GatewayEnrollmentTargetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidCloudOrigin => "cloud URL must be a bare HTTPS origin",
            Self::InsecureCloudOrigin => "cloud URL does not satisfy the transport policy",
            Self::InvalidTenantId => "tenant ID must be a canonical non-nil UUID",
            Self::InvalidProjectId => "project ID must be a canonical non-nil UUID",
            Self::InvalidGatewayId => "gateway ID must be a canonical non-nil UUID",
        })
    }
}

impl Error for GatewayEnrollmentTargetError {}

/// An Ed25519 private seed held in zeroizing memory.
///
/// This type deliberately does not implement `Clone`, `Display`, or
/// serialization. Debug output is always redacted.
pub struct GatewayPrivateKeySeed(Zeroizing<[u8; 32]>);

impl GatewayPrivateKeySeed {
    /// Wraps 32 bytes of private seed material and clears the input copy.
    #[must_use]
    pub fn from_bytes(mut seed: [u8; 32]) -> Self {
        let protected = Zeroizing::new(seed);
        seed.zeroize();
        Self::from_zeroizing(protected)
    }

    /// Takes ownership of seed material that is already protected by zeroize.
    ///
    /// Adapter implementations should prefer this constructor so a protected
    /// buffer is not copied back into an ordinary stack array.
    #[must_use]
    pub fn from_zeroizing(seed: Zeroizing<[u8; 32]>) -> Self {
        Self(seed)
    }

    /// Exposes the seed only to the final persistence or signing boundary.
    #[must_use]
    pub fn expose(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for GatewayPrivateKeySeed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("GatewayPrivateKeySeed([REDACTED])")
    }
}

/// Raw 32-byte Ed25519 public key.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct GatewayPublicKey([u8; 32]);

impl GatewayPublicKey {
    /// Creates a public key from its raw Ed25519 representation.
    #[must_use]
    pub const fn from_bytes(public_key: [u8; 32]) -> Self {
        Self(public_key)
    }

    /// Returns the raw 32-byte Ed25519 public key.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Calculates the canonical SHA-256 fingerprint.
    #[must_use]
    pub fn fingerprint(&self) -> GatewayPublicKeyFingerprint {
        let digest = Sha256::digest(self.0);
        let mut encoded = String::with_capacity(64);
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for byte in digest {
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        GatewayPublicKeyFingerprint(encoded)
    }
}

impl fmt::Debug for GatewayPublicKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("GatewayPublicKey([REDACTED])")
    }
}

/// Lowercase hexadecimal SHA-256 of a raw Ed25519 public key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GatewayPublicKeyFingerprint(String);

impl GatewayPublicKeyFingerprint {
    /// Returns the fixed 64-character lowercase hexadecimal representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Freshly generated Ed25519 identity key material.
///
/// This type cannot be cloned and its Debug output never reveals key bytes.
pub struct GeneratedGatewayIdentityKey {
    private_seed: GatewayPrivateKeySeed,
    public_key: GatewayPublicKey,
}

impl GeneratedGatewayIdentityKey {
    /// Combines a generated private seed with its corresponding public key.
    #[must_use]
    pub fn new(private_seed: GatewayPrivateKeySeed, public_key: GatewayPublicKey) -> Self {
        Self {
            private_seed,
            public_key,
        }
    }

    /// Returns the public portion without exposing the private seed.
    #[must_use]
    pub const fn public_key(&self) -> &GatewayPublicKey {
        &self.public_key
    }

    /// Consumes the generated key at the persistence boundary.
    #[must_use]
    pub fn into_parts(self) -> (GatewayPrivateKeySeed, GatewayPublicKey) {
        (self.private_seed, self.public_key)
    }
}

impl fmt::Debug for GeneratedGatewayIdentityKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("GeneratedGatewayIdentityKey([REDACTED])")
    }
}

/// Stable UUID used to make a Claim request idempotent.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EnrollmentIdempotencyKey {
    value: Uuid,
    canonical: String,
}

impl EnrollmentIdempotencyKey {
    /// Creates an idempotency key from a non-nil UUID.
    pub fn new(value: Uuid) -> Result<Self, EnrollmentIdempotencyKeyError> {
        if value == Uuid::nil() {
            return Err(EnrollmentIdempotencyKeyError);
        }
        Ok(Self {
            value,
            canonical: value.to_string(),
        })
    }

    /// Returns the canonical UUID string used in the HTTP header.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.canonical
    }

    /// Returns the parsed UUID.
    #[must_use]
    pub const fn as_uuid(&self) -> Uuid {
        self.value
    }
}

/// A nil UUID cannot be used as an enrollment idempotency key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnrollmentIdempotencyKeyError;

impl fmt::Display for EnrollmentIdempotencyKeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("enrollment idempotency key must be a non-nil UUID")
    }
}

impl Error for EnrollmentIdempotencyKeyError {}

/// Claim-pending phase data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimPendingGatewayIdentity {
    idempotency_key: EnrollmentIdempotencyKey,
}

impl ClaimPendingGatewayIdentity {
    /// Creates a pending phase with the stable Claim key.
    #[must_use]
    pub fn new(idempotency_key: EnrollmentIdempotencyKey) -> Self {
        Self { idempotency_key }
    }

    /// Returns the stable Claim idempotency key.
    #[must_use]
    pub const fn idempotency_key(&self) -> &EnrollmentIdempotencyKey {
        &self.idempotency_key
    }
}

/// Successfully claimed phase data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedGatewayIdentityState {
    idempotency_key: EnrollmentIdempotencyKey,
    revision: u64,
}

impl ClaimedGatewayIdentityState {
    /// Creates a claimed phase after validating the Cloud revision.
    pub fn new(
        idempotency_key: EnrollmentIdempotencyKey,
        revision: u64,
    ) -> Result<Self, GatewayIdentityError> {
        if !(1..=MAX_CLOUD_ENROLLMENT_REVISION).contains(&revision) {
            return Err(GatewayIdentityError::InvalidState);
        }
        Ok(Self {
            idempotency_key,
            revision,
        })
    }

    /// Returns the stable Claim idempotency key.
    #[must_use]
    pub const fn idempotency_key(&self) -> &EnrollmentIdempotencyKey {
        &self.idempotency_key
    }

    /// Returns the positive, JSON-safe Cloud revision.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }
}

/// Persisted enrollment phase with no contradictory optional-field combinations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayEnrollmentPhase {
    /// A key exists durably but no Claim has been prepared.
    KeyGenerated,
    /// The Claim key exists durably and may be retried.
    ClaimPending(ClaimPendingGatewayIdentity),
    /// Cloud acknowledged the public-key binding.
    Claimed(ClaimedGatewayIdentityState),
}

/// Public, non-secret metadata for one configured gateway identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfiguredGatewayIdentity {
    target: GatewayEnrollmentTarget,
    public_key: GatewayPublicKey,
    fingerprint: GatewayPublicKeyFingerprint,
    phase: GatewayEnrollmentPhase,
}

impl ConfiguredGatewayIdentity {
    /// Creates metadata for a durably generated key.
    #[must_use]
    pub fn key_generated(target: GatewayEnrollmentTarget, public_key: GatewayPublicKey) -> Self {
        Self::with_phase(target, public_key, GatewayEnrollmentPhase::KeyGenerated)
    }

    /// Creates metadata for a durably pending Claim.
    #[must_use = "a pending identity must satisfy the stable idempotency-key invariant"]
    pub fn claim_pending(
        target: GatewayEnrollmentTarget,
        public_key: GatewayPublicKey,
        idempotency_key: EnrollmentIdempotencyKey,
    ) -> Result<Self, GatewayIdentityError> {
        let identity = Self::with_phase(
            target,
            public_key,
            GatewayEnrollmentPhase::ClaimPending(ClaimPendingGatewayIdentity::new(idempotency_key)),
        );
        identity.validated_idempotency_key()?;
        Ok(identity)
    }

    /// Creates metadata for a successfully claimed identity.
    pub fn claimed(
        target: GatewayEnrollmentTarget,
        public_key: GatewayPublicKey,
        idempotency_key: EnrollmentIdempotencyKey,
        revision: u64,
    ) -> Result<Self, GatewayIdentityError> {
        let phase = GatewayEnrollmentPhase::Claimed(ClaimedGatewayIdentityState::new(
            idempotency_key,
            revision,
        )?);
        let identity = Self::with_phase(target, public_key, phase);
        identity.validated_idempotency_key()?;
        Ok(identity)
    }

    fn with_phase(
        target: GatewayEnrollmentTarget,
        public_key: GatewayPublicKey,
        phase: GatewayEnrollmentPhase,
    ) -> Self {
        let fingerprint = public_key.fingerprint();
        Self {
            target,
            public_key,
            fingerprint,
            phase,
        }
    }

    /// Returns the immutable enrollment scope.
    #[must_use]
    pub const fn target(&self) -> &GatewayEnrollmentTarget {
        &self.target
    }

    /// Returns the public key.
    #[must_use]
    pub const fn public_key(&self) -> &GatewayPublicKey {
        &self.public_key
    }

    /// Returns the canonical public-key fingerprint.
    #[must_use]
    pub const fn fingerprint(&self) -> &GatewayPublicKeyFingerprint {
        &self.fingerprint
    }

    /// Returns the persisted enrollment phase.
    #[must_use]
    pub const fn phase(&self) -> &GatewayEnrollmentPhase {
        &self.phase
    }

    /// Returns the Claim key for pending or claimed state.
    #[must_use]
    pub const fn idempotency_key(&self) -> Option<&EnrollmentIdempotencyKey> {
        match &self.phase {
            GatewayEnrollmentPhase::KeyGenerated => None,
            GatewayEnrollmentPhase::ClaimPending(pending) => Some(pending.idempotency_key()),
            GatewayEnrollmentPhase::Claimed(claimed) => Some(claimed.idempotency_key()),
        }
    }

    /// Derives the one stable Claim key for this immutable scope and public key.
    #[must_use]
    pub fn stable_idempotency_key(&self) -> EnrollmentIdempotencyKey {
        let target = self.target();
        let canonical_name = format!(
            "aether.cloud.gateway-enrollment-claim.v1\n{}\n{}\n{}\n{}\n{}",
            target.cloud_origin(),
            target.tenant_id(),
            target.project_id(),
            target.gateway_id(),
            self.fingerprint().as_str(),
        );
        let value = Uuid::new_v5(&ENROLLMENT_IDEMPOTENCY_NAMESPACE, canonical_name.as_bytes());
        EnrollmentIdempotencyKey {
            value,
            canonical: value.to_string(),
        }
    }

    /// Returns the stable key after checking any persisted phase key.
    pub fn validated_idempotency_key(
        &self,
    ) -> Result<EnrollmentIdempotencyKey, GatewayIdentityError> {
        let expected = self.stable_idempotency_key();
        if self
            .idempotency_key()
            .is_some_and(|stored| stored != &expected)
        {
            Err(GatewayIdentityError::InvalidState)
        } else {
            Ok(expected)
        }
    }

    /// Returns the Cloud revision only after a successful Claim.
    #[must_use]
    pub const fn claimed_revision(&self) -> Option<u64> {
        match &self.phase {
            GatewayEnrollmentPhase::Claimed(claimed) => Some(claimed.revision()),
            GatewayEnrollmentPhase::KeyGenerated | GatewayEnrollmentPhase::ClaimPending(_) => None,
        }
    }
}

/// Locally persisted enrollment state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayEnrollmentStatus {
    /// No key or enrollment scope exists locally.
    Unconfigured,
    /// A single immutable gateway identity exists locally.
    Configured(ConfiguredGatewayIdentity),
}

/// Private key and scope handed to a durable identity store.
pub struct GatewayIdentityInitialization {
    target: GatewayEnrollmentTarget,
    generated_key: GeneratedGatewayIdentityKey,
}

impl GatewayIdentityInitialization {
    /// Creates the first persisted identity state.
    #[must_use]
    pub fn new(
        target: GatewayEnrollmentTarget,
        generated_key: GeneratedGatewayIdentityKey,
    ) -> Self {
        Self {
            target,
            generated_key,
        }
    }

    /// Returns the immutable enrollment scope.
    #[must_use]
    pub const fn target(&self) -> &GatewayEnrollmentTarget {
        &self.target
    }

    /// Returns the public key without exposing the private seed.
    #[must_use]
    pub const fn public_key(&self) -> &GatewayPublicKey {
        self.generated_key.public_key()
    }

    /// Consumes the initialization at the persistence boundary.
    #[must_use]
    pub fn into_parts(self) -> (GatewayEnrollmentTarget, GeneratedGatewayIdentityKey) {
        (self.target, self.generated_key)
    }
}

impl fmt::Debug for GatewayIdentityInitialization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("GatewayIdentityInitialization([REDACTED])")
    }
}

/// A complete claimed identity for the future `aether-uplink` signing owner.
pub struct ClaimedGatewayIdentity {
    target: GatewayEnrollmentTarget,
    private_seed: GatewayPrivateKeySeed,
    public_key: GatewayPublicKey,
    fingerprint: GatewayPublicKeyFingerprint,
    idempotency_key: EnrollmentIdempotencyKey,
    revision: u64,
}

impl ClaimedGatewayIdentity {
    /// Constructs a complete claimed identity from durable material.
    pub fn new(
        target: GatewayEnrollmentTarget,
        private_seed: GatewayPrivateKeySeed,
        public_key: GatewayPublicKey,
        idempotency_key: EnrollmentIdempotencyKey,
        revision: u64,
    ) -> Result<Self, GatewayIdentityError> {
        ConfiguredGatewayIdentity::claimed(
            target.clone(),
            public_key,
            idempotency_key.clone(),
            revision,
        )?;
        Ok(Self {
            target,
            private_seed,
            public_key,
            fingerprint: public_key.fingerprint(),
            idempotency_key,
            revision,
        })
    }

    /// Returns the immutable enrollment scope.
    #[must_use]
    pub const fn target(&self) -> &GatewayEnrollmentTarget {
        &self.target
    }

    /// Exposes the private seed only to the `aether-uplink` signing boundary.
    #[must_use]
    pub const fn private_seed(&self) -> &GatewayPrivateKeySeed {
        &self.private_seed
    }

    /// Returns the public key.
    #[must_use]
    pub const fn public_key(&self) -> &GatewayPublicKey {
        &self.public_key
    }

    /// Returns the canonical public-key fingerprint.
    #[must_use]
    pub const fn fingerprint(&self) -> &GatewayPublicKeyFingerprint {
        &self.fingerprint
    }

    /// Returns the stable Claim idempotency key.
    #[must_use]
    pub const fn idempotency_key(&self) -> &EnrollmentIdempotencyKey {
        &self.idempotency_key
    }

    /// Returns the positive, JSON-safe Cloud revision.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }
}

impl fmt::Debug for ClaimedGatewayIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ClaimedGatewayIdentity([REDACTED])")
    }
}

/// One transport-neutral request to bind a public key in AetherCloud.
pub struct CloudEnrollmentClaim {
    target: GatewayEnrollmentTarget,
    public_key: GatewayPublicKey,
    fingerprint: GatewayPublicKeyFingerprint,
    idempotency_key: EnrollmentIdempotencyKey,
    enrollment_token: SecretMaterial,
}

impl CloudEnrollmentClaim {
    /// Creates a Claim request, deriving the fingerprint from the raw public key.
    #[must_use]
    pub fn new(
        target: GatewayEnrollmentTarget,
        public_key: GatewayPublicKey,
        idempotency_key: EnrollmentIdempotencyKey,
        enrollment_token: SecretMaterial,
    ) -> Self {
        Self {
            target,
            public_key,
            fingerprint: public_key.fingerprint(),
            idempotency_key,
            enrollment_token,
        }
    }

    /// Returns the immutable enrollment scope.
    #[must_use]
    pub const fn target(&self) -> &GatewayEnrollmentTarget {
        &self.target
    }

    /// Returns the raw Ed25519 public key.
    #[must_use]
    pub const fn public_key(&self) -> &GatewayPublicKey {
        &self.public_key
    }

    /// Returns the canonical public-key fingerprint.
    #[must_use]
    pub const fn fingerprint(&self) -> &GatewayPublicKeyFingerprint {
        &self.fingerprint
    }

    /// Returns the stable idempotency key.
    #[must_use]
    pub const fn idempotency_key(&self) -> &EnrollmentIdempotencyKey {
        &self.idempotency_key
    }

    /// Exposes the token only to the concrete Cloud adapter.
    #[must_use]
    pub const fn enrollment_token(&self) -> &SecretMaterial {
        &self.enrollment_token
    }
}

impl fmt::Debug for CloudEnrollmentClaim {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CloudEnrollmentClaim([REDACTED])")
    }
}

/// Minimal Cloud acknowledgement returned by the Claim adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudEnrollmentReceipt {
    gateway_id: String,
    revision: u64,
}

impl CloudEnrollmentReceipt {
    /// Creates a receipt from strictly decoded adapter data.
    #[must_use]
    pub fn new(gateway_id: impl Into<String>, revision: u64) -> Self {
        Self {
            gateway_id: gateway_id.into(),
            revision,
        }
    }

    /// Returns the gateway identifier exactly as received.
    #[must_use]
    pub fn gateway_id(&self) -> &str {
        &self.gateway_id
    }

    /// Returns the revision exactly as received.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }
}

/// Sanitized Cloud Claim failure category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudEnrollmentClientError {
    /// Adapter configuration violates the endpoint or timeout policy.
    InvalidConfiguration,
    /// Cloud rejected authentication or authorization.
    Rejected,
    /// Cloud reported an identity or idempotency conflict.
    Conflict,
    /// The response failed strict contract validation.
    InvalidResponse,
    /// The bounded operation exceeded its deadline.
    Timeout,
    /// Cloud or the network is temporarily unavailable.
    Unavailable,
}

impl CloudEnrollmentClientError {
    /// Returns whether retry with the same key and idempotency key is meaningful.
    #[must_use]
    pub const fn is_retryable(self) -> bool {
        matches!(self, Self::Timeout | Self::Unavailable)
    }
}

impl fmt::Display for CloudEnrollmentClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidConfiguration => "cloud enrollment client configuration is invalid",
            Self::Rejected => "cloud enrollment was rejected",
            Self::Conflict => "cloud enrollment conflicts with existing identity",
            Self::InvalidResponse => "cloud enrollment response is invalid",
            Self::Timeout => "cloud enrollment request timed out",
            Self::Unavailable => "cloud enrollment service is unavailable",
        })
    }
}

impl Error for CloudEnrollmentClientError {}

/// Sanitized identity key or durable-store failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayIdentityError {
    /// Secure Ed25519 key generation failed.
    GenerationFailed,
    /// Durable identity storage is temporarily unavailable.
    Unavailable,
    /// Storage permissions, paths, or file types are unsafe.
    InsecureStorage,
    /// Persisted bytes violate the identity state contract.
    CorruptState,
    /// A different identity already owns the store.
    Conflict,
    /// The requested durable transition is not valid from current state.
    InvalidTransition,
    /// Identity data violates a phase invariant.
    InvalidState,
}

impl GatewayIdentityError {
    /// Returns whether retry without changing identity data can succeed.
    #[must_use]
    pub const fn is_retryable(self) -> bool {
        matches!(self, Self::Unavailable)
    }
}

impl fmt::Display for GatewayIdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::GenerationFailed => "gateway identity key generation failed",
            Self::Unavailable => "gateway identity store is unavailable",
            Self::InsecureStorage => "gateway identity storage is unsafe",
            Self::CorruptState => "gateway identity state is invalid",
            Self::Conflict => "gateway identity conflicts with existing state",
            Self::InvalidTransition => "gateway identity transition is invalid",
            Self::InvalidState => "gateway identity phase is invalid",
        })
    }
}

impl Error for GatewayIdentityError {}

/// Calls the concrete AetherCloud Claim transport.
#[async_trait]
pub trait CloudEnrollmentClient: Send + Sync + 'static {
    /// Sends one bounded Claim attempt.
    async fn claim(
        &self,
        claim: CloudEnrollmentClaim,
    ) -> Result<CloudEnrollmentReceipt, CloudEnrollmentClientError>;
}

/// Generates a fresh Ed25519 identity using an operating-system CSPRNG.
pub trait GatewayIdentityKeyGenerator: Send + Sync + 'static {
    /// Generates a matching private seed and public key.
    fn generate(&self) -> Result<GeneratedGatewayIdentityKey, GatewayIdentityError>;
}

/// Owns durable enrollment identity state transitions.
#[async_trait]
pub trait GatewayIdentityStore: Send + Sync + 'static {
    /// Loads non-secret enrollment status.
    async fn load(&self) -> Result<GatewayEnrollmentStatus, GatewayIdentityError>;

    /// Durably writes a fresh private key before any Cloud request.
    async fn persist_key_generated(
        &self,
        identity: GatewayIdentityInitialization,
        recorded_at: TimestampMs,
    ) -> Result<(), GatewayIdentityError>;

    /// Durably records the stable Claim key before any Cloud request.
    async fn mark_claim_pending(
        &self,
        identity: &ConfiguredGatewayIdentity,
        idempotency_key: &EnrollmentIdempotencyKey,
        recorded_at: TimestampMs,
    ) -> Result<(), GatewayIdentityError>;

    /// Atomically records a validated successful Claim.
    async fn mark_claimed(
        &self,
        identity: &ConfiguredGatewayIdentity,
        idempotency_key: &EnrollmentIdempotencyKey,
        revision: u64,
        recorded_at: TimestampMs,
    ) -> Result<(), GatewayIdentityError>;
}

/// Read-only seam reserved for the `aether-uplink` identity owner.
#[async_trait]
pub trait ClaimedGatewayIdentitySource: Send + Sync + 'static {
    /// Loads complete claimed identity material, if present.
    async fn load_claimed_identity(
        &self,
    ) -> Result<Option<ClaimedGatewayIdentity>, GatewayIdentityError>;
}
