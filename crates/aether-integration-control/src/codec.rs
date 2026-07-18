//! Bounded strict JSON, RFC 8785 projections, and SHA-256 bindings.

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{ControlResult, IntegrationControlError, IntegrationControlErrorCode};
use crate::ports::ControlSession;
use crate::wire::{
    ActionIntent, ActionOffer, ActionReceiptEnvelope, ActionReceiptPayload, MessageAuthentication,
    ReceiptDelivery,
};
use crate::{CLOUDLINK_PROTOCOL_VERSION, MAX_INTEGRATION_CONTROL_MESSAGE_BYTES};

/// Strict codec for the default-off Integration-control extension.
pub struct IntegrationControlCodec;

impl IntegrationControlCodec {
    /// Decodes and fully validates one signed action offer.
    pub fn decode_offer(bytes: &[u8]) -> ControlResult<ActionOffer> {
        bound(bytes)?;
        let offer: ActionOffer = serde_json::from_slice(bytes)
            .map_err(|_source| invalid_message("offer is not closed valid JSON"))?;
        offer.validate()?;
        let found = Self::intent_digest(offer.intent())?;
        if found != offer.intent_digest() {
            return Err(IntegrationControlError::new(
                IntegrationControlErrorCode::IntentDigestMismatch,
                "offer intent digest does not match canonical intent",
            ));
        }
        Ok(offer)
    }

    /// Decodes and fully validates one authenticated receipt envelope.
    pub fn decode_receipt_envelope(bytes: &[u8]) -> ControlResult<ActionReceiptEnvelope> {
        bound(bytes)?;
        let envelope: ActionReceiptEnvelope = serde_json::from_slice(bytes)
            .map_err(|_source| invalid_message("receipt is not closed valid JSON"))?;
        envelope.validate()?;
        if Self::receipt_digest(envelope.payload())? != envelope.delivery().digest() {
            return Err(invalid_message(
                "receipt business digest does not match its delivery descriptor",
            ));
        }
        Ok(envelope)
    }

    /// Canonically encodes a previously validated contract value.
    pub fn encode<T: Serialize>(value: &T) -> ControlResult<Vec<u8>> {
        let bytes = serde_json_canonicalizer::to_vec(value)
            .map_err(|_source| invalid_message("canonical JSON encoding failed"))?;
        bound(&bytes)?;
        Ok(bytes)
    }

    /// Returns the RFC 8785 SHA-256 digest of the complete closed intent.
    pub fn intent_digest(intent: &ActionIntent) -> ControlResult<String> {
        intent.validate()?;
        canonical_digest(intent)
    }

    /// Returns the existing CloudLink business digest for a receipt payload.
    pub fn receipt_digest(payload: &ActionReceiptPayload) -> ControlResult<String> {
        payload.validate()?;
        #[derive(Serialize)]
        struct DigestInput<'a> {
            protocol_version: &'static str,
            message_kind: &'static str,
            payload: &'a ActionReceiptPayload,
        }
        canonical_digest(&DigestInput {
            protocol_version: CLOUDLINK_PROTOCOL_VERSION,
            message_kind: crate::MESSAGE_KIND_RECEIPT,
            payload,
        })
    }

    /// Builds an authenticated existing CloudLink uplink envelope for a spooled receipt.
    pub fn receipt_envelope(
        session: &ControlSession,
        sent_at_ms: u64,
        delivery: ReceiptDelivery,
        message_authentication: Option<MessageAuthentication>,
        payload: ActionReceiptPayload,
    ) -> ControlResult<ActionReceiptEnvelope> {
        if Self::receipt_digest(&payload)? != delivery.digest() {
            return Err(invalid_message(
                "receipt delivery descriptor has the wrong business digest",
            ));
        }
        ActionReceiptEnvelope::new(
            session.gateway_id().to_string(),
            session.session_id().to_string(),
            session.session_epoch(),
            session.credential_generation(),
            sent_at_ms,
            delivery,
            message_authentication,
            payload,
        )
    }

    /// Returns the exact RFC 8785 CloudLink per-uplink receipt signing projection.
    pub fn receipt_signing_bytes(
        session: &ControlSession,
        sent_at_ms: u64,
        delivery: &ReceiptDelivery,
    ) -> ControlResult<Vec<u8>> {
        delivery.validate()?;
        aether_cloudlink::UplinkSigningProjection::delivery(
            session.gateway_id(),
            &session.credential_generation().to_string(),
            session.session_id(),
            &session.session_epoch().to_string(),
            crate::MESSAGE_KIND_RECEIPT,
            &sent_at_ms.to_string(),
            None,
            delivery.stream_id(),
            &delivery.stream_epoch().to_string(),
            &delivery.position().to_string(),
            delivery.batch_id(),
            delivery.digest(),
        )
        .and_then(|projection| projection.canonical_bytes())
        .map_err(|_source| invalid_message("receipt signing projection failed"))
    }

    /// Computes an intent digest from closed JSON, useful for fixture and producer conformance.
    pub fn intent_digest_json(value: &Value) -> ControlResult<String> {
        let intent: ActionIntent = serde_json::from_value(value.clone())
            .map_err(|_source| invalid_message("intent is not closed valid JSON"))?;
        Self::intent_digest(&intent)
    }
}

fn canonical_digest(value: &impl Serialize) -> ControlResult<String> {
    let bytes = serde_json_canonicalizer::to_vec(value)
        .map_err(|_source| invalid_message("canonical digest projection failed"))?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn bound(bytes: &[u8]) -> ControlResult<()> {
    if bytes.is_empty() || bytes.len() > MAX_INTEGRATION_CONTROL_MESSAGE_BYTES {
        return Err(invalid_message(
            "Integration-control message exceeds its byte bound",
        ));
    }
    Ok(())
}

fn invalid_message(message: &'static str) -> IntegrationControlError {
    IntegrationControlError::new(IntegrationControlErrorCode::InvalidMessage, message)
}
