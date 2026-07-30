use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use aether_application::{EnrollGatewayWithAetherCloud, GatewayEnrollmentError};
use aether_domain::TimestampMs;
use aether_ports::{
    Clock, CloudEndpointPolicy, CloudEnrollmentClaim, CloudEnrollmentClient,
    CloudEnrollmentClientError, CloudEnrollmentReceipt, ConfiguredGatewayIdentity,
    EnrollmentIdempotencyKey, GatewayEnrollmentPhase, GatewayEnrollmentStatus,
    GatewayEnrollmentTarget, GatewayIdentityError, GatewayIdentityInitialization,
    GatewayIdentityKeyGenerator, GatewayIdentityStore, GatewayPrivateKeySeed, GatewayPublicKey,
    GeneratedGatewayIdentityKey, PortResult, SecretMaterial,
};
use async_trait::async_trait;

const TENANT_ID: &str = "0190d8c5-a8dd-7c4e-8b2f-25cc70d02d31";
const PROJECT_ID: &str = "0190d8c5-a8dd-7c4e-8b2f-25cc70d02d32";
const GATEWAY_ID: &str = "0190d8c5-a8dd-7c4e-8b2f-25cc70d02d33";
const SAFE_REVISION_MAX: u64 = 9_007_199_254_740_991;

#[derive(Clone, Debug, PartialEq, Eq)]
struct CapturedClaim {
    target: GatewayEnrollmentTarget,
    public_key: [u8; 32],
    fingerprint: String,
    idempotency_key: String,
    token: String,
}

struct FakeClient {
    events: Arc<Mutex<Vec<&'static str>>>,
    responses: Mutex<VecDeque<Result<CloudEnrollmentReceipt, CloudEnrollmentClientError>>>,
    claims: Mutex<Vec<CapturedClaim>>,
}

impl FakeClient {
    fn new(
        events: Arc<Mutex<Vec<&'static str>>>,
        responses: impl IntoIterator<Item = Result<CloudEnrollmentReceipt, CloudEnrollmentClientError>>,
    ) -> Self {
        Self {
            events,
            responses: Mutex::new(responses.into_iter().collect()),
            claims: Mutex::new(Vec::new()),
        }
    }

    fn claims(&self) -> Vec<CapturedClaim> {
        self.claims.lock().expect("claims lock").clone()
    }
}

#[async_trait]
impl CloudEnrollmentClient for FakeClient {
    async fn claim(
        &self,
        claim: CloudEnrollmentClaim,
    ) -> Result<CloudEnrollmentReceipt, CloudEnrollmentClientError> {
        self.events.lock().expect("events lock").push("claim");
        self.claims
            .lock()
            .expect("claims lock")
            .push(CapturedClaim {
                target: claim.target().clone(),
                public_key: *claim.public_key().as_bytes(),
                fingerprint: claim.fingerprint().as_str().to_owned(),
                idempotency_key: claim.idempotency_key().as_str().to_owned(),
                token: claim.enrollment_token().expose().to_owned(),
            });
        self.responses
            .lock()
            .expect("responses lock")
            .pop_front()
            .expect("configured response")
    }
}

struct FakeKeyGenerator {
    events: Arc<Mutex<Vec<&'static str>>>,
    calls: Mutex<usize>,
}

impl FakeKeyGenerator {
    fn calls(&self) -> usize {
        *self.calls.lock().expect("calls lock")
    }
}

impl GatewayIdentityKeyGenerator for FakeKeyGenerator {
    fn generate(&self) -> Result<GeneratedGatewayIdentityKey, GatewayIdentityError> {
        self.events.lock().expect("events lock").push("generate");
        *self.calls.lock().expect("calls lock") += 1;
        Ok(GeneratedGatewayIdentityKey::new(
            GatewayPrivateKeySeed::from_bytes([0x11; 32]),
            GatewayPublicKey::from_bytes([0x22; 32]),
        ))
    }
}

struct FakeClock {
    events: Arc<Mutex<Vec<&'static str>>>,
    next: Mutex<u64>,
}

impl Clock for FakeClock {
    fn now(&self) -> PortResult<TimestampMs> {
        self.events.lock().expect("events lock").push("clock");
        let mut next = self.next.lock().expect("clock lock");
        let value = *next;
        *next += 1;
        Ok(TimestampMs::new(value))
    }
}

struct FakeStore {
    events: Arc<Mutex<Vec<&'static str>>>,
    status: Mutex<GatewayEnrollmentStatus>,
}

impl FakeStore {
    fn new(events: Arc<Mutex<Vec<&'static str>>>, status: GatewayEnrollmentStatus) -> Self {
        Self {
            events,
            status: Mutex::new(status),
        }
    }

    fn status(&self) -> GatewayEnrollmentStatus {
        self.status.lock().expect("status lock").clone()
    }
}

#[async_trait]
impl GatewayIdentityStore for FakeStore {
    async fn load(&self) -> Result<GatewayEnrollmentStatus, GatewayIdentityError> {
        Ok(self.status())
    }

    async fn persist_key_generated(
        &self,
        identity: GatewayIdentityInitialization,
        _recorded_at: TimestampMs,
    ) -> Result<(), GatewayIdentityError> {
        self.events
            .lock()
            .expect("events lock")
            .push("persist-key-generated");
        let (target, generated) = identity.into_parts();
        let (_, public_key) = generated.into_parts();
        *self.status.lock().expect("status lock") = GatewayEnrollmentStatus::Configured(
            ConfiguredGatewayIdentity::key_generated(target, public_key),
        );
        Ok(())
    }

    async fn mark_claim_pending(
        &self,
        identity: &ConfiguredGatewayIdentity,
        idempotency_key: &EnrollmentIdempotencyKey,
        _recorded_at: TimestampMs,
    ) -> Result<(), GatewayIdentityError> {
        self.events
            .lock()
            .expect("events lock")
            .push("persist-claim-pending");
        *self.status.lock().expect("status lock") = GatewayEnrollmentStatus::Configured(
            ConfiguredGatewayIdentity::claim_pending(
                identity.target().clone(),
                *identity.public_key(),
                idempotency_key.clone(),
            )
            .expect("application provides the stable Claim key"),
        );
        Ok(())
    }

    async fn mark_claimed(
        &self,
        identity: &ConfiguredGatewayIdentity,
        idempotency_key: &EnrollmentIdempotencyKey,
        revision: u64,
        _recorded_at: TimestampMs,
    ) -> Result<(), GatewayIdentityError> {
        self.events
            .lock()
            .expect("events lock")
            .push("persist-claimed");
        *self.status.lock().expect("status lock") = GatewayEnrollmentStatus::Configured(
            ConfiguredGatewayIdentity::claimed(
                identity.target().clone(),
                *identity.public_key(),
                idempotency_key.clone(),
                revision,
            )
            .expect("valid claimed identity"),
        );
        Ok(())
    }
}

struct Fixture {
    use_case: EnrollGatewayWithAetherCloud,
    client: Arc<FakeClient>,
    generator: Arc<FakeKeyGenerator>,
    store: Arc<FakeStore>,
    events: Arc<Mutex<Vec<&'static str>>>,
}

impl Fixture {
    fn new(
        status: GatewayEnrollmentStatus,
        responses: impl IntoIterator<Item = Result<CloudEnrollmentReceipt, CloudEnrollmentClientError>>,
    ) -> Self {
        let events = Arc::new(Mutex::new(Vec::new()));
        let client = Arc::new(FakeClient::new(events.clone(), responses));
        let generator = Arc::new(FakeKeyGenerator {
            events: events.clone(),
            calls: Mutex::new(0),
        });
        let store = Arc::new(FakeStore::new(events.clone(), status));
        let clock = Arc::new(FakeClock {
            events: events.clone(),
            next: Mutex::new(1_000),
        });
        let use_case = EnrollGatewayWithAetherCloud::new(
            client.clone(),
            generator.clone(),
            store.clone(),
            clock,
        );
        Self {
            use_case,
            client,
            generator,
            store,
            events,
        }
    }
}

fn target(origin: &str) -> GatewayEnrollmentTarget {
    target_with(origin, TENANT_ID, PROJECT_ID, GATEWAY_ID)
}

fn target_with(
    origin: &str,
    tenant_id: &str,
    project_id: &str,
    gateway_id: &str,
) -> GatewayEnrollmentTarget {
    GatewayEnrollmentTarget::new(
        origin,
        tenant_id,
        project_id,
        gateway_id,
        CloudEndpointPolicy::Production,
    )
    .expect("valid target")
}

fn token(value: &str) -> SecretMaterial {
    SecretMaterial::new(value).expect("valid secret")
}

fn stable_idempotency_key(
    target: &GatewayEnrollmentTarget,
    public_key: GatewayPublicKey,
) -> EnrollmentIdempotencyKey {
    ConfiguredGatewayIdentity::key_generated(target.clone(), public_key).stable_idempotency_key()
}

#[tokio::test]
async fn persists_pending_identity_before_claim_and_claimed_after_receipt() {
    let fixture = Fixture::new(
        GatewayEnrollmentStatus::Unconfigured,
        [Ok(CloudEnrollmentReceipt::new(GATEWAY_ID, 17))],
    );

    let result = fixture
        .use_case
        .enroll(target("https://cloud.example"), token("one-time-token"))
        .await
        .expect("enrollment succeeds");

    assert!(!result.was_already_claimed());
    assert_eq!(result.identity().claimed_revision(), Some(17));
    assert_eq!(fixture.generator.calls(), 1);
    assert_eq!(
        fixture.events.lock().expect("events lock").as_slice(),
        [
            "generate",
            "clock",
            "persist-key-generated",
            "clock",
            "persist-claim-pending",
            "claim",
            "clock",
            "persist-claimed",
        ]
    );
    let claims = fixture.client.claims();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].token, "one-time-token");
    assert_eq!(claims[0].public_key, [0x22; 32]);
    assert_eq!(claims[0].fingerprint.len(), 64);
    assert_eq!(
        claims[0].fingerprint,
        GatewayPublicKey::from_bytes([0x22; 32])
            .fingerprint()
            .as_str()
    );
    assert_eq!(
        fixture.store.status(),
        GatewayEnrollmentStatus::Configured(result.identity().clone())
    );
}

#[tokio::test]
async fn timeout_retry_reuses_key_and_stable_idempotency_key() {
    let fixture = Fixture::new(
        GatewayEnrollmentStatus::Unconfigured,
        [
            Err(CloudEnrollmentClientError::Timeout),
            Ok(CloudEnrollmentReceipt::new(GATEWAY_ID, 21)),
        ],
    );

    let first = fixture
        .use_case
        .enroll(target("https://cloud.example"), token("first-token"))
        .await;
    assert!(matches!(
        first,
        Err(GatewayEnrollmentError::Cloud(
            CloudEnrollmentClientError::Timeout
        ))
    ));
    let pending = fixture.store.status();
    let GatewayEnrollmentStatus::Configured(pending) = pending else {
        panic!("pending identity must remain persisted");
    };
    assert!(matches!(
        pending.phase(),
        GatewayEnrollmentPhase::ClaimPending(_)
    ));

    let result = fixture
        .use_case
        .enroll(target("https://cloud.example"), token("second-token"))
        .await
        .expect("retry succeeds");

    assert_eq!(result.identity().claimed_revision(), Some(21));
    assert_eq!(fixture.generator.calls(), 1);
    let claims = fixture.client.claims();
    assert_eq!(claims.len(), 2);
    assert_eq!(claims[0].public_key, claims[1].public_key);
    assert_eq!(claims[0].fingerprint, claims[1].fingerprint);
    assert_eq!(claims[0].idempotency_key, claims[1].idempotency_key);
    assert_eq!(claims[0].token, "first-token");
    assert_eq!(claims[1].token, "second-token");
}

#[tokio::test]
async fn same_claimed_scope_is_idempotent_without_generation_or_cloud_call() {
    let target = target("https://cloud.example");
    let public_key = GatewayPublicKey::from_bytes([0x44; 32]);
    let configured = ConfiguredGatewayIdentity::claimed(
        target.clone(),
        public_key,
        stable_idempotency_key(&target, public_key),
        7,
    )
    .expect("claimed identity");
    let fixture = Fixture::new(GatewayEnrollmentStatus::Configured(configured.clone()), []);

    let result = fixture
        .use_case
        .enroll(target.clone(), token("unused-token"))
        .await
        .expect("already claimed is success");

    assert!(result.was_already_claimed());
    assert_eq!(result.identity(), &configured);
    assert_eq!(fixture.generator.calls(), 0);
    assert!(fixture.client.claims().is_empty());
    assert!(fixture.events.lock().expect("events lock").is_empty());
}

#[tokio::test]
async fn any_different_existing_scope_or_origin_fails_closed() {
    let alternatives = [
        target_with("https://other.example", TENANT_ID, PROJECT_ID, GATEWAY_ID),
        target_with(
            "https://cloud.example",
            "0190d8c5-a8dd-7c4e-8b2f-25cc70d02d34",
            PROJECT_ID,
            GATEWAY_ID,
        ),
        target_with(
            "https://cloud.example",
            TENANT_ID,
            "0190d8c5-a8dd-7c4e-8b2f-25cc70d02d34",
            GATEWAY_ID,
        ),
        target_with(
            "https://cloud.example",
            TENANT_ID,
            PROJECT_ID,
            "0190d8c5-a8dd-7c4e-8b2f-25cc70d02d34",
        ),
    ];

    for alternative in alternatives {
        let existing = ConfiguredGatewayIdentity::key_generated(
            target("https://cloud.example"),
            GatewayPublicKey::from_bytes([0x33; 32]),
        );
        let fixture = Fixture::new(GatewayEnrollmentStatus::Configured(existing), []);
        let error = fixture
            .use_case
            .enroll(alternative, token("unused-token"))
            .await
            .expect_err("scope replacement must fail");

        assert!(matches!(error, GatewayEnrollmentError::IdentityConflict));
        assert_eq!(fixture.generator.calls(), 0);
        assert!(fixture.client.claims().is_empty());
    }
}

#[tokio::test]
async fn rejects_mismatched_gateway_and_unsafe_revision_without_claiming() {
    for receipt in [
        CloudEnrollmentReceipt::new("0190d8c5-a8dd-7c4e-8b2f-25cc70d02d34", 1),
        CloudEnrollmentReceipt::new(GATEWAY_ID, 0),
        CloudEnrollmentReceipt::new(GATEWAY_ID, SAFE_REVISION_MAX + 1),
        CloudEnrollmentReceipt::new("not-a-uuid", 1),
    ] {
        let fixture = Fixture::new(GatewayEnrollmentStatus::Unconfigured, [Ok(receipt)]);
        let error = fixture
            .use_case
            .enroll(target("https://cloud.example"), token("secret-token"))
            .await
            .expect_err("invalid receipt must fail");

        assert!(matches!(error, GatewayEnrollmentError::InvalidCloudReceipt));
        let GatewayEnrollmentStatus::Configured(identity) = fixture.store.status() else {
            panic!("pending identity must remain");
        };
        assert!(matches!(
            identity.phase(),
            GatewayEnrollmentPhase::ClaimPending(_)
        ));
        assert!(
            !error.to_string().contains("secret-token"),
            "error output must not contain token"
        );
    }
}
