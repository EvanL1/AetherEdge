use aether_domain::TimestampMs;
use aether_ports::{
    ClaimedGatewayIdentitySource, CloudEndpointPolicy, CloudEnrollmentClaim, CloudEnrollmentClient,
    CloudEnrollmentClientError, CloudEnrollmentReceipt, ConfiguredGatewayIdentity,
    EnrollmentIdempotencyKey, GatewayIdentityError, GatewayIdentityKeyGenerator,
    GatewayIdentityStore, GatewayPrivateKeySeed, GatewayPublicKey, SecretMaterial,
};
use async_trait::async_trait;
use uuid::Uuid;
use zeroize::Zeroizing;

fn assert_object_safe_client(_: &dyn CloudEnrollmentClient) {}
fn assert_object_safe_store(_: &dyn GatewayIdentityStore) {}
fn assert_object_safe_generator(_: &dyn GatewayIdentityKeyGenerator) {}
fn assert_object_safe_claimed_source(_: &dyn ClaimedGatewayIdentitySource) {}

#[test]
fn enrollment_ports_are_object_safe() {
    struct Ports;

    #[async_trait]
    impl CloudEnrollmentClient for Ports {
        async fn claim(
            &self,
            _claim: CloudEnrollmentClaim,
        ) -> Result<CloudEnrollmentReceipt, CloudEnrollmentClientError> {
            Err(CloudEnrollmentClientError::Unavailable)
        }
    }

    #[async_trait]
    impl GatewayIdentityStore for Ports {
        async fn load(
            &self,
        ) -> Result<aether_ports::GatewayEnrollmentStatus, GatewayIdentityError> {
            Ok(aether_ports::GatewayEnrollmentStatus::Unconfigured)
        }

        async fn persist_key_generated(
            &self,
            _identity: aether_ports::GatewayIdentityInitialization,
            _recorded_at: TimestampMs,
        ) -> Result<(), GatewayIdentityError> {
            Ok(())
        }

        async fn mark_claim_pending(
            &self,
            _identity: &ConfiguredGatewayIdentity,
            _idempotency_key: &EnrollmentIdempotencyKey,
            _recorded_at: TimestampMs,
        ) -> Result<(), GatewayIdentityError> {
            Ok(())
        }

        async fn mark_claimed(
            &self,
            _identity: &ConfiguredGatewayIdentity,
            _idempotency_key: &EnrollmentIdempotencyKey,
            _revision: u64,
            _recorded_at: TimestampMs,
        ) -> Result<(), GatewayIdentityError> {
            Ok(())
        }
    }

    impl GatewayIdentityKeyGenerator for Ports {
        fn generate(
            &self,
        ) -> Result<aether_ports::GeneratedGatewayIdentityKey, GatewayIdentityError> {
            Err(GatewayIdentityError::GenerationFailed)
        }
    }

    #[async_trait]
    impl ClaimedGatewayIdentitySource for Ports {
        async fn load_claimed_identity(
            &self,
        ) -> Result<Option<aether_ports::ClaimedGatewayIdentity>, GatewayIdentityError> {
            Ok(None)
        }
    }

    let ports = Ports;
    assert_object_safe_client(&ports);
    assert_object_safe_store(&ports);
    assert_object_safe_generator(&ports);
    assert_object_safe_claimed_source(&ports);
}

#[test]
fn target_normalizes_a_strict_cloud_origin_and_canonical_ids() {
    let target = aether_ports::GatewayEnrollmentTarget::new(
        "https://EXAMPLE.com:443/",
        "0190d8c5-a8dd-7c4e-8b2f-25cc70d02d31",
        "0190d8c5-a8dd-7c4e-8b2f-25cc70d02d32",
        "0190d8c5-a8dd-7c4e-8b2f-25cc70d02d33",
        CloudEndpointPolicy::Production,
    )
    .expect("valid target");

    assert_eq!(target.cloud_origin(), "https://example.com");
    assert_eq!(
        target.tenant_id(),
        Uuid::parse_str("0190d8c5-a8dd-7c4e-8b2f-25cc70d02d31").expect("uuid")
    );
    assert_eq!(
        target.project_id(),
        Uuid::parse_str("0190d8c5-a8dd-7c4e-8b2f-25cc70d02d32").expect("uuid")
    );
    assert_eq!(
        target.gateway_id(),
        Uuid::parse_str("0190d8c5-a8dd-7c4e-8b2f-25cc70d02d33").expect("uuid")
    );
}

#[test]
fn target_rejects_unsafe_origins_and_noncanonical_or_nil_ids() {
    let tenant = "0190d8c5-a8dd-7c4e-8b2f-25cc70d02d31";
    let project = "0190d8c5-a8dd-7c4e-8b2f-25cc70d02d32";
    let gateway = "0190d8c5-a8dd-7c4e-8b2f-25cc70d02d33";

    for origin in [
        "http://example.com",
        "https://user@example.com",
        "https://example.com/path",
        "https://example.com?query=yes",
        "https://example.com/#fragment",
    ] {
        assert!(
            aether_ports::GatewayEnrollmentTarget::new(
                origin,
                tenant,
                project,
                gateway,
                CloudEndpointPolicy::Production,
            )
            .is_err(),
            "{origin} must be rejected"
        );
    }

    assert!(
        aether_ports::GatewayEnrollmentTarget::new(
            "http://localhost:8080/",
            tenant,
            project,
            gateway,
            CloudEndpointPolicy::AllowLoopbackHttp,
        )
        .is_ok()
    );
    assert!(
        aether_ports::GatewayEnrollmentTarget::new(
            "http://127.0.0.1:8080/",
            tenant,
            project,
            gateway,
            CloudEndpointPolicy::AllowLoopbackHttp,
        )
        .is_ok()
    );
    assert!(
        aether_ports::GatewayEnrollmentTarget::new(
            "http://192.168.1.10:8080/",
            tenant,
            project,
            gateway,
            CloudEndpointPolicy::AllowLoopbackHttp,
        )
        .is_err()
    );
    for ambiguous_loopback in [
        "http://127.1",
        "http://2130706433",
        "http://localhost.",
        "http://[::1]",
    ] {
        assert!(
            aether_ports::GatewayEnrollmentTarget::new(
                ambiguous_loopback,
                tenant,
                project,
                gateway,
                CloudEndpointPolicy::AllowLoopbackHttp,
            )
            .is_err(),
            "{ambiguous_loopback} must not pass the exact loopback policy"
        );
    }
    assert!(
        aether_ports::GatewayEnrollmentTarget::new(
            "https://example.com",
            "0190D8C5-A8DD-7C4E-8B2F-25CC70D02D31",
            project,
            gateway,
            CloudEndpointPolicy::Production,
        )
        .is_err()
    );
    assert!(
        aether_ports::GatewayEnrollmentTarget::new(
            "https://example.com",
            "00000000-0000-0000-0000-000000000000",
            project,
            gateway,
            CloudEndpointPolicy::Production,
        )
        .is_err()
    );
}

#[test]
fn private_material_and_claim_debug_output_are_redacted() {
    let seed_bytes = [0x5au8; 32];
    let public_bytes = [0xa5u8; 32];
    let seed = GatewayPrivateKeySeed::from_bytes(seed_bytes);
    let public_key = GatewayPublicKey::from_bytes(public_bytes);
    let target = aether_ports::GatewayEnrollmentTarget::new(
        "https://example.com",
        "0190d8c5-a8dd-7c4e-8b2f-25cc70d02d31",
        "0190d8c5-a8dd-7c4e-8b2f-25cc70d02d32",
        "0190d8c5-a8dd-7c4e-8b2f-25cc70d02d33",
        CloudEndpointPolicy::Production,
    )
    .expect("valid target");
    let idempotency_key = EnrollmentIdempotencyKey::new(
        Uuid::parse_str("8fed195b-01bc-502d-8e5f-c7e804086868").expect("uuid"),
    )
    .expect("non-nil UUID");
    let token = SecretMaterial::new("never-log-this-token").expect("secret");
    let claim = CloudEnrollmentClaim::new(target, public_key, idempotency_key, token);

    let seed_debug = format!("{seed:?}");
    let key_debug = format!("{:?}", claim.public_key());
    let claim_debug = format!("{claim:?}");

    assert!(seed_debug.contains("[REDACTED]"));
    assert!(!seed_debug.contains("5a"));
    assert!(key_debug.contains("[REDACTED]"));
    assert!(!claim_debug.contains("never-log-this-token"));
    assert!(!claim_debug.contains(&"a5".repeat(32)));
    assert_eq!(seed.expose(), &seed_bytes);
    assert_eq!(claim.public_key().as_bytes(), &public_bytes);
    assert_eq!(
        claim.idempotency_key().as_str(),
        "8fed195b-01bc-502d-8e5f-c7e804086868"
    );
    assert_eq!(claim.enrollment_token().expose(), "never-log-this-token");
}

#[test]
fn preprotected_private_seed_moves_into_the_port_without_plaintext_conversion() {
    let seed = GatewayPrivateKeySeed::from_zeroizing(Zeroizing::new([0x3c_u8; 32]));

    assert_eq!(seed.expose(), &[0x3c_u8; 32]);
    assert_eq!(format!("{seed:?}"), "GatewayPrivateKeySeed([REDACTED])");
}

#[test]
fn fingerprint_is_lowercase_sha256_of_raw_public_key() {
    let public_key = GatewayPublicKey::from_bytes([0u8; 32]);
    assert_eq!(
        public_key.fingerprint().as_str(),
        "66687aadf862bd776c8fc18b8e9f8e20089714856ee233b3902a591d0d5f2925"
    );
}

#[test]
fn configured_identity_owns_the_stable_claim_key_invariant() {
    let target = aether_ports::GatewayEnrollmentTarget::new(
        "https://example.com",
        "0190d8c5-a8dd-7c4e-8b2f-25cc70d02d31",
        "0190d8c5-a8dd-7c4e-8b2f-25cc70d02d32",
        "0190d8c5-a8dd-7c4e-8b2f-25cc70d02d33",
        CloudEndpointPolicy::Production,
    )
    .expect("valid target");
    let public_key = GatewayPublicKey::from_bytes([0_u8; 32]);
    let key_generated = ConfiguredGatewayIdentity::key_generated(target.clone(), public_key);
    let stable = key_generated.stable_idempotency_key();
    assert_eq!(stable.as_str(), "d9c5e319-5c7f-582e-af26-317b1e94b015");
    assert_eq!(
        key_generated
            .validated_idempotency_key()
            .expect("key-generated identity"),
        stable
    );

    let pending =
        ConfiguredGatewayIdentity::claim_pending(target.clone(), public_key, stable.clone())
            .expect("stable pending identity");
    let claimed = ConfiguredGatewayIdentity::claimed(target.clone(), public_key, stable.clone(), 1)
        .expect("stable claimed identity");
    assert_eq!(pending.validated_idempotency_key(), Ok(stable.clone()));
    assert_eq!(claimed.validated_idempotency_key(), Ok(stable));

    let wrong = EnrollmentIdempotencyKey::new(
        Uuid::parse_str("8fed195b-01bc-502d-8e5f-c7e804086868").expect("UUID"),
    )
    .expect("non-nil key");
    assert!(matches!(
        ConfiguredGatewayIdentity::claim_pending(target.clone(), public_key, wrong.clone()),
        Err(GatewayIdentityError::InvalidState)
    ));
    assert!(matches!(
        ConfiguredGatewayIdentity::claimed(target, public_key, wrong, 1),
        Err(GatewayIdentityError::InvalidState)
    ));
}

#[test]
fn cloud_errors_have_fixed_retry_semantics() {
    assert!(CloudEnrollmentClientError::Timeout.is_retryable());
    assert!(CloudEnrollmentClientError::Unavailable.is_retryable());
    assert!(!CloudEnrollmentClientError::Rejected.is_retryable());
    assert!(!CloudEnrollmentClientError::Conflict.is_retryable());
    assert!(!CloudEnrollmentClientError::InvalidConfiguration.is_retryable());
    assert!(!CloudEnrollmentClientError::InvalidResponse.is_retryable());
}
