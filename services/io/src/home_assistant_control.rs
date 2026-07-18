//! Production trust, policy, persistence, and provider composition for governed control.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use aether_cloudlink::{SessionBinding, UplinkAuthentication, UplinkSigningProjection};
use aether_cloudlink_mqtt::{IntegrationControlTopicNamespace, MqttCloudLinkTransport};
use aether_domain::{EntityId, GatewayIdentity, IntegrationId};
use aether_home_assistant_bridge::WebSocketHomeAssistantTransport;
use aether_integration_control::{
    ActionReceiptEnvelope, CloudOfferVerifier, ControlDependencyError, ControlSession,
    IntegrationActionExecutor, IntegrationControlAudit, IntegrationControlCodec,
    IntegrationControlConfig, IntegrationControlLedger, IntegrationControlProcessor,
    LocalAuthorityDecision, LocalAuthorityRequest, LocalControlAuthority, MessageAuthentication,
    PERMISSION, ProjectionTargetResolver, ProviderExecutionResult, SpooledActionReceipt,
    SystemControlClock, UuidControlIdGenerator,
};
use aether_ports::{IntegrationProjectionQuery, PortError, PortErrorKind, PortResult, SecretRef};
use aether_store_local::{FileIntegrationControlAudit, FileIntegrationControlLedger};
use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use serde::Deserialize;
use tokio::sync::RwLock;
use zeroize::Zeroize;

use crate::home_assistant::{HomeAssistantIntegrationControlConfig, HomeAssistantStartupError};

const POLICY_SCHEMA: &str = "aether.edge.integration-control-policy.v1";
const MAX_POLICY_BYTES: u64 = 1024 * 1024;
const MAX_POLICY_ENTITIES: usize = 4_096;
const RECEIPT_BATCH_LIMIT: usize = 256;

/// Current live Home Assistant transport, intentionally empty while disconnected.
#[derive(Default)]
pub(crate) struct ActiveHomeAssistantExecutor {
    current: RwLock<Option<WebSocketHomeAssistantTransport>>,
}

impl ActiveHomeAssistantExecutor {
    pub(crate) async fn install(&self, transport: WebSocketHomeAssistantTransport) {
        *self.current.write().await = Some(transport);
    }

    pub(crate) async fn clear(&self) {
        *self.current.write().await = None;
    }
}

#[async_trait]
impl IntegrationActionExecutor for ActiveHomeAssistantExecutor {
    async fn execute(
        &self,
        action: &aether_integration_control::IntegrationPowerAction,
    ) -> ProviderExecutionResult {
        let transport = self.current.read().await.clone();
        match transport {
            Some(transport) => IntegrationActionExecutor::execute(&transport, action).await,
            None => ProviderExecutionResult::Unknown,
        }
    }
}

pub(crate) struct PreparedIntegrationControl {
    config: IntegrationControlConfig,
    verifier: Arc<dyn CloudOfferVerifier>,
    authority: Arc<dyn LocalControlAuthority>,
    audit: Arc<dyn IntegrationControlAudit>,
    ledger: Arc<dyn IntegrationControlLedger>,
    executor: Arc<dyn IntegrationActionExecutor>,
    active_executor: Arc<ActiveHomeAssistantExecutor>,
    legacy_receipt_signer: Option<LegacyReceiptSigner>,
    topics: IntegrationControlTopicNamespace,
    recovery_complete: AtomicBool,
}

impl PreparedIntegrationControl {
    pub(crate) fn prepare(
        config: &HomeAssistantIntegrationControlConfig,
        gateway_id: &GatewayIdentity,
        integration_id: &IntegrationId,
        mqtt_topic_prefix: &str,
    ) -> Result<Self, HomeAssistantStartupError> {
        let cloud_key = resolve_key(config.cloud_public_key_ref())?;
        let verifying_key = verifying_key(cloud_key)?;
        let legacy_receipt_signer = config
            .legacy_receipt_signing()
            .map(|(key_id, reference)| {
                let mut bytes = resolve_key(reference)?;
                let key = SigningKey::from_bytes(&bytes);
                bytes.zeroize();
                Ok::<_, HomeAssistantStartupError>(LegacyReceiptSigner {
                    key_id: key_id.to_owned(),
                    key,
                })
            })
            .transpose()?;
        let ledger: Arc<dyn IntegrationControlLedger> = Arc::new(
            FileIntegrationControlLedger::open(config.ledger_path())
                .map_err(|_error| HomeAssistantStartupError::IntegrationControlLedgerUnavailable)?,
        );
        let authority: Arc<dyn LocalControlAuthority> = Arc::new(LocalPolicy::open(
            config.policy_path(),
            gateway_id,
            integration_id,
        )?);
        let audit: Arc<dyn IntegrationControlAudit> = Arc::new(
            FileIntegrationControlAudit::open(config.audit_path())
                .map_err(|_error| HomeAssistantStartupError::IntegrationControlAuditUnavailable)?,
        );
        let topics = IntegrationControlTopicNamespace::new(mqtt_topic_prefix, gateway_id.as_str())
            .map_err(|_error| HomeAssistantStartupError::IntegrationControlTopicConfiguration)?;
        let control_config = IntegrationControlConfig::enabled(config.provider_timeout())
            .map_err(|_error| HomeAssistantStartupError::InvalidIntegrationControlConfiguration)?;
        let active_executor = Arc::new(ActiveHomeAssistantExecutor::default());
        Ok(Self {
            config: control_config,
            verifier: Arc::new(Ed25519CloudOfferVerifier {
                key_id: config.cloud_key_id().to_string(),
                key: verifying_key,
            }),
            authority,
            audit,
            ledger,
            executor: active_executor.clone(),
            active_executor,
            legacy_receipt_signer,
            topics,
            recovery_complete: AtomicBool::new(false),
        })
    }

    pub(crate) fn executor(&self) -> Arc<ActiveHomeAssistantExecutor> {
        self.active_executor.clone()
    }

    pub(crate) fn processor(
        &self,
        session: &SessionBinding,
        projection: Arc<dyn IntegrationProjectionQuery>,
    ) -> PortResult<IntegrationControlProcessor> {
        let control_session = ControlSession::new(
            session.gateway_id(),
            session.session_id(),
            session.session_epoch(),
            session.credential_generation(),
        )
        .map_err(control_error)?;
        Ok(IntegrationControlProcessor::new(
            self.config.clone(),
            control_session,
            self.verifier.clone(),
            Arc::new(ProjectionTargetResolver::new(projection)),
            self.authority.clone(),
            self.audit.clone(),
            self.ledger.clone(),
            self.executor.clone(),
            Arc::new(SystemControlClock),
            Arc::new(UuidControlIdGenerator),
        ))
    }

    pub(crate) async fn recover_once(
        &self,
        processor: &IntegrationControlProcessor,
    ) -> PortResult<()> {
        if self.recovery_complete.load(Ordering::Acquire) {
            return Ok(());
        }
        processor
            .recover_interrupted()
            .await
            .map_err(control_error)?;
        self.recovery_complete.store(true, Ordering::Release);
        Ok(())
    }

    pub(crate) async fn activate(&self, transport: &MqttCloudLinkTransport) -> PortResult<()> {
        transport
            .enable_integration_control(self.topics.clone())
            .await
    }

    pub(crate) async fn process_offer(
        &self,
        processor: &IntegrationControlProcessor,
        session: &SessionBinding,
        authentication: &UplinkAuthentication,
        transport: &(impl IntegrationControlReceiptPublisher + ?Sized),
        bytes: &[u8],
    ) -> PortResult<()> {
        processor.process(bytes).await.map_err(control_error)?;
        self.flush_receipts(session, authentication, transport)
            .await
    }

    pub(crate) async fn flush_receipts(
        &self,
        session: &SessionBinding,
        authentication: &UplinkAuthentication,
        transport: &(impl IntegrationControlReceiptPublisher + ?Sized),
    ) -> PortResult<()> {
        let pending = self
            .ledger
            .pending_receipts(RECEIPT_BATCH_LIMIT)
            .await
            .map_err(store_error)?;
        for receipt in pending {
            let bytes = self
                .encoded_receipt(session, authentication, &receipt)
                .await?;
            transport.publish_receipt(bytes).await?;
        }
        Ok(())
    }

    pub(crate) async fn acknowledge(
        &self,
        session: &SessionBinding,
        bytes: &[u8],
    ) -> PortResult<bool> {
        let candidate = aether_cloudlink::CloudLinkCodec::decode(bytes)
            .map_err(|error| PortError::new(PortErrorKind::InvalidData, error.to_string()))?;
        let aether_cloudlink::CandidateMessage::DurableAck(message) = candidate else {
            return Ok(false);
        };
        let ack = message
            .to_spool_ack(session)
            .map_err(|error| PortError::new(PortErrorKind::Rejected, error.to_string()))?;
        if ack.stream_id() != "integration-control-receipts" {
            return Ok(false);
        }
        if ack.stream_epoch() != 1 {
            return Err(PortError::new(
                PortErrorKind::Rejected,
                "Integration-control receipt ACK has the wrong stream epoch",
            ));
        }
        self.ledger.acknowledge(&ack).await.map_err(store_error)?;
        Ok(true)
    }

    async fn encoded_receipt(
        &self,
        session: &SessionBinding,
        authentication: &UplinkAuthentication,
        receipt: &SpooledActionReceipt,
    ) -> PortResult<Vec<u8>> {
        encode_receipt(
            session,
            authentication,
            self.legacy_receipt_signer.as_ref(),
            receipt,
        )
    }
}

#[async_trait]
pub(crate) trait IntegrationControlReceiptPublisher: Send + Sync {
    async fn publish_receipt(&self, payload: Vec<u8>) -> PortResult<()>;
}

#[async_trait]
impl IntegrationControlReceiptPublisher for MqttCloudLinkTransport {
    async fn publish_receipt(&self, payload: Vec<u8>) -> PortResult<()> {
        self.send_integration_control_receipt(payload).await
    }
}

struct Ed25519CloudOfferVerifier {
    key_id: String,
    key: VerifyingKey,
}

/// Explicit test-harness signer for a trusted connector's control receipts.
///
/// It is never presented as the CloudLink session signer and is absent from
/// Gateway-signed production composition.
struct LegacyReceiptSigner {
    key_id: String,
    key: SigningKey,
}

impl LegacyReceiptSigner {
    fn authenticate(
        &self,
        projection: &UplinkSigningProjection,
    ) -> PortResult<MessageAuthentication> {
        let signing_bytes = projection.canonical_bytes().map_err(cloudlink_error)?;
        MessageAuthentication::new(
            &self.key_id,
            URL_SAFE_NO_PAD.encode(self.key.sign(&signing_bytes).to_bytes()),
        )
        .map_err(control_error)
    }
}

#[async_trait]
impl CloudOfferVerifier for Ed25519CloudOfferVerifier {
    async fn verify(
        &self,
        key_id: &str,
        signature: &str,
        signing_bytes: &[u8],
    ) -> Result<bool, ControlDependencyError> {
        if key_id != self.key_id {
            return Ok(false);
        }
        let Ok(bytes) = decode_base64url::<64>(signature) else {
            return Ok(false);
        };
        Ok(self
            .key
            .verify_strict(signing_bytes, &Signature::from_bytes(&bytes))
            .is_ok())
    }
}

fn encode_receipt(
    session: &SessionBinding,
    authentication: &UplinkAuthentication,
    legacy_receipt_signer: Option<&LegacyReceiptSigner>,
    receipt: &SpooledActionReceipt,
) -> PortResult<Vec<u8>> {
    let control_session = ControlSession::new(
        session.gateway_id(),
        session.session_id(),
        session.session_epoch(),
        session.credential_generation(),
    )
    .map_err(control_error)?;
    let sent_at_ms = receipt.sent_at_ms();
    let delivery = receipt.delivery().map_err(store_error)?;
    let projection = UplinkSigningProjection::delivery(
        session.gateway_id(),
        &session.credential_generation().to_string(),
        session.session_id(),
        &session.session_epoch().to_string(),
        "integration-action-receipt",
        &sent_at_ms.to_string(),
        None,
        delivery.stream_id(),
        &delivery.stream_epoch().to_string(),
        &delivery.position().to_string(),
        delivery.batch_id(),
        delivery.digest(),
    )
    .map_err(cloudlink_error)?;
    let session_authentication = authentication
        .authenticate(&projection)
        .map_err(cloudlink_error)?
        .map(|authentication| {
            MessageAuthentication::new(authentication.key_id(), authentication.signature())
                .map_err(control_error)
        })
        .transpose()?;
    let message_authentication = match session_authentication {
        Some(authentication) => Some(authentication),
        None => legacy_receipt_signer
            .map(|signer| signer.authenticate(&projection))
            .transpose()?,
    };
    let envelope: ActionReceiptEnvelope = IntegrationControlCodec::receipt_envelope(
        &control_session,
        sent_at_ms,
        delivery,
        message_authentication,
        receipt.payload().clone(),
    )
    .map_err(control_error)?;
    IntegrationControlCodec::encode(&envelope).map_err(control_error)
}

struct LocalPolicy {
    gateway_id: GatewayIdentity,
    integration_id: IntegrationId,
    commissioned_entities: BTreeSet<String>,
    delegated_entities: BTreeSet<String>,
    allowed_subjects: BTreeSet<String>,
}

impl LocalPolicy {
    fn open(
        path: &Path,
        expected_gateway: &GatewayIdentity,
        expected_integration: &IntegrationId,
    ) -> Result<Self, HomeAssistantStartupError> {
        reject_non_regular_file(path)?;
        let mut file = File::open(path)
            .map_err(|_source| HomeAssistantStartupError::IntegrationControlPolicyUnavailable)?;
        let length = file
            .metadata()
            .map_err(|_source| HomeAssistantStartupError::IntegrationControlPolicyUnavailable)?
            .len();
        if length == 0 || length > MAX_POLICY_BYTES {
            return Err(HomeAssistantStartupError::IntegrationControlPolicyUnavailable);
        }
        let capacity = usize::try_from(length)
            .map_err(|_source| HomeAssistantStartupError::IntegrationControlPolicyUnavailable)?;
        let mut bytes = Vec::with_capacity(capacity);
        file.read_to_end(&mut bytes)
            .map_err(|_source| HomeAssistantStartupError::IntegrationControlPolicyUnavailable)?;
        let document: LocalPolicyDocument = serde_json::from_slice(&bytes)
            .map_err(|_source| HomeAssistantStartupError::IntegrationControlPolicyUnavailable)?;
        if document.schema != POLICY_SCHEMA
            || document.permission != PERMISSION
            || document.gateway_id != expected_gateway.as_str()
            || document.integration_id != expected_integration.as_str()
        {
            return Err(HomeAssistantStartupError::IntegrationControlPolicyUnavailable);
        }
        let commissioned_entities = entity_set(document.commissioned_entities)?;
        let delegated_entities = entity_set(document.delegated_entities)?;
        let allowed_subjects = subject_set(document.allowed_subjects)?;
        Ok(Self {
            gateway_id: expected_gateway.clone(),
            integration_id: expected_integration.clone(),
            commissioned_entities,
            delegated_entities,
            allowed_subjects,
        })
    }
}

#[async_trait]
impl LocalControlAuthority for LocalPolicy {
    async fn evaluate(
        &self,
        request: &LocalAuthorityRequest<'_>,
    ) -> Result<LocalAuthorityDecision, ControlDependencyError> {
        let offer = request.offer();
        let target = request.target().target();
        let scoped = offer.gateway_id() == self.gateway_id.as_str()
            && target.integration_id() == self.integration_id.as_str();
        let subject = offer.intent().authorization().subject_id();
        Ok(LocalAuthorityDecision {
            commissioned: scoped && self.commissioned_entities.contains(target.entity_id()),
            delegated: scoped && self.delegated_entities.contains(target.entity_id()),
            permission_granted: scoped && self.allowed_subjects.contains(subject),
            confirmation_valid: scoped
                && offer.intent().confirmation().subject_id() == subject
                && self.allowed_subjects.contains(subject),
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalPolicyDocument {
    schema: String,
    gateway_id: String,
    integration_id: String,
    permission: String,
    commissioned_entities: Vec<String>,
    delegated_entities: Vec<String>,
    allowed_subjects: Vec<String>,
}

fn entity_set(values: Vec<String>) -> Result<BTreeSet<String>, HomeAssistantStartupError> {
    if values.is_empty() || values.len() > MAX_POLICY_ENTITIES {
        return Err(HomeAssistantStartupError::IntegrationControlPolicyUnavailable);
    }
    let mut result = BTreeSet::new();
    for value in values {
        EntityId::new(&value)
            .map_err(|_source| HomeAssistantStartupError::IntegrationControlPolicyUnavailable)?;
        if !result.insert(value) {
            return Err(HomeAssistantStartupError::IntegrationControlPolicyUnavailable);
        }
    }
    Ok(result)
}

fn subject_set(values: Vec<String>) -> Result<BTreeSet<String>, HomeAssistantStartupError> {
    if values.is_empty() || values.len() > MAX_POLICY_ENTITIES {
        return Err(HomeAssistantStartupError::IntegrationControlPolicyUnavailable);
    }
    let mut result = BTreeSet::new();
    for value in values {
        if !valid_identifier(&value) || !result.insert(value) {
            return Err(HomeAssistantStartupError::IntegrationControlPolicyUnavailable);
        }
    }
    Ok(result)
}

fn resolve_key(reference: &SecretRef) -> Result<[u8; 32], HomeAssistantStartupError> {
    let variable = reference
        .as_str()
        .strip_prefix("env:")
        .ok_or(HomeAssistantStartupError::IntegrationControlSecretUnavailable)?;
    let mut encoded = std::env::var(variable)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or(HomeAssistantStartupError::IntegrationControlSecretUnavailable)?;
    let result = decode_base64url::<32>(&encoded)
        .map_err(|()| HomeAssistantStartupError::InvalidIntegrationControlKeyMaterial);
    encoded.zeroize();
    result
}

fn verifying_key(bytes: [u8; 32]) -> Result<VerifyingKey, HomeAssistantStartupError> {
    VerifyingKey::from_bytes(&bytes)
        .map_err(|_source| HomeAssistantStartupError::InvalidIntegrationControlKeyMaterial)
}

fn decode_base64url<const N: usize>(value: &str) -> Result<[u8; N], ()> {
    let mut decoded = URL_SAFE_NO_PAD.decode(value).map_err(|_source| ())?;
    if URL_SAFE_NO_PAD.encode(&decoded) != value || decoded.len() != N {
        decoded.zeroize();
        return Err(());
    }
    let mut bytes = [0_u8; N];
    bytes.copy_from_slice(&decoded);
    decoded.zeroize();
    Ok(bytes)
}

fn reject_non_regular_file(path: &Path) -> Result<(), HomeAssistantStartupError> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_source| HomeAssistantStartupError::IntegrationControlPolicyUnavailable)?;
    if !metadata.file_type().is_file() {
        return Err(HomeAssistantStartupError::IntegrationControlPolicyUnavailable);
    }
    Ok(())
}

fn valid_identifier(value: &str) -> bool {
    let mut bytes = value.bytes();
    value.len() <= 128
        && bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && bytes
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

fn control_error(error: impl std::fmt::Display) -> PortError {
    PortError::new(
        PortErrorKind::InvalidData,
        format!("Integration-control processing failed: {error}"),
    )
}

fn cloudlink_error(_error: impl std::fmt::Display) -> PortError {
    PortError::new(
        PortErrorKind::InvalidData,
        "CloudLink per-uplink authentication failed closed",
    )
}

fn store_error(_error: impl std::fmt::Display) -> PortError {
    PortError::new(
        PortErrorKind::Unavailable,
        "Integration-control durable ledger is unavailable",
    )
}

#[cfg(test)]
#[path = "home_assistant_control_tests.rs"]
mod tests;
