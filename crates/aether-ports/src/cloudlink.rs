//! Durable CloudLink stream and transport capabilities.
//!
//! These ports are deliberately distinct from [`crate::DurableOutbox`]. A
//! transport outbox may define successful publication as its acknowledgement
//! boundary. CloudLink records remain durable until a matching application
//! receipt proves that the cloud committed the business fact.

use std::error::Error;
use std::fmt;
use std::sync::Arc;

use aether_domain::TimestampMs;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Business message classes that may enter the durable CloudLink stream.
///
/// There is intentionally no command or arbitrary-RPC variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CloudLinkMessageKind {
    /// A verified Runtime Manifest report.
    RuntimeManifestReport,
    /// A bounded batch of acquisition-owned point facts.
    TelemetryBatch,
    /// One durable alarm-state transition emitted by the local alarm owner.
    AlarmEvent,
    /// One complete provider-neutral delegated-integration topology replacement.
    IntegrationTopologySnapshot,
    /// One provider-neutral delegated-integration observation batch.
    IntegrationObservationBatch,
    /// Explicit evidence that a requested retained range no longer exists.
    DataLoss,
}

impl CloudLinkMessageKind {
    /// Returns the stable wire identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RuntimeManifestReport => "runtime-manifest-report",
            Self::TelemetryBatch => "telemetry-batch",
            Self::AlarmEvent => "alarm-event",
            Self::IntegrationTopologySnapshot => "integration-topology-snapshot",
            Self::IntegrationObservationBatch => "integration-observation-batch",
            Self::DataLoss => "data-loss",
        }
    }
}

/// Durable identity of one record in one logical stream epoch.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CloudLinkRecordIdentity {
    stream_id: String,
    stream_epoch: u64,
    position: u64,
}

impl CloudLinkRecordIdentity {
    /// Creates a record identity. Adapters allocate positions monotonically.
    #[must_use]
    pub fn new(stream_id: impl Into<String>, stream_epoch: u64, position: u64) -> Self {
        Self {
            stream_id: stream_id.into(),
            stream_epoch,
            position,
        }
    }

    /// Returns the logical stream identifier.
    #[must_use]
    pub fn stream_id(&self) -> &str {
        &self.stream_id
    }

    /// Returns the durable stream epoch.
    #[must_use]
    pub const fn stream_epoch(&self) -> u64 {
        self.stream_epoch
    }

    /// Returns the lossless position within the epoch.
    #[must_use]
    pub const fn position(&self) -> u64 {
        self.position
    }
}

/// Local delivery state. Only a durable application ACK permits removal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CloudLinkDeliveryState {
    /// Persisted but not offered on a session.
    Queued,
    /// Offered to one verified CloudLink session.
    Offered,
    /// Submitted through the transport; this is not a cloud durable ACK.
    TransportPublished,
}

/// Session identity used to validate offers and application acknowledgements.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudLinkSessionBinding {
    session_id: String,
    session_epoch: u64,
}

impl CloudLinkSessionBinding {
    /// Creates a session binding after session acceptance was validated.
    #[must_use]
    pub fn new(session_id: impl Into<String>, session_epoch: u64) -> Self {
        Self {
            session_id: session_id.into(),
            session_epoch,
        }
    }

    /// Returns the opaque session identifier.
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Returns the monotonic session epoch.
    #[must_use]
    pub const fn session_epoch(&self) -> u64 {
        self.session_epoch
    }
}

/// Canonical business content ready to receive a durable stream position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudLinkEnqueue {
    message_kind: CloudLinkMessageKind,
    batch_id: String,
    digest: String,
    payload: Vec<u8>,
    created_at: TimestampMs,
    expires_at: Option<TimestampMs>,
}

/// Result of lossless durable admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudLinkAdmission {
    identity: CloudLinkRecordIdentity,
    pending_record: Option<CloudLinkRecord>,
    duplicate: bool,
    acknowledged: bool,
}

/// Post-ACK identity-retention policy for one lossless admission.
///
/// Both policies prohibit pending-record eviction. Only facts whose upstream
/// retry contract requires deduplication after a lost HTTP response retain a
/// protected business receipt after Cloud acknowledgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudLinkReceiptRetention {
    /// Remove the pending record after ACK without consuming receipt capacity.
    DiscardAfterAck,
    /// Preserve the business identity and digest for exact retry recognition.
    RetainForIdempotency,
}

impl CloudLinkAdmission {
    /// Creates an admission for a new or already-pending record.
    #[must_use]
    pub fn pending(record: CloudLinkRecord, duplicate: bool) -> Self {
        Self {
            identity: record.identity().clone(),
            pending_record: Some(record),
            duplicate,
            acknowledged: false,
        }
    }

    /// Creates an exact duplicate admission for a previously acknowledged fact.
    #[must_use]
    pub fn acknowledged(identity: CloudLinkRecordIdentity) -> Self {
        Self {
            identity,
            pending_record: None,
            duplicate: true,
            acknowledged: true,
        }
    }

    /// Returns the stable stream identity originally assigned to the fact.
    #[must_use]
    pub const fn identity(&self) -> &CloudLinkRecordIdentity {
        &self.identity
    }

    /// Returns the retained record when it still requires Cloud delivery.
    #[must_use]
    pub const fn pending_record(&self) -> Option<&CloudLinkRecord> {
        self.pending_record.as_ref()
    }

    /// Reports whether the exact business event was already pending.
    #[must_use]
    pub const fn duplicate(&self) -> bool {
        self.duplicate
    }

    /// Reports that Cloud has already durably acknowledged this exact fact.
    #[must_use]
    pub const fn acknowledged_duplicate(&self) -> bool {
        self.acknowledged
    }
}

impl CloudLinkEnqueue {
    /// Creates content whose identity and canonical digest were already sealed.
    #[must_use]
    pub fn new(
        message_kind: CloudLinkMessageKind,
        batch_id: impl Into<String>,
        digest: impl Into<String>,
        payload: impl Into<Vec<u8>>,
        created_at: TimestampMs,
        expires_at: Option<TimestampMs>,
    ) -> Self {
        Self {
            message_kind,
            batch_id: batch_id.into(),
            digest: digest.into(),
            payload: payload.into(),
            created_at,
            expires_at,
        }
    }

    /// Returns the business message kind.
    #[must_use]
    pub const fn message_kind(&self) -> CloudLinkMessageKind {
        self.message_kind
    }

    /// Returns the stable batch identity.
    #[must_use]
    pub fn batch_id(&self) -> &str {
        &self.batch_id
    }

    /// Returns the canonical business-content digest.
    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// Returns canonical business payload bytes.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// Returns when the fact entered the spool.
    #[must_use]
    pub const fn created_at(&self) -> TimestampMs {
        self.created_at
    }

    /// Returns business expiry, when one exists.
    #[must_use]
    pub const fn expires_at(&self) -> Option<TimestampMs> {
        self.expires_at
    }
}

/// One durable CloudLink stream record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudLinkRecord {
    identity: CloudLinkRecordIdentity,
    message_kind: CloudLinkMessageKind,
    batch_id: String,
    digest: String,
    #[serde(with = "shared_payload")]
    payload: Arc<[u8]>,
    created_at_ms: u64,
    expires_at_ms: Option<u64>,
    lossless_admission: bool,
    retain_acknowledged_receipt: bool,
    state: CloudLinkDeliveryState,
    offered_session: Option<CloudLinkSessionBinding>,
}

impl CloudLinkRecord {
    /// Builds a queued record from an adapter-allocated identity and sealed input.
    ///
    /// This constructor exists for port implementations and deterministic test
    /// adapters. Production callers normally use [`CloudLinkSpool::enqueue`].
    #[must_use]
    pub fn from_enqueue(identity: CloudLinkRecordIdentity, input: CloudLinkEnqueue) -> Self {
        Self::from_enqueue_with_policy(identity, input, false, false)
    }

    /// Builds a queued lossless record whose acknowledged business identity
    /// must remain protected after the pending payload is removed.
    ///
    /// This constructor is reserved for implementations of
    /// [`CloudLinkSpool::admit_lossless`].
    #[must_use]
    pub fn from_lossless_enqueue(
        identity: CloudLinkRecordIdentity,
        input: CloudLinkEnqueue,
        receipt_retention: CloudLinkReceiptRetention,
    ) -> Self {
        Self::from_enqueue_with_policy(
            identity,
            input,
            true,
            receipt_retention == CloudLinkReceiptRetention::RetainForIdempotency,
        )
    }

    fn from_enqueue_with_policy(
        identity: CloudLinkRecordIdentity,
        input: CloudLinkEnqueue,
        lossless_admission: bool,
        retain_acknowledged_receipt: bool,
    ) -> Self {
        Self {
            identity,
            message_kind: input.message_kind,
            batch_id: input.batch_id,
            digest: input.digest,
            payload: input.payload.into(),
            created_at_ms: input.created_at.get(),
            expires_at_ms: input.expires_at.map(TimestampMs::get),
            lossless_admission,
            retain_acknowledged_receipt,
            state: CloudLinkDeliveryState::Queued,
            offered_session: None,
        }
    }

    /// Returns the durable record identity.
    #[must_use]
    pub const fn identity(&self) -> &CloudLinkRecordIdentity {
        &self.identity
    }

    /// Returns the business message kind.
    #[must_use]
    pub const fn message_kind(&self) -> CloudLinkMessageKind {
        self.message_kind
    }

    /// Returns the stable batch identity.
    #[must_use]
    pub fn batch_id(&self) -> &str {
        &self.batch_id
    }

    /// Returns the canonical business digest.
    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// Returns canonical business payload bytes.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// Returns when the record entered the spool.
    #[must_use]
    pub const fn created_at(&self) -> TimestampMs {
        TimestampMs::new(self.created_at_ms)
    }

    /// Returns business expiry, when supplied.
    #[must_use]
    pub const fn expires_at(&self) -> Option<TimestampMs> {
        match self.expires_at_ms {
            Some(value) => Some(TimestampMs::new(value)),
            None => None,
        }
    }

    /// Reports whether a durable business-identity receipt must survive ACK.
    #[must_use]
    pub const fn retains_acknowledged_receipt(&self) -> bool {
        self.retain_acknowledged_receipt
    }

    /// Reports whether this pending record is protected from capacity eviction.
    #[must_use]
    pub const fn is_lossless_admission(&self) -> bool {
        self.lossless_admission
    }

    /// Returns the local delivery state.
    #[must_use]
    pub const fn state(&self) -> CloudLinkDeliveryState {
        self.state
    }

    /// Returns the most recent session on which the record was offered.
    #[must_use]
    pub const fn offered_session(&self) -> Option<&CloudLinkSessionBinding> {
        self.offered_session.as_ref()
    }

    /// Updates the offer state for a port implementation.
    pub fn set_offered(&mut self, session: CloudLinkSessionBinding) {
        self.state = CloudLinkDeliveryState::Offered;
        self.offered_session = Some(session);
    }

    /// Updates the transport-published state for a port implementation.
    pub fn set_transport_published(&mut self, session: CloudLinkSessionBinding) {
        self.state = CloudLinkDeliveryState::TransportPublished;
        self.offered_session = Some(session);
    }
}

mod shared_payload {
    use std::sync::Arc;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(super) fn serialize<SerializerT>(
        payload: &Arc<[u8]>,
        serializer: SerializerT,
    ) -> Result<SerializerT::Ok, SerializerT::Error>
    where
        SerializerT: Serializer,
    {
        payload.as_ref().serialize(serializer)
    }

    pub(super) fn deserialize<'de, DeserializerT>(
        deserializer: DeserializerT,
    ) -> Result<Arc<[u8]>, DeserializerT::Error>
    where
        DeserializerT: Deserializer<'de>,
    {
        Vec::<u8>::deserialize(deserializer).map(Arc::from)
    }
}

#[cfg(test)]
mod cloudlink_record_tests {
    use super::*;

    #[test]
    fn delivery_state_clones_share_the_immutable_payload_allocation() {
        let original = CloudLinkRecord::from_enqueue(
            CloudLinkRecordIdentity::new("business", 1, 1),
            CloudLinkEnqueue::new(
                CloudLinkMessageKind::TelemetryBatch,
                "batch-1",
                format!("sha256:{}", "a".repeat(64)),
                vec![7_u8; 256 * 1024],
                TimestampMs::new(1),
                None,
            ),
        );
        let original_payload = original.payload.as_ptr();
        let mut delivery_update = original.clone();
        delivery_update.set_offered(CloudLinkSessionBinding::new("session-1", 1));

        assert_eq!(delivery_update.payload.as_ptr(), original_payload);
        assert_eq!(original.state(), CloudLinkDeliveryState::Queued);
        assert_eq!(delivery_update.state(), CloudLinkDeliveryState::Offered);
    }
}

/// Application-level durable acknowledgement through one contiguous position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudLinkDurableAck {
    session: CloudLinkSessionBinding,
    stream_id: String,
    stream_epoch: u64,
    acknowledged_position: u64,
    batch_id: String,
    digest: String,
    receipt_id: String,
}

impl CloudLinkDurableAck {
    /// Creates a receipt to be validated by the spool.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        session: CloudLinkSessionBinding,
        stream_id: impl Into<String>,
        stream_epoch: u64,
        acknowledged_position: u64,
        batch_id: impl Into<String>,
        digest: impl Into<String>,
        receipt_id: impl Into<String>,
    ) -> Self {
        Self {
            session,
            stream_id: stream_id.into(),
            stream_epoch,
            acknowledged_position,
            batch_id: batch_id.into(),
            digest: digest.into(),
            receipt_id: receipt_id.into(),
        }
    }

    /// Returns the acknowledging session.
    #[must_use]
    pub const fn session(&self) -> &CloudLinkSessionBinding {
        &self.session
    }

    /// Returns the logical stream.
    #[must_use]
    pub fn stream_id(&self) -> &str {
        &self.stream_id
    }

    /// Returns the acknowledged stream epoch.
    #[must_use]
    pub const fn stream_epoch(&self) -> u64 {
        self.stream_epoch
    }

    /// Returns the contiguous durable position.
    #[must_use]
    pub const fn acknowledged_position(&self) -> u64 {
        self.acknowledged_position
    }

    /// Returns the terminal batch identity at that position.
    #[must_use]
    pub fn batch_id(&self) -> &str {
        &self.batch_id
    }

    /// Returns the terminal business digest at that position.
    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// Returns the durable cloud receipt identity.
    #[must_use]
    pub fn receipt_id(&self) -> &str {
        &self.receipt_id
    }
}

/// Outcome of applying a valid durable acknowledgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurableAckOutcome {
    /// Records through the acknowledged position were removed.
    Applied {
        /// Number of records removed from this retained window.
        removed: usize,
    },
    /// The exact previously applied receipt was replayed.
    Duplicate,
}

/// Durable evidence that a retained position range was lost.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudLinkDataLossEvidence {
    stream_id: String,
    stream_epoch: u64,
    first_lost_position: u64,
    last_lost_position: u64,
    earliest_retained_position: u64,
    reason: String,
    recorded_at_ms: u64,
}

impl CloudLinkDataLossEvidence {
    /// Creates explicit loss evidence for a port implementation.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        stream_id: impl Into<String>,
        stream_epoch: u64,
        first_lost_position: u64,
        last_lost_position: u64,
        earliest_retained_position: u64,
        reason: impl Into<String>,
        recorded_at: TimestampMs,
    ) -> Self {
        Self {
            stream_id: stream_id.into(),
            stream_epoch,
            first_lost_position,
            last_lost_position,
            earliest_retained_position,
            reason: reason.into(),
            recorded_at_ms: recorded_at.get(),
        }
    }

    /// Returns the stream identifier.
    #[must_use]
    pub fn stream_id(&self) -> &str {
        &self.stream_id
    }

    /// Returns the affected stream epoch.
    #[must_use]
    pub const fn stream_epoch(&self) -> u64 {
        self.stream_epoch
    }

    /// Returns the first unavailable position.
    #[must_use]
    pub const fn first_lost_position(&self) -> u64 {
        self.first_lost_position
    }

    /// Returns the last unavailable position.
    #[must_use]
    pub const fn last_lost_position(&self) -> u64 {
        self.last_lost_position
    }

    /// Returns the first position still available for replay.
    #[must_use]
    pub const fn earliest_retained_position(&self) -> u64 {
        self.earliest_retained_position
    }

    /// Returns the stable loss reason.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }

    /// Returns when the loss was recorded.
    #[must_use]
    pub const fn recorded_at(&self) -> TimestampMs {
        TimestampMs::new(self.recorded_at_ms)
    }

    /// Extends a contiguous overflow range while retaining the first timestamp.
    pub fn extend_overflow(&mut self, last_lost: u64, earliest_retained: u64) {
        self.last_lost_position = last_lost;
        self.earliest_retained_position = earliest_retained;
    }
}

/// Replay result. Data loss is returned instead of silently skipping a gap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudLinkReplayWindow {
    records: Vec<CloudLinkRecord>,
    data_loss: Option<CloudLinkDataLossEvidence>,
}

impl CloudLinkReplayWindow {
    /// Creates a replay window for a port implementation.
    #[must_use]
    pub fn new(
        records: Vec<CloudLinkRecord>,
        data_loss: Option<CloudLinkDataLossEvidence>,
    ) -> Self {
        Self { records, data_loss }
    }

    /// Returns retained records in ascending position order.
    #[must_use]
    pub fn records(&self) -> &[CloudLinkRecord] {
        &self.records
    }

    /// Returns durable evidence when the requested cursor predates retention.
    #[must_use]
    pub const fn data_loss(&self) -> Option<&CloudLinkDataLossEvidence> {
        self.data_loss.as_ref()
    }
}

/// Current durable stream metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudLinkSpoolStatus {
    stream_id: String,
    stream_epoch: u64,
    next_position: u64,
    earliest_retained_position: u64,
    last_acknowledged_position: u64,
    pending_records: usize,
    ordinary_pending_records: usize,
    system_pending_records: usize,
    record_capacity: usize,
    acknowledged_receipts: usize,
    pending_receipt_reservations: usize,
    acknowledged_receipt_capacity: usize,
    current_live_bytes: u64,
    max_live_bytes: u64,
    ordinary_max_live_bytes: u64,
    journal_bytes: u64,
    max_journal_bytes: u64,
    quota_rejections: u64,
    last_ack: Option<CloudLinkDurableAck>,
    data_loss: Option<CloudLinkDataLossEvidence>,
}

impl CloudLinkSpoolStatus {
    /// Creates status metadata for a port implementation.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        stream_id: impl Into<String>,
        stream_epoch: u64,
        next_position: u64,
        earliest_retained_position: u64,
        last_acknowledged_position: u64,
        pending_records: usize,
        ordinary_pending_records: usize,
        system_pending_records: usize,
        record_capacity: usize,
        acknowledged_receipts: usize,
        pending_receipt_reservations: usize,
        acknowledged_receipt_capacity: usize,
        current_live_bytes: u64,
        max_live_bytes: u64,
        ordinary_max_live_bytes: u64,
        journal_bytes: u64,
        max_journal_bytes: u64,
        quota_rejections: u64,
        last_ack: Option<CloudLinkDurableAck>,
        data_loss: Option<CloudLinkDataLossEvidence>,
    ) -> Self {
        Self {
            stream_id: stream_id.into(),
            stream_epoch,
            next_position,
            earliest_retained_position,
            last_acknowledged_position,
            pending_records,
            ordinary_pending_records,
            system_pending_records,
            record_capacity,
            acknowledged_receipts,
            pending_receipt_reservations,
            acknowledged_receipt_capacity,
            current_live_bytes,
            max_live_bytes,
            ordinary_max_live_bytes,
            journal_bytes,
            max_journal_bytes,
            quota_rejections,
            last_ack,
            data_loss,
        }
    }

    /// Returns the stream identifier.
    #[must_use]
    pub fn stream_id(&self) -> &str {
        &self.stream_id
    }

    /// Returns the durable stream epoch.
    #[must_use]
    pub const fn stream_epoch(&self) -> u64 {
        self.stream_epoch
    }

    /// Returns the next position to allocate.
    #[must_use]
    pub const fn next_position(&self) -> u64 {
        self.next_position
    }

    /// Returns the earliest retained position, or `next_position` when empty.
    #[must_use]
    pub const fn earliest_retained_position(&self) -> u64 {
        self.earliest_retained_position
    }

    /// Returns the last application-acknowledged position.
    #[must_use]
    pub const fn last_acknowledged_position(&self) -> u64 {
        self.last_acknowledged_position
    }

    /// Returns the number of retained records.
    #[must_use]
    pub const fn pending_records(&self) -> usize {
        self.pending_records
    }

    /// Returns pending records which consume the configured ordinary count bound.
    #[must_use]
    pub const fn ordinary_pending_records(&self) -> usize {
        self.ordinary_pending_records
    }

    /// Returns pending records occupying the dedicated data-loss system slot.
    #[must_use]
    pub const fn system_pending_records(&self) -> usize {
        self.system_pending_records
    }

    /// Returns the configured maximum number of pending records.
    #[must_use]
    pub const fn record_capacity(&self) -> usize {
        self.record_capacity
    }

    /// Returns the number of protected acknowledged business identities.
    #[must_use]
    pub const fn acknowledged_receipts(&self) -> usize {
        self.acknowledged_receipts
    }

    /// Returns the number of pending lossless records with reserved receipt slots.
    #[must_use]
    pub const fn pending_receipt_reservations(&self) -> usize {
        self.pending_receipt_reservations
    }

    /// Returns the configured protected-receipt bound.
    #[must_use]
    pub const fn acknowledged_receipt_capacity(&self) -> usize {
        self.acknowledged_receipt_capacity
    }

    /// Returns compacted bytes reserved by all pending records and retained receipts.
    /// Pending records include the largest legal persisted delivery-state projection.
    #[must_use]
    pub const fn current_live_bytes(&self) -> u64 {
        self.current_live_bytes
    }

    /// Returns the configured live-state byte ceiling.
    #[must_use]
    pub const fn max_live_bytes(&self) -> u64 {
        self.max_live_bytes
    }

    /// Returns the byte ceiling available to ordinary admissions after the
    /// dedicated data-loss report reserve is held back.
    #[must_use]
    pub const fn ordinary_max_live_bytes(&self) -> u64 {
        self.ordinary_max_live_bytes
    }

    /// Returns current physical journal bytes, or zero for a non-file adapter.
    #[must_use]
    pub const fn journal_bytes(&self) -> u64 {
        self.journal_bytes
    }

    /// Returns the configured physical journal ceiling, or zero when not applicable.
    #[must_use]
    pub const fn max_journal_bytes(&self) -> u64 {
        self.max_journal_bytes
    }

    /// Returns admission attempts rejected by a record, receipt, live-byte, or journal bound.
    #[must_use]
    pub const fn quota_rejections(&self) -> u64 {
        self.quota_rejections
    }

    /// Returns the last durable receipt retained for idempotent restart decisions.
    #[must_use]
    pub const fn last_ack(&self) -> Option<&CloudLinkDurableAck> {
        self.last_ack.as_ref()
    }

    /// Returns pending explicit data-loss evidence.
    #[must_use]
    pub const fn data_loss(&self) -> Option<&CloudLinkDataLossEvidence> {
        self.data_loss.as_ref()
    }
}

/// Stable reasons callers may use for recovery and security policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudLinkSpoolErrorReason {
    /// Configuration or record content is invalid.
    InvalidData,
    /// Lossless admission cannot evict an existing unacknowledged record.
    CapacityExceeded,
    /// An ACK or delivery event belongs to an old session.
    StaleSession,
    /// A stream identifier or epoch does not match this spool.
    WrongStream,
    /// Equal business identity was paired with conflicting content.
    ConflictingIdentity,
    /// The requested position is absent or beyond the allocated range.
    PositionGap,
    /// Rotation was attempted while unacknowledged records remain.
    PendingRecords,
    /// Persistent journal bytes are not safely recoverable.
    CorruptJournal,
    /// A filesystem or process-lock operation failed.
    Storage,
}

/// Typed CloudLink spool error.
#[derive(Debug)]
pub struct CloudLinkSpoolError {
    reason: CloudLinkSpoolErrorReason,
    message: String,
}

impl CloudLinkSpoolError {
    /// Creates a stable typed spool error.
    #[must_use]
    pub fn new(reason: CloudLinkSpoolErrorReason, message: impl Into<String>) -> Self {
        Self {
            reason,
            message: message.into(),
        }
    }

    /// Returns the stable failure reason.
    #[must_use]
    pub const fn reason(&self) -> Option<CloudLinkSpoolErrorReason> {
        Some(self.reason)
    }
}

impl fmt::Display for CloudLinkSpoolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for CloudLinkSpoolError {}

/// Dedicated durable CloudLink stream capability.
#[async_trait]
pub trait CloudLinkSpool: Send + Sync + 'static {
    /// Persists one sealed business fact and allocates its stream position.
    ///
    /// Repeating an extant batch ID with the same digest is idempotent. The same
    /// batch ID with another digest is a conflict.
    async fn enqueue(
        &self,
        input: CloudLinkEnqueue,
    ) -> Result<CloudLinkRecord, CloudLinkSpoolError>;

    /// Persists an event without ever evicting another unacknowledged record.
    ///
    /// With [`CloudLinkReceiptRetention::RetainForIdempotency`], exact retries
    /// return the original identity and set `duplicate`, including retries after
    /// a durable Cloud ACK. `DiscardAfterAck` preserves lossless pending
    /// admission without consuming the post-ACK identity ledger. Conflicting
    /// reuse of a retained batch identity fails closed.
    async fn admit_lossless(
        &self,
        input: CloudLinkEnqueue,
        receipt_retention: CloudLinkReceiptRetention,
    ) -> Result<CloudLinkAdmission, CloudLinkSpoolError>;

    /// Persists the report for the current data-loss evidence through a reserved
    /// system slot, so a full ordinary record window cannot deadlock reporting.
    ///
    /// Implementations durably bind the report identity to the supplied evidence.
    /// A cumulative ACK clears the evidence only when that exact range is still
    /// current; an evidence range extended after admission remains reportable.
    async fn admit_data_loss(
        &self,
        input: CloudLinkEnqueue,
        evidence: &CloudLinkDataLossEvidence,
    ) -> Result<CloudLinkAdmission, CloudLinkSpoolError>;

    /// Returns records beginning at one server-authoritative requested position.
    async fn replay_from(
        &self,
        requested_position: u64,
        limit: usize,
    ) -> Result<CloudLinkReplayWindow, CloudLinkSpoolError>;

    /// Records that a fact was offered on a verified session.
    async fn mark_offered(
        &self,
        identity: &CloudLinkRecordIdentity,
        session: &CloudLinkSessionBinding,
    ) -> Result<(), CloudLinkSpoolError>;

    /// Records transport publication evidence. This is never removal authority.
    async fn mark_transport_published(
        &self,
        identity: &CloudLinkRecordIdentity,
        session: &CloudLinkSessionBinding,
    ) -> Result<(), CloudLinkSpoolError>;

    /// Applies a validated cloud application receipt and removes covered records.
    async fn acknowledge(
        &self,
        ack: &CloudLinkDurableAck,
    ) -> Result<DurableAckOutcome, CloudLinkSpoolError>;

    /// Returns current durable stream state.
    async fn status(&self) -> Result<CloudLinkSpoolStatus, CloudLinkSpoolError>;

    /// Rotates to the next stream epoch when no record is pending.
    async fn rotate_stream_epoch(&self) -> Result<u64, CloudLinkSpoolError>;
}

/// Transport-neutral logical route used by a concrete MQTT binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudLinkTransportRoute {
    /// Edge-to-cloud session establishment.
    SessionUp,
    /// Cloud-to-edge session acceptance.
    SessionDown,
    /// Edge-to-cloud heartbeat.
    HeartbeatUp,
    /// Edge-to-cloud Runtime Manifest report.
    ManifestUp,
    /// Edge-to-cloud point telemetry.
    TelemetryUp,
    /// Edge-to-cloud durable alarm transitions.
    AlarmUp,
    /// Edge-to-cloud complete delegated-integration topology.
    IntegrationTopologyUp,
    /// Edge-to-cloud delegated-integration observations.
    IntegrationObservationsUp,
    /// Edge-to-cloud data-loss evidence.
    DataLossUp,
    /// Cloud-to-edge durable or heartbeat ACK.
    AckDown,
    /// Cloud-to-edge replay request.
    ReplayDown,
}

/// One bounded transport message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudLinkTransportMessage {
    route: CloudLinkTransportRoute,
    payload: Vec<u8>,
    delivery: Option<CloudLinkRecordIdentity>,
}

impl CloudLinkTransportMessage {
    /// Creates a non-retained QoS-1 CloudLink transport message.
    #[must_use]
    pub fn new(
        route: CloudLinkTransportRoute,
        payload: impl Into<Vec<u8>>,
        delivery: Option<CloudLinkRecordIdentity>,
    ) -> Self {
        Self {
            route,
            payload: payload.into(),
            delivery,
        }
    }

    /// Returns the logical route.
    #[must_use]
    pub const fn route(&self) -> CloudLinkTransportRoute {
        self.route
    }

    /// Returns the encoded message bytes.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// Returns the associated durable record, when applicable.
    #[must_use]
    pub const fn delivery(&self) -> Option<&CloudLinkRecordIdentity> {
        self.delivery.as_ref()
    }
}

/// Event emitted by a CloudLink transport binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudLinkTransportEvent {
    /// The transport connection is ready.
    Connected,
    /// The transport connection ended; local edge behavior continues.
    Disconnected,
    /// MQTT QoS-1 PUBACK or equivalent transport evidence was observed.
    TransportPublished(CloudLinkRecordIdentity),
    /// A bounded frame arrived on an allowed downlink route.
    Inbound(CloudLinkTransportMessage),
}

/// Broker/vendor-neutral CloudLink transport capability.
#[async_trait]
pub trait CloudLinkTransport: Send + Sync + 'static {
    /// Sends one bounded message. Success means accepted by the transport queue.
    async fn send(&self, message: CloudLinkTransportMessage) -> crate::PortResult<()>;

    /// Waits for the next connection, PUBACK, or inbound-frame event.
    async fn receive(&self) -> crate::PortResult<CloudLinkTransportEvent>;
}
