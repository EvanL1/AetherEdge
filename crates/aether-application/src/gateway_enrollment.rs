//! One-shot gateway identity enrollment orchestration.

use std::sync::Arc;

use aether_ports::{
    Clock, CloudEnrollmentClaim, CloudEnrollmentClient, CloudEnrollmentClientError,
    ConfiguredGatewayIdentity, EnrollmentIdempotencyKey, GatewayEnrollmentPhase,
    GatewayEnrollmentStatus, GatewayEnrollmentTarget, GatewayIdentityError,
    GatewayIdentityInitialization, GatewayIdentityKeyGenerator, GatewayIdentityStore,
    MAX_CLOUD_ENROLLMENT_REVISION, PortError, SecretMaterial,
};
use thiserror::Error;
use uuid::Uuid;

/// Failure returned by one bounded gateway enrollment attempt.
#[derive(Debug, Error)]
pub enum GatewayEnrollmentError {
    /// Key generation or durable identity storage failed.
    #[error("gateway identity operation failed")]
    Identity(#[source] GatewayIdentityError),
    /// The configured identity belongs to a different immutable scope.
    #[error("a different gateway identity is already configured")]
    IdentityConflict,
    /// The trusted wall clock could not provide a persistence timestamp.
    #[error("trusted enrollment clock is unavailable")]
    Clock(#[source] PortError),
    /// The Cloud Claim attempt failed with sanitized recovery semantics.
    #[error("{0}")]
    Cloud(#[source] CloudEnrollmentClientError),
    /// Cloud returned an acknowledgement that did not match the request.
    #[error("cloud enrollment receipt is invalid")]
    InvalidCloudReceipt,
    /// A port returned state that violates the application invariant.
    #[error("gateway identity state is invalid")]
    InvalidStoredIdentity,
}

/// Successful result from one enrollment invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayEnrollmentResult {
    identity: ConfiguredGatewayIdentity,
    was_already_claimed: bool,
}

impl GatewayEnrollmentResult {
    fn newly_claimed(identity: ConfiguredGatewayIdentity) -> Self {
        Self {
            identity,
            was_already_claimed: false,
        }
    }

    fn already_claimed(identity: ConfiguredGatewayIdentity) -> Self {
        Self {
            identity,
            was_already_claimed: true,
        }
    }

    /// Returns the non-secret claimed identity metadata.
    #[must_use]
    pub const fn identity(&self) -> &ConfiguredGatewayIdentity {
        &self.identity
    }

    /// Returns whether no Cloud request was required because this scope was already claimed.
    #[must_use]
    pub const fn was_already_claimed(&self) -> bool {
        self.was_already_claimed
    }
}

/// Generates, persists, and claims one immutable AetherCloud gateway identity.
///
/// This use case performs one bounded Cloud attempt and never retries internally.
/// A caller retries with a new token; the durable pending identity preserves the
/// same private key and idempotency key.
pub struct EnrollGatewayWithAetherCloud {
    client: Arc<dyn CloudEnrollmentClient>,
    key_generator: Arc<dyn GatewayIdentityKeyGenerator>,
    identity_store: Arc<dyn GatewayIdentityStore>,
    clock: Arc<dyn Clock>,
}

impl EnrollGatewayWithAetherCloud {
    /// Creates the transport- and storage-neutral enrollment use case.
    #[must_use]
    pub fn new(
        client: Arc<dyn CloudEnrollmentClient>,
        key_generator: Arc<dyn GatewayIdentityKeyGenerator>,
        identity_store: Arc<dyn GatewayIdentityStore>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            client,
            key_generator,
            identity_store,
            clock,
        }
    }

    /// Reads local enrollment state without contacting AetherCloud.
    pub async fn status(&self) -> Result<GatewayEnrollmentStatus, GatewayEnrollmentError> {
        let status = self
            .identity_store
            .load()
            .await
            .map_err(GatewayEnrollmentError::Identity)?;
        if let GatewayEnrollmentStatus::Configured(identity) = &status {
            validated_idempotency_key(identity)?;
        }
        Ok(status)
    }

    /// Performs one bounded Claim attempt after all pending state is durable.
    pub async fn enroll(
        &self,
        target: GatewayEnrollmentTarget,
        enrollment_token: SecretMaterial,
    ) -> Result<GatewayEnrollmentResult, GatewayEnrollmentError> {
        let status = self
            .identity_store
            .load()
            .await
            .map_err(GatewayEnrollmentError::Identity)?;
        let identity = match status {
            GatewayEnrollmentStatus::Unconfigured => {
                self.generate_and_persist_identity(target.clone()).await?
            },
            GatewayEnrollmentStatus::Configured(identity) => {
                if identity.target() != &target {
                    return Err(GatewayEnrollmentError::IdentityConflict);
                }
                identity
            },
        };
        let stable_idempotency_key = validated_idempotency_key(&identity)?;

        if matches!(identity.phase(), GatewayEnrollmentPhase::Claimed(_)) {
            return Ok(GatewayEnrollmentResult::already_claimed(identity));
        }

        let (pending_identity, idempotency_key) = match identity.phase() {
            GatewayEnrollmentPhase::KeyGenerated => {
                let idempotency_key = stable_idempotency_key;
                let recorded_at = self.clock.now().map_err(GatewayEnrollmentError::Clock)?;
                self.identity_store
                    .mark_claim_pending(&identity, &idempotency_key, recorded_at)
                    .await
                    .map_err(GatewayEnrollmentError::Identity)?;
                (
                    ConfiguredGatewayIdentity::claim_pending(
                        identity.target().clone(),
                        *identity.public_key(),
                        idempotency_key.clone(),
                    )
                    .map_err(|_| GatewayEnrollmentError::InvalidStoredIdentity)?,
                    idempotency_key,
                )
            },
            GatewayEnrollmentPhase::ClaimPending(_) => (identity.clone(), stable_idempotency_key),
            GatewayEnrollmentPhase::Claimed(_) => {
                return Err(GatewayEnrollmentError::InvalidStoredIdentity);
            },
        };

        let receipt = self
            .client
            .claim(CloudEnrollmentClaim::new(
                pending_identity.target().clone(),
                *pending_identity.public_key(),
                idempotency_key.clone(),
                enrollment_token,
            ))
            .await
            .map_err(GatewayEnrollmentError::Cloud)?;
        validate_receipt(&pending_identity, &receipt)?;

        let recorded_at = self.clock.now().map_err(GatewayEnrollmentError::Clock)?;
        self.identity_store
            .mark_claimed(
                &pending_identity,
                &idempotency_key,
                receipt.revision(),
                recorded_at,
            )
            .await
            .map_err(GatewayEnrollmentError::Identity)?;
        let claimed = ConfiguredGatewayIdentity::claimed(
            pending_identity.target().clone(),
            *pending_identity.public_key(),
            idempotency_key,
            receipt.revision(),
        )
        .map_err(|_| GatewayEnrollmentError::InvalidCloudReceipt)?;

        Ok(GatewayEnrollmentResult::newly_claimed(claimed))
    }

    async fn generate_and_persist_identity(
        &self,
        target: GatewayEnrollmentTarget,
    ) -> Result<ConfiguredGatewayIdentity, GatewayEnrollmentError> {
        let generated = self
            .key_generator
            .generate()
            .map_err(GatewayEnrollmentError::Identity)?;
        let public_key = *generated.public_key();
        let initialization = GatewayIdentityInitialization::new(target.clone(), generated);
        let recorded_at = self.clock.now().map_err(GatewayEnrollmentError::Clock)?;
        self.identity_store
            .persist_key_generated(initialization, recorded_at)
            .await
            .map_err(GatewayEnrollmentError::Identity)?;
        Ok(ConfiguredGatewayIdentity::key_generated(target, public_key))
    }
}

fn validated_idempotency_key(
    identity: &ConfiguredGatewayIdentity,
) -> Result<EnrollmentIdempotencyKey, GatewayEnrollmentError> {
    identity
        .validated_idempotency_key()
        .map_err(|_| GatewayEnrollmentError::InvalidStoredIdentity)
}

fn validate_receipt(
    identity: &ConfiguredGatewayIdentity,
    receipt: &aether_ports::CloudEnrollmentReceipt,
) -> Result<(), GatewayEnrollmentError> {
    let gateway_id = Uuid::parse_str(receipt.gateway_id())
        .ok()
        .filter(|parsed| *parsed != Uuid::nil())
        .filter(|parsed| parsed.to_string() == receipt.gateway_id())
        .filter(|parsed| *parsed == identity.target().gateway_id());
    if gateway_id.is_none() || !(1..=MAX_CLOUD_ENROLLMENT_REVISION).contains(&receipt.revision()) {
        return Err(GatewayEnrollmentError::InvalidCloudReceipt);
    }
    Ok(())
}
