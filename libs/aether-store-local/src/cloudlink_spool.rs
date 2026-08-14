//! In-memory CloudLink spool and shared durable-state transition logic.

use std::collections::BTreeMap;
use std::sync::Mutex;

use aether_ports::{
    CloudLinkAdmission, CloudLinkDataLossEvidence, CloudLinkDurableAck, CloudLinkEnqueue,
    CloudLinkMessageKind, CloudLinkReceiptRetention, CloudLinkRecord, CloudLinkRecordIdentity,
    CloudLinkReplayWindow, CloudLinkSessionBinding, CloudLinkSpool, CloudLinkSpoolError,
    CloudLinkSpoolErrorReason, CloudLinkSpoolStatus, DurableAckOutcome,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
pub(crate) const MAX_SPOOL_PAYLOAD_BYTES: usize = 256 * 1024;
const MAX_DATA_LOSS_PAYLOAD_BYTES: usize = 4 * 1024;
pub const CLOUDLINK_DATA_LOSS_RESERVED_LIVE_BYTES: u64 = 32 * 1024;
const MAX_SPOOL_RECORDS: usize = 65_536;
const DEFAULT_ACKNOWLEDGED_RECEIPT_CAPACITY: usize = 100_000;
pub(crate) const MAX_ACKNOWLEDGED_RECEIPT_CAPACITY: usize = 1_000_000;
pub const DEFAULT_CLOUDLINK_SPOOL_MAX_LIVE_BYTES: u64 = 256 * 1024 * 1024;
pub const MIN_CLOUDLINK_SPOOL_MAX_LIVE_BYTES: u64 = 64 * 1024;
pub const MAX_CLOUDLINK_SPOOL_MAX_LIVE_BYTES: u64 = 16 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CloudLinkAcknowledgedReceipt {
    pub(crate) identity: CloudLinkRecordIdentity,
    pub(crate) message_kind: CloudLinkMessageKind,
    pub(crate) batch_id: String,
    pub(crate) digest: String,
}

impl CloudLinkAcknowledgedReceipt {
    fn from_record(record: &CloudLinkRecord) -> Self {
        Self {
            identity: record.identity().clone(),
            message_kind: record.message_kind(),
            batch_id: record.batch_id().to_owned(),
            digest: record.digest().to_owned(),
        }
    }

    pub(crate) fn matches(&self, input: &CloudLinkEnqueue) -> bool {
        self.message_kind == input.message_kind() && self.digest == input.digest()
    }
}

#[derive(Serialize)]
#[serde(tag = "operation", rename_all = "kebab-case")]
enum CompactedLiveEntry<'a> {
    Receipt {
        receipt: &'a CloudLinkAcknowledgedReceipt,
    },
    Record {
        record: &'a CloudLinkRecord,
        data_loss_report: Option<&'a CloudLinkDataLossEvidence>,
    },
}

fn encoded_live_entry_bytes(entry: &CompactedLiveEntry<'_>) -> Result<u64, CloudLinkSpoolError> {
    let payload = serde_json::to_vec(entry).map_err(|source| {
        error(
            CloudLinkSpoolErrorReason::Storage,
            format!("cannot account CloudLink compacted live entry: {source}"),
        )
    })?;
    u64::try_from(payload.len())
        .ok()
        .and_then(|bytes| bytes.checked_add(8))
        .ok_or_else(|| {
            error(
                CloudLinkSpoolErrorReason::Storage,
                "CloudLink compacted live-byte accounting overflow",
            )
        })
}

pub(crate) fn record_live_bytes(
    record: &CloudLinkRecord,
    data_loss_report: Option<&CloudLinkDataLossEvidence>,
) -> Result<u64, CloudLinkSpoolError> {
    // Admission reserves the largest legal persisted delivery projection. This
    // keeps later Offered/TransportPublished mutations from consuming bytes
    // which were never admitted, while still accounting payload and metadata.
    let mut reserved = record.clone();
    reserved.set_transport_published(CloudLinkSessionBinding::new("s".repeat(128), u64::MAX));
    encoded_live_entry_bytes(&CompactedLiveEntry::Record {
        record: &reserved,
        data_loss_report,
    })
}

pub(crate) fn receipt_live_bytes(
    receipt: &CloudLinkAcknowledgedReceipt,
) -> Result<u64, CloudLinkSpoolError> {
    encoded_live_entry_bytes(&CompactedLiveEntry::Receipt { receipt })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CloudLinkSpoolState {
    pub(crate) stream_id: String,
    pub(crate) stream_epoch: u64,
    pub(crate) next_position: u64,
    pub(crate) capacity: usize,
    pub(crate) acknowledged_receipt_capacity: usize,
    pub(crate) max_live_bytes: u64,
    pub(crate) current_live_bytes: u64,
    pub(crate) records: BTreeMap<u64, CloudLinkRecord>,
    pub(crate) last_ack: Option<CloudLinkDurableAck>,
    pub(crate) last_acknowledged_position: u64,
    pub(crate) data_loss: Option<CloudLinkDataLossEvidence>,
    pub(crate) data_loss_reports: BTreeMap<u64, CloudLinkDataLossEvidence>,
    pub(crate) acknowledged_receipts: BTreeMap<String, CloudLinkAcknowledgedReceipt>,
    pub(crate) quota_rejections: u64,
}

impl CloudLinkSpoolState {
    #[cfg(test)]
    pub(crate) fn new(
        stream_id: impl Into<String>,
        capacity: usize,
        acknowledged_receipt_capacity: usize,
    ) -> Result<Self, CloudLinkSpoolError> {
        Self::new_with_limits(
            stream_id,
            capacity,
            acknowledged_receipt_capacity,
            DEFAULT_CLOUDLINK_SPOOL_MAX_LIVE_BYTES,
        )
    }

    pub(crate) fn new_with_limits(
        stream_id: impl Into<String>,
        capacity: usize,
        acknowledged_receipt_capacity: usize,
        max_live_bytes: u64,
    ) -> Result<Self, CloudLinkSpoolError> {
        let stream_id = stream_id.into();
        validate_stream_id(&stream_id)?;
        if capacity == 0 || capacity > MAX_SPOOL_RECORDS {
            return Err(error(
                CloudLinkSpoolErrorReason::InvalidData,
                "CloudLink spool capacity must be between 1 and 65536 records",
            ));
        }
        if acknowledged_receipt_capacity == 0
            || acknowledged_receipt_capacity > MAX_ACKNOWLEDGED_RECEIPT_CAPACITY
        {
            return Err(error(
                CloudLinkSpoolErrorReason::InvalidData,
                "CloudLink acknowledged-receipt capacity must be between 1 and 1000000 records",
            ));
        }
        if max_live_bytes <= CLOUDLINK_DATA_LOSS_RESERVED_LIVE_BYTES {
            return Err(error(
                CloudLinkSpoolErrorReason::InvalidData,
                format!(
                    "CloudLink spool live-byte limit must exceed the {}-byte data-loss reserve",
                    CLOUDLINK_DATA_LOSS_RESERVED_LIVE_BYTES
                ),
            ));
        }
        Ok(Self {
            stream_id,
            stream_epoch: 1,
            next_position: 1,
            capacity,
            acknowledged_receipt_capacity,
            max_live_bytes,
            current_live_bytes: 0,
            records: BTreeMap::new(),
            last_ack: None,
            last_acknowledged_position: 0,
            data_loss: None,
            data_loss_reports: BTreeMap::new(),
            acknowledged_receipts: BTreeMap::new(),
            quota_rejections: 0,
        })
    }

    pub(crate) fn validate_open(
        &mut self,
        stream_id: &str,
        capacity: usize,
        acknowledged_receipt_capacity: usize,
        max_live_bytes: u64,
    ) -> Result<bool, CloudLinkSpoolError> {
        validate_stream_id(stream_id)?;
        if self.stream_id != stream_id {
            return Err(error(
                CloudLinkSpoolErrorReason::WrongStream,
                format!(
                    "CloudLink spool belongs to stream {:?}, not {:?}",
                    self.stream_id, stream_id
                ),
            ));
        }
        let ordinary_records = self
            .records
            .len()
            .saturating_sub(self.data_loss_reports.len());
        if capacity == 0 || capacity > MAX_SPOOL_RECORDS || capacity < ordinary_records {
            return Err(error(
                CloudLinkSpoolErrorReason::InvalidData,
                "configured CloudLink spool capacity is smaller than retained state",
            ));
        }
        let reserved_receipts = self.pending_receipt_reservations();
        if acknowledged_receipt_capacity == 0
            || acknowledged_receipt_capacity > MAX_ACKNOWLEDGED_RECEIPT_CAPACITY
            || acknowledged_receipt_capacity
                < self
                    .acknowledged_receipts
                    .len()
                    .saturating_add(reserved_receipts)
        {
            return Err(error(
                CloudLinkSpoolErrorReason::InvalidData,
                "configured CloudLink acknowledged-receipt capacity is smaller than protected state",
            ));
        }
        let (ordinary_live_bytes, system_live_bytes) = self.live_byte_partitions()?;
        if max_live_bytes <= CLOUDLINK_DATA_LOSS_RESERVED_LIVE_BYTES
            || ordinary_live_bytes
                > max_live_bytes.saturating_sub(CLOUDLINK_DATA_LOSS_RESERVED_LIVE_BYTES)
            || system_live_bytes > CLOUDLINK_DATA_LOSS_RESERVED_LIVE_BYTES
        {
            return Err(error(
                CloudLinkSpoolErrorReason::InvalidData,
                "configured CloudLink live-byte limit is smaller than retained state or its data-loss reserve",
            ));
        }
        let changed = self.capacity != capacity
            || self.acknowledged_receipt_capacity != acknowledged_receipt_capacity
            || self.max_live_bytes != max_live_bytes;
        self.capacity = capacity;
        self.acknowledged_receipt_capacity = acknowledged_receipt_capacity;
        self.max_live_bytes = max_live_bytes;
        Ok(changed)
    }

    pub(crate) fn enqueue(
        &mut self,
        input: CloudLinkEnqueue,
    ) -> Result<CloudLinkRecord, CloudLinkSpoolError> {
        let admission = self.enqueue_with_policy(input, true, false, None)?;
        admission.pending_record().cloned().ok_or_else(|| {
            error(
                CloudLinkSpoolErrorReason::ConflictingIdentity,
                "CloudLink batch identity was already durably acknowledged",
            )
        })
    }

    pub(crate) fn admit_lossless(
        &mut self,
        input: CloudLinkEnqueue,
        receipt_retention: CloudLinkReceiptRetention,
    ) -> Result<CloudLinkAdmission, CloudLinkSpoolError> {
        self.enqueue_with_policy(
            input,
            false,
            receipt_retention == CloudLinkReceiptRetention::RetainForIdempotency,
            None,
        )
    }

    pub(crate) fn admit_data_loss(
        &mut self,
        input: CloudLinkEnqueue,
        evidence: &CloudLinkDataLossEvidence,
    ) -> Result<CloudLinkAdmission, CloudLinkSpoolError> {
        validate_enqueue(&input)?;
        if input.message_kind() != CloudLinkMessageKind::DataLoss {
            return Err(error(
                CloudLinkSpoolErrorReason::InvalidData,
                "reserved data-loss admission accepts only DataLoss records",
            ));
        }
        if input.payload().len() > MAX_DATA_LOSS_PAYLOAD_BYTES {
            return Err(error(
                CloudLinkSpoolErrorReason::InvalidData,
                "CloudLink data-loss payload exceeds its 4 KiB system bound",
            ));
        }
        if self.data_loss.as_ref() != Some(evidence) {
            return Err(error(
                CloudLinkSpoolErrorReason::ConflictingIdentity,
                "data-loss report does not match the current durable evidence",
            ));
        }
        if let Some((position, pending_evidence)) = self.data_loss_reports.first_key_value() {
            let record = self.records.get(position).ok_or_else(|| {
                error(
                    CloudLinkSpoolErrorReason::Storage,
                    "pending data-loss report association has no record",
                )
            })?;
            if pending_evidence == evidence
                && record.batch_id() == input.batch_id()
                && record.digest() == input.digest()
                && record.payload() == input.payload()
            {
                return Ok(CloudLinkAdmission::pending(record.clone(), true));
            }
            self.quota_rejections = self.quota_rejections.saturating_add(1);
            return Err(error(
                CloudLinkSpoolErrorReason::CapacityExceeded,
                "a previous data-loss range must be acknowledged before the extended range is admitted",
            ));
        }
        self.enqueue_with_policy(input, false, false, Some(evidence.clone()))
    }

    fn enqueue_with_policy(
        &mut self,
        input: CloudLinkEnqueue,
        allow_eviction: bool,
        retain_acknowledged_receipt: bool,
        data_loss_report: Option<CloudLinkDataLossEvidence>,
    ) -> Result<CloudLinkAdmission, CloudLinkSpoolError> {
        validate_enqueue(&input)?;

        if let Some(existing) = self
            .records
            .values()
            .find(|record| record.batch_id() == input.batch_id())
        {
            if existing.digest() == input.digest()
                && existing.message_kind() == input.message_kind()
                && existing.payload() == input.payload()
            {
                if !allow_eviction && !existing.is_lossless_admission() {
                    return Err(error(
                        CloudLinkSpoolErrorReason::ConflictingIdentity,
                        "CloudLink batch identity was first admitted with an evictable pending policy",
                    ));
                }
                if retain_acknowledged_receipt && !existing.retains_acknowledged_receipt() {
                    return Err(error(
                        CloudLinkSpoolErrorReason::ConflictingIdentity,
                        "CloudLink batch identity was first admitted without lossless receipt protection",
                    ));
                }
                return Ok(CloudLinkAdmission::pending(existing.clone(), true));
            }
            return Err(error(
                CloudLinkSpoolErrorReason::ConflictingIdentity,
                format!(
                    "CloudLink batch identity {:?} was reused with different business content",
                    input.batch_id()
                ),
            ));
        }

        if let Some(receipt) = self.acknowledged_receipts.get(input.batch_id()) {
            if receipt.matches(&input) {
                if retain_acknowledged_receipt {
                    return Ok(CloudLinkAdmission::acknowledged(receipt.identity.clone()));
                }
                return Err(error(
                    CloudLinkSpoolErrorReason::ConflictingIdentity,
                    "CloudLink batch identity was already durably acknowledged",
                ));
            }
            return Err(error(
                CloudLinkSpoolErrorReason::ConflictingIdentity,
                format!(
                    "CloudLink batch identity {:?} was reused with different business content",
                    input.batch_id()
                ),
            ));
        }

        if retain_acknowledged_receipt
            && self.protected_receipt_slots() >= self.acknowledged_receipt_capacity
        {
            self.quota_rejections = self.quota_rejections.saturating_add(1);
            return Err(error(
                CloudLinkSpoolErrorReason::CapacityExceeded,
                "CloudLink acknowledged-receipt ledger is full; refusing unsafe identity reuse",
            ));
        }

        let ordinary_records = self
            .records
            .len()
            .saturating_sub(self.data_loss_reports.len());
        if data_loss_report.is_none() && !allow_eviction && ordinary_records >= self.capacity {
            self.quota_rejections = self.quota_rejections.saturating_add(1);
            return Err(error(
                CloudLinkSpoolErrorReason::CapacityExceeded,
                "CloudLink lossless admission cannot evict an unacknowledged record",
            ));
        }

        let evicted =
            if data_loss_report.is_none() && allow_eviction && ordinary_records >= self.capacity {
                let position = self.records.keys().next().copied().ok_or_else(|| {
                    error(
                        CloudLinkSpoolErrorReason::Storage,
                        "CloudLink spool capacity accounting is inconsistent",
                    )
                })?;
                if self
                    .records
                    .get(&position)
                    .is_some_and(CloudLinkRecord::is_lossless_admission)
                    || self.data_loss_reports.contains_key(&position)
                {
                    self.quota_rejections = self.quota_rejections.saturating_add(1);
                    return Err(error(
                        CloudLinkSpoolErrorReason::CapacityExceeded,
                        "CloudLink spool cannot evict the earliest pending lossless record",
                    ));
                }
                Some(position)
            } else {
                None
            };

        let position = self.next_position;
        let next_position = self.next_position.checked_add(1).ok_or_else(|| {
            error(
                CloudLinkSpoolErrorReason::Storage,
                "CloudLink stream position exhausted uint64",
            )
        })?;
        let recorded_at = input.created_at();
        let identity =
            CloudLinkRecordIdentity::new(self.stream_id.clone(), self.stream_epoch, position);
        let record = if retain_acknowledged_receipt {
            CloudLinkRecord::from_lossless_enqueue(
                identity,
                input,
                CloudLinkReceiptRetention::RetainForIdempotency,
            )
        } else if !allow_eviction || data_loss_report.is_some() {
            CloudLinkRecord::from_lossless_enqueue(
                identity,
                input,
                CloudLinkReceiptRetention::DiscardAfterAck,
            )
        } else {
            CloudLinkRecord::from_enqueue(identity, input)
        };
        let added_bytes = record_live_bytes(&record, data_loss_report.as_ref())?;
        let evicted_bytes = match evicted.and_then(|position| self.records.get(&position)) {
            Some(record) => record_live_bytes(record, None)?,
            None => 0,
        };
        let (ordinary_live_bytes, system_live_bytes) = self.live_byte_partitions()?;
        let (projected_ordinary_bytes, projected_system_bytes) = if data_loss_report.is_some() {
            (
                ordinary_live_bytes,
                system_live_bytes.checked_add(added_bytes).ok_or_else(|| {
                    error(
                        CloudLinkSpoolErrorReason::Storage,
                        "CloudLink system live-byte accounting overflow",
                    )
                })?,
            )
        } else {
            (
                ordinary_live_bytes
                    .checked_sub(evicted_bytes)
                    .and_then(|bytes| bytes.checked_add(added_bytes))
                    .ok_or_else(|| {
                        error(
                            CloudLinkSpoolErrorReason::Storage,
                            "CloudLink ordinary live-byte accounting overflow",
                        )
                    })?,
                system_live_bytes,
            )
        };
        let ordinary_limit = self.max_live_bytes - CLOUDLINK_DATA_LOSS_RESERVED_LIVE_BYTES;
        if projected_ordinary_bytes > ordinary_limit
            || projected_system_bytes > CLOUDLINK_DATA_LOSS_RESERVED_LIVE_BYTES
        {
            self.quota_rejections = self.quota_rejections.saturating_add(1);
            return Err(error(
                CloudLinkSpoolErrorReason::CapacityExceeded,
                format!(
                    "CloudLink live-byte partition reached (ordinary {projected_ordinary_bytes}/{ordinary_limit}, system {projected_system_bytes}/{CLOUDLINK_DATA_LOSS_RESERVED_LIVE_BYTES})"
                ),
            ));
        }
        let projected_live_bytes = projected_ordinary_bytes
            .checked_add(projected_system_bytes)
            .ok_or_else(|| {
                error(
                    CloudLinkSpoolErrorReason::Storage,
                    "CloudLink total live-byte accounting overflow",
                )
            })?;

        if let (Some(lost_position), Some(evidence)) = (evicted, self.data_loss.as_ref())
            && (evidence.stream_epoch() != self.stream_epoch
                || evidence.last_lost_position().checked_add(1) != Some(lost_position))
        {
            self.quota_rejections = self.quota_rejections.saturating_add(1);
            return Err(error(
                CloudLinkSpoolErrorReason::CapacityExceeded,
                "CloudLink spool cannot replace unresolved data-loss evidence with a non-contiguous range",
            ));
        }

        if let Some(position) = evicted {
            self.records.remove(&position);
        }
        self.next_position = next_position;
        self.records.insert(position, record.clone());
        if let Some(evidence) = data_loss_report {
            self.data_loss_reports.insert(position, evidence);
        }
        self.current_live_bytes = projected_live_bytes;

        if let Some(lost_position) = evicted {
            let earliest_retained = self
                .records
                .keys()
                .next()
                .copied()
                .unwrap_or(self.next_position);
            match self.data_loss.as_mut() {
                Some(evidence) => {
                    evidence.extend_overflow(lost_position, earliest_retained);
                },
                None => {
                    self.data_loss = Some(CloudLinkDataLossEvidence::new(
                        self.stream_id.clone(),
                        self.stream_epoch,
                        lost_position,
                        lost_position,
                        earliest_retained,
                        "capacity-overflow",
                        recorded_at,
                    ));
                },
            }
        }

        Ok(CloudLinkAdmission::pending(record, false))
    }

    pub(crate) fn replay_from(
        &self,
        requested_position: u64,
        limit: usize,
    ) -> Result<CloudLinkReplayWindow, CloudLinkSpoolError> {
        if requested_position == 0 || limit == 0 {
            return Err(error(
                CloudLinkSpoolErrorReason::InvalidData,
                "CloudLink replay position and limit must be greater than zero",
            ));
        }
        if requested_position > self.next_position {
            return Err(error(
                CloudLinkSpoolErrorReason::PositionGap,
                format!(
                    "CloudLink replay position {requested_position} is beyond next position {}",
                    self.next_position
                ),
            ));
        }
        let earliest = self
            .records
            .keys()
            .next()
            .copied()
            .unwrap_or(self.next_position);
        if requested_position < earliest {
            if let Some(evidence) = &self.data_loss
                && requested_position >= evidence.first_lost_position()
                && requested_position <= evidence.last_lost_position()
            {
                return Ok(CloudLinkReplayWindow::new(
                    Vec::new(),
                    Some(evidence.clone()),
                ));
            }
            return Err(error(
                CloudLinkSpoolErrorReason::PositionGap,
                format!(
                    "CloudLink replay position {requested_position} precedes retained position {earliest} without matching loss evidence"
                ),
            ));
        }

        let records = self
            .records
            .range(requested_position..)
            .take(limit)
            .map(|(_, record)| record.clone())
            .collect();
        Ok(CloudLinkReplayWindow::new(records, None))
    }

    pub(crate) fn mark_offered(
        &mut self,
        identity: &CloudLinkRecordIdentity,
        session: &CloudLinkSessionBinding,
    ) -> Result<(), CloudLinkSpoolError> {
        validate_session(session)?;
        self.record_mut(identity)?.set_offered(session.clone());
        Ok(())
    }

    pub(crate) fn mark_transport_published(
        &mut self,
        identity: &CloudLinkRecordIdentity,
        session: &CloudLinkSessionBinding,
    ) -> Result<(), CloudLinkSpoolError> {
        validate_session(session)?;
        let record = self.record_mut(identity)?;
        if record.offered_session() != Some(session) {
            return Err(error(
                CloudLinkSpoolErrorReason::StaleSession,
                "transport publication belongs to a session other than the current offer",
            ));
        }
        record.set_transport_published(session.clone());
        Ok(())
    }

    pub(crate) fn acknowledge(
        &mut self,
        ack: &CloudLinkDurableAck,
    ) -> Result<DurableAckOutcome, CloudLinkSpoolError> {
        validate_session(ack.session())?;
        if self.last_ack.as_ref() == Some(ack) {
            return Ok(DurableAckOutcome::Duplicate);
        }
        if ack.stream_id() != self.stream_id || ack.stream_epoch() != self.stream_epoch {
            return Err(error(
                CloudLinkSpoolErrorReason::WrongStream,
                "durable ACK stream identity does not match the active spool",
            ));
        }
        if ack.acknowledged_position() <= self.last_acknowledged_position {
            return Err(error(
                CloudLinkSpoolErrorReason::ConflictingIdentity,
                "durable ACK reuses an acknowledged position with another receipt",
            ));
        }
        if !valid_identifier(ack.receipt_id(), 128) {
            return Err(error(
                CloudLinkSpoolErrorReason::InvalidData,
                "durable ACK receipt identity must be a bounded transport-safe identifier",
            ));
        }
        let record = self
            .records
            .get(&ack.acknowledged_position())
            .ok_or_else(|| {
                error(
                    CloudLinkSpoolErrorReason::PositionGap,
                    "durable ACK terminal position is not retained",
                )
            })?;
        if record.offered_session() != Some(ack.session()) {
            return Err(error(
                CloudLinkSpoolErrorReason::StaleSession,
                "durable ACK session does not match the record's current offer",
            ));
        }
        if record.batch_id() != ack.batch_id() || record.digest() != ack.digest() {
            return Err(error(
                CloudLinkSpoolErrorReason::ConflictingIdentity,
                "durable ACK batch identity or digest conflicts with retained content",
            ));
        }
        if self
            .records
            .range(..=ack.acknowledged_position())
            .any(|(_, record)| record.offered_session() != Some(ack.session()))
        {
            return Err(error(
                CloudLinkSpoolErrorReason::StaleSession,
                "durable cumulative ACK would remove a record not offered in its session",
            ));
        }

        let acknowledged_positions = self
            .records
            .range(..=ack.acknowledged_position())
            .map(|(position, _)| *position)
            .collect::<Vec<_>>();
        let retained_receipts = self
            .records
            .range(..=ack.acknowledged_position())
            .filter(|(_, record)| record.retains_acknowledged_receipt())
            .map(|(_, record)| CloudLinkAcknowledgedReceipt::from_record(record))
            .collect::<Vec<_>>();
        let additional_receipts = retained_receipts
            .iter()
            .filter(|receipt| !self.acknowledged_receipts.contains_key(&receipt.batch_id))
            .count();
        if self
            .acknowledged_receipts
            .len()
            .checked_add(additional_receipts)
            .is_none_or(|required| required > self.acknowledged_receipt_capacity)
        {
            self.quota_rejections = self.quota_rejections.saturating_add(1);
            return Err(error(
                CloudLinkSpoolErrorReason::CapacityExceeded,
                "CloudLink durable ACK would overflow the acknowledged-receipt ledger",
            ));
        }
        let removed_live_bytes = self
            .records
            .range(..=ack.acknowledged_position())
            .try_fold(0_u64, |total, (position, record)| {
                record_live_bytes(record, self.data_loss_reports.get(position)).and_then(|bytes| {
                    total.checked_add(bytes).ok_or_else(|| {
                        error(
                            CloudLinkSpoolErrorReason::Storage,
                            "CloudLink ACK live-byte accounting overflow",
                        )
                    })
                })
            })?;
        let added_receipt_bytes = retained_receipts.iter().try_fold(0_u64, |total, receipt| {
            receipt_live_bytes(receipt).and_then(|bytes| {
                total.checked_add(bytes).ok_or_else(|| {
                    error(
                        CloudLinkSpoolErrorReason::Storage,
                        "CloudLink receipt live-byte accounting overflow",
                    )
                })
            })
        })?;
        let next_live_bytes = self
            .current_live_bytes
            .checked_sub(removed_live_bytes)
            .and_then(|bytes| bytes.checked_add(added_receipt_bytes))
            .ok_or_else(|| {
                error(
                    CloudLinkSpoolErrorReason::Storage,
                    "CloudLink ACK live-byte accounting underflow",
                )
            })?;
        if next_live_bytes > self.current_live_bytes || next_live_bytes > self.max_live_bytes {
            return Err(error(
                CloudLinkSpoolErrorReason::Storage,
                "CloudLink ACK receipt reservation exceeds its pending record reservation",
            ));
        }
        let clears_current_data_loss = self.data_loss.as_ref().is_some_and(|current| {
            acknowledged_positions
                .iter()
                .any(|position| self.data_loss_reports.get(position) == Some(current))
        });

        for position in &acknowledged_positions {
            self.records.remove(position);
            self.data_loss_reports.remove(position);
        }
        for receipt in retained_receipts {
            self.acknowledged_receipts
                .insert(receipt.batch_id.clone(), receipt);
        }
        self.last_acknowledged_position = ack.acknowledged_position();
        self.last_ack = Some(ack.clone());
        self.current_live_bytes = next_live_bytes;
        if clears_current_data_loss {
            self.data_loss = None;
        }
        Ok(DurableAckOutcome::Applied {
            removed: acknowledged_positions.len(),
        })
    }

    pub(crate) fn status(&self) -> CloudLinkSpoolStatus {
        self.status_with_journal(0, 0, 0)
    }

    pub(crate) fn status_with_journal(
        &self,
        journal_bytes: u64,
        max_journal_bytes: u64,
        additional_quota_rejections: u64,
    ) -> CloudLinkSpoolStatus {
        let earliest = self
            .records
            .keys()
            .next()
            .copied()
            .unwrap_or(self.next_position);
        CloudLinkSpoolStatus::new(
            self.stream_id.clone(),
            self.stream_epoch,
            self.next_position,
            earliest,
            self.last_acknowledged_position,
            self.records.len(),
            self.records
                .len()
                .saturating_sub(self.data_loss_reports.len()),
            self.data_loss_reports.len(),
            self.capacity,
            self.acknowledged_receipts.len(),
            self.pending_receipt_reservations(),
            self.acknowledged_receipt_capacity,
            self.current_live_bytes,
            self.max_live_bytes,
            self.max_live_bytes - CLOUDLINK_DATA_LOSS_RESERVED_LIVE_BYTES,
            journal_bytes,
            max_journal_bytes,
            self.quota_rejections
                .saturating_add(additional_quota_rejections),
            self.last_ack.clone(),
            self.data_loss.clone(),
        )
    }

    pub(crate) fn rotate_stream_epoch(&mut self) -> Result<u64, CloudLinkSpoolError> {
        if !self.records.is_empty() || self.data_loss.is_some() {
            return Err(error(
                CloudLinkSpoolErrorReason::PendingRecords,
                "CloudLink stream epoch cannot rotate while records or unreported data-loss evidence are pending",
            ));
        }
        self.stream_epoch = self.stream_epoch.checked_add(1).ok_or_else(|| {
            error(
                CloudLinkSpoolErrorReason::Storage,
                "CloudLink stream epoch exhausted uint64",
            )
        })?;
        self.next_position = 1;
        self.last_ack = None;
        self.last_acknowledged_position = 0;
        Ok(self.stream_epoch)
    }

    pub(crate) fn pending_receipt_reservations(&self) -> usize {
        self.records
            .values()
            .filter(|record| record.retains_acknowledged_receipt())
            .count()
    }

    pub(crate) fn protected_receipt_slots(&self) -> usize {
        self.acknowledged_receipts
            .len()
            .saturating_add(self.pending_receipt_reservations())
    }

    pub(crate) fn live_byte_partitions(&self) -> Result<(u64, u64), CloudLinkSpoolError> {
        let system_live_bytes =
            self.data_loss_reports
                .iter()
                .try_fold(0_u64, |total, (position, evidence)| {
                    let record = self.records.get(position).ok_or_else(|| {
                        error(
                            CloudLinkSpoolErrorReason::Storage,
                            "data-loss report byte accounting has no retained record",
                        )
                    })?;
                    let bytes = record_live_bytes(record, Some(evidence))?;
                    total.checked_add(bytes).ok_or_else(|| {
                        error(
                            CloudLinkSpoolErrorReason::Storage,
                            "data-loss report byte accounting overflow",
                        )
                    })
                })?;
        let ordinary_live_bytes = self
            .current_live_bytes
            .checked_sub(system_live_bytes)
            .ok_or_else(|| {
                error(
                    CloudLinkSpoolErrorReason::Storage,
                    "CloudLink live-byte partition accounting underflow",
                )
            })?;
        Ok((ordinary_live_bytes, system_live_bytes))
    }

    fn record_mut(
        &mut self,
        identity: &CloudLinkRecordIdentity,
    ) -> Result<&mut CloudLinkRecord, CloudLinkSpoolError> {
        if identity.stream_id() != self.stream_id || identity.stream_epoch() != self.stream_epoch {
            return Err(error(
                CloudLinkSpoolErrorReason::WrongStream,
                "CloudLink record identity does not match the active spool",
            ));
        }
        self.records.get_mut(&identity.position()).ok_or_else(|| {
            error(
                CloudLinkSpoolErrorReason::PositionGap,
                "CloudLink record position is not retained",
            )
        })
    }
}

fn validate_stream_id(stream_id: &str) -> Result<(), CloudLinkSpoolError> {
    if valid_identifier(stream_id, 128) {
        Ok(())
    } else {
        Err(error(
            CloudLinkSpoolErrorReason::InvalidData,
            "CloudLink stream ID must be a bounded transport-safe identifier",
        ))
    }
}

fn validate_session(session: &CloudLinkSessionBinding) -> Result<(), CloudLinkSpoolError> {
    if session.session_epoch() > 0 && valid_identifier(session.session_id(), 128) {
        Ok(())
    } else {
        Err(error(
            CloudLinkSpoolErrorReason::InvalidData,
            "CloudLink session binding must contain a safe ID and positive epoch",
        ))
    }
}

fn valid_identifier(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

fn validate_enqueue(input: &CloudLinkEnqueue) -> Result<(), CloudLinkSpoolError> {
    if !valid_identifier(input.batch_id(), 128) {
        return Err(error(
            CloudLinkSpoolErrorReason::InvalidData,
            "CloudLink batch ID must be a bounded transport-safe identifier",
        ));
    }
    let digest = input.digest();
    let digest_valid = digest.len() == 71
        && digest.starts_with("sha256:")
        && digest[7..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase());
    if !digest_valid {
        return Err(error(
            CloudLinkSpoolErrorReason::InvalidData,
            "CloudLink digest must be sha256 followed by 64 lowercase hex digits",
        ));
    }
    if input.payload().is_empty() || input.payload().len() > MAX_SPOOL_PAYLOAD_BYTES {
        return Err(error(
            CloudLinkSpoolErrorReason::InvalidData,
            "CloudLink payload is empty or exceeds the 256 KiB bound",
        ));
    }
    if input
        .expires_at()
        .is_some_and(|expires| expires.get() <= input.created_at().get())
    {
        return Err(error(
            CloudLinkSpoolErrorReason::InvalidData,
            "CloudLink expiry must be after creation time",
        ));
    }
    Ok(())
}

pub(crate) fn error(
    reason: CloudLinkSpoolErrorReason,
    message: impl Into<String>,
) -> CloudLinkSpoolError {
    CloudLinkSpoolError::new(reason, message)
}

/// Deterministic in-memory implementation used by tests and local compositions.
pub struct MemoryCloudLinkSpool {
    state: Mutex<CloudLinkSpoolState>,
}

impl MemoryCloudLinkSpool {
    /// Creates one bounded logical stream.
    pub fn new(stream_id: impl Into<String>, capacity: usize) -> Result<Self, CloudLinkSpoolError> {
        Self::new_with_limits(
            stream_id,
            capacity,
            DEFAULT_ACKNOWLEDGED_RECEIPT_CAPACITY,
            DEFAULT_CLOUDLINK_SPOOL_MAX_LIVE_BYTES,
        )
    }

    /// Creates one in-memory stream with explicit count, receipt, and byte bounds.
    pub fn new_with_limits(
        stream_id: impl Into<String>,
        capacity: usize,
        acknowledged_receipt_capacity: usize,
        max_live_bytes: u64,
    ) -> Result<Self, CloudLinkSpoolError> {
        Ok(Self {
            state: Mutex::new(CloudLinkSpoolState::new_with_limits(
                stream_id,
                capacity,
                acknowledged_receipt_capacity,
                max_live_bytes,
            )?),
        })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, CloudLinkSpoolState>, CloudLinkSpoolError> {
        self.state.lock().map_err(|_| {
            error(
                CloudLinkSpoolErrorReason::Storage,
                "CloudLink memory spool lock was poisoned",
            )
        })
    }
}

#[async_trait]
impl CloudLinkSpool for MemoryCloudLinkSpool {
    async fn enqueue(
        &self,
        input: CloudLinkEnqueue,
    ) -> Result<CloudLinkRecord, CloudLinkSpoolError> {
        self.lock()?.enqueue(input)
    }

    async fn admit_lossless(
        &self,
        input: CloudLinkEnqueue,
        receipt_retention: CloudLinkReceiptRetention,
    ) -> Result<CloudLinkAdmission, CloudLinkSpoolError> {
        self.lock()?.admit_lossless(input, receipt_retention)
    }

    async fn admit_data_loss(
        &self,
        input: CloudLinkEnqueue,
        evidence: &CloudLinkDataLossEvidence,
    ) -> Result<CloudLinkAdmission, CloudLinkSpoolError> {
        self.lock()?.admit_data_loss(input, evidence)
    }

    async fn replay_from(
        &self,
        requested_position: u64,
        limit: usize,
    ) -> Result<CloudLinkReplayWindow, CloudLinkSpoolError> {
        self.lock()?.replay_from(requested_position, limit)
    }

    async fn mark_offered(
        &self,
        identity: &CloudLinkRecordIdentity,
        session: &CloudLinkSessionBinding,
    ) -> Result<(), CloudLinkSpoolError> {
        self.lock()?.mark_offered(identity, session)
    }

    async fn mark_transport_published(
        &self,
        identity: &CloudLinkRecordIdentity,
        session: &CloudLinkSessionBinding,
    ) -> Result<(), CloudLinkSpoolError> {
        self.lock()?.mark_transport_published(identity, session)
    }

    async fn acknowledge(
        &self,
        ack: &CloudLinkDurableAck,
    ) -> Result<DurableAckOutcome, CloudLinkSpoolError> {
        self.lock()?.acknowledge(ack)
    }

    async fn status(&self) -> Result<CloudLinkSpoolStatus, CloudLinkSpoolError> {
        Ok(self.lock()?.status())
    }

    async fn rotate_stream_epoch(&self) -> Result<u64, CloudLinkSpoolError> {
        self.lock()?.rotate_stream_epoch()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_domain::TimestampMs;
    use aether_ports::{CloudLinkDeliveryState, CloudLinkMessageKind};

    const RECEIPT_CAPACITY: usize = 100_000;

    #[test]
    fn equal_batch_is_idempotent_and_conflicting_content_fails_closed() {
        let mut state = CloudLinkSpoolState::new("telemetry", 4, RECEIPT_CAPACITY).expect("state");
        let input = CloudLinkEnqueue::new(
            CloudLinkMessageKind::TelemetryBatch,
            "batch-1",
            format!("sha256:{}", "a".repeat(64)),
            b"{}".to_vec(),
            TimestampMs::new(1),
            None,
        );
        let first = state.enqueue(input.clone()).expect("first");
        let replay = state.enqueue(input).expect("idempotent enqueue");
        assert_eq!(first, replay);

        let conflict = CloudLinkEnqueue::new(
            CloudLinkMessageKind::TelemetryBatch,
            "batch-1",
            format!("sha256:{}", "b".repeat(64)),
            b"{}".to_vec(),
            TimestampMs::new(1),
            None,
        );
        assert_eq!(
            state.enqueue(conflict).expect_err("conflict").reason(),
            Some(CloudLinkSpoolErrorReason::ConflictingIdentity)
        );
    }

    #[test]
    fn operational_message_kind_cannot_be_constructed() {
        assert_eq!(
            CloudLinkMessageKind::TelemetryBatch.as_str(),
            "telemetry-batch"
        );
        assert_ne!(
            CloudLinkDeliveryState::TransportPublished,
            CloudLinkDeliveryState::Queued
        );
    }

    #[test]
    fn acknowledged_receipt_capacity_never_silently_evicts_identity() {
        let mut state = CloudLinkSpoolState::new("telemetry", 2, 2).expect("state");
        for position in 1..=2 {
            let receipt = CloudLinkAcknowledgedReceipt {
                identity: CloudLinkRecordIdentity::new("telemetry", 1, position),
                message_kind: CloudLinkMessageKind::AlarmEvent,
                batch_id: format!("receipt-{position}"),
                digest: format!("sha256:{}", "a".repeat(64)),
            };
            state
                .acknowledged_receipts
                .insert(receipt.batch_id.clone(), receipt);
        }

        let exact = CloudLinkEnqueue::new(
            CloudLinkMessageKind::AlarmEvent,
            "receipt-1",
            format!("sha256:{}", "a".repeat(64)),
            b"{}".to_vec(),
            TimestampMs::new(2),
            None,
        );
        let duplicate = state
            .admit_lossless(exact, CloudLinkReceiptRetention::RetainForIdempotency)
            .expect("protected duplicate");
        assert!(duplicate.acknowledged_duplicate());
        assert_eq!(duplicate.identity().position(), 1);

        let conflict = CloudLinkEnqueue::new(
            CloudLinkMessageKind::AlarmEvent,
            "receipt-1",
            format!("sha256:{}", "b".repeat(64)),
            b"{}".to_vec(),
            TimestampMs::new(2),
            None,
        );
        assert_eq!(
            state
                .admit_lossless(conflict, CloudLinkReceiptRetention::RetainForIdempotency,)
                .expect_err("protected conflict")
                .reason(),
            Some(CloudLinkSpoolErrorReason::ConflictingIdentity)
        );

        let fresh = CloudLinkEnqueue::new(
            CloudLinkMessageKind::AlarmEvent,
            "fresh-batch",
            format!("sha256:{}", "c".repeat(64)),
            b"{}".to_vec(),
            TimestampMs::new(2),
            None,
        );
        assert_eq!(
            state
                .admit_lossless(fresh, CloudLinkReceiptRetention::RetainForIdempotency)
                .expect_err("full receipt ledger")
                .reason(),
            Some(CloudLinkSpoolErrorReason::CapacityExceeded)
        );
        assert_eq!(state.status().acknowledged_receipts(), 2);

        state
            .admit_lossless(
                CloudLinkEnqueue::new(
                    CloudLinkMessageKind::TelemetryBatch,
                    "ordinary-telemetry",
                    format!("sha256:{}", "d".repeat(64)),
                    b"{}".to_vec(),
                    TimestampMs::new(2),
                    None,
                ),
                CloudLinkReceiptRetention::DiscardAfterAck,
            )
            .expect("ordinary traffic does not consume protected receipt capacity");
    }

    #[test]
    fn lossless_admission_reserves_receipt_before_cumulative_ack() {
        let mut state = CloudLinkSpoolState::new("telemetry", 4, 1).expect("state");
        let alarm = state
            .admit_lossless(
                CloudLinkEnqueue::new(
                    CloudLinkMessageKind::AlarmEvent,
                    "alarm-1",
                    format!("sha256:{}", "b".repeat(64)),
                    b"{}".to_vec(),
                    TimestampMs::new(2),
                    None,
                ),
                CloudLinkReceiptRetention::RetainForIdempotency,
            )
            .expect("reserved lossless admission");
        assert_eq!(state.status().pending_receipt_reservations(), 1);
        assert_eq!(
            state
                .admit_lossless(
                    CloudLinkEnqueue::new(
                        CloudLinkMessageKind::AlarmEvent,
                        "alarm-2",
                        format!("sha256:{}", "c".repeat(64)),
                        b"{}".to_vec(),
                        TimestampMs::new(3),
                        None,
                    ),
                    CloudLinkReceiptRetention::RetainForIdempotency
                )
                .expect_err("second lossless event has no receipt reservation")
                .reason(),
            Some(CloudLinkSpoolErrorReason::CapacityExceeded)
        );
        let telemetry = state
            .admit_lossless(
                CloudLinkEnqueue::new(
                    CloudLinkMessageKind::TelemetryBatch,
                    "telemetry-1",
                    format!("sha256:{}", "d".repeat(64)),
                    b"{}".to_vec(),
                    TimestampMs::new(3),
                    None,
                ),
                CloudLinkReceiptRetention::DiscardAfterAck,
            )
            .expect("ordinary lossless telemetry remains admissible")
            .pending_record()
            .expect("pending telemetry")
            .clone();
        let session = CloudLinkSessionBinding::new("session-1", 1);
        state
            .mark_offered(alarm.identity(), &session)
            .expect("cumulative prefix offer");
        state
            .mark_offered(telemetry.identity(), &session)
            .expect("terminal offer");
        let ack = CloudLinkDurableAck::new(
            session,
            "telemetry",
            1,
            telemetry.identity().position(),
            telemetry.batch_id(),
            telemetry.digest(),
            "receipt-terminal",
        );
        assert_eq!(
            state.acknowledge(&ack).expect("cumulative ACK"),
            DurableAckOutcome::Applied { removed: 2 }
        );
        assert!(!state.records.contains_key(&alarm.identity().position()));
        assert_eq!(state.acknowledged_receipts.len(), 1);
        assert_eq!(state.status().pending_receipt_reservations(), 0);

        let later = state
            .admit_lossless(
                CloudLinkEnqueue::new(
                    CloudLinkMessageKind::TelemetryBatch,
                    "telemetry-after-full-receipt-ledger",
                    format!("sha256:{}", "e".repeat(64)),
                    b"{}".to_vec(),
                    TimestampMs::new(4),
                    None,
                ),
                CloudLinkReceiptRetention::DiscardAfterAck,
            )
            .expect("ordinary lossless admission after receipt capacity is full")
            .pending_record()
            .expect("pending telemetry")
            .clone();
        state
            .mark_offered(
                later.identity(),
                &CloudLinkSessionBinding::new("session-2", 2),
            )
            .expect("offer later telemetry");
        assert_eq!(
            state
                .acknowledge(&CloudLinkDurableAck::new(
                    CloudLinkSessionBinding::new("session-2", 2),
                    "telemetry",
                    1,
                    later.identity().position(),
                    later.batch_id(),
                    later.digest(),
                    "ordinary-receipt-after-protected-ledger-full",
                ))
                .expect("ordinary ACK after protected ledger is full"),
            DurableAckOutcome::Applied { removed: 1 }
        );
        assert_eq!(state.acknowledged_receipts.len(), 1);
    }

    #[test]
    fn lossless_without_post_ack_receipt_still_cannot_be_evicted_while_pending() {
        let mut state = CloudLinkSpoolState::new("telemetry", 1, 1).expect("state");
        let protected = state
            .admit_lossless(
                CloudLinkEnqueue::new(
                    CloudLinkMessageKind::TelemetryBatch,
                    "lossless-telemetry",
                    format!("sha256:{}", "d".repeat(64)),
                    b"{}".to_vec(),
                    TimestampMs::new(1),
                    None,
                ),
                CloudLinkReceiptRetention::DiscardAfterAck,
            )
            .expect("lossless admission");
        assert!(
            protected
                .pending_record()
                .expect("pending")
                .is_lossless_admission()
        );
        assert_eq!(state.status().pending_receipt_reservations(), 0);

        let error = state
            .enqueue(CloudLinkEnqueue::new(
                CloudLinkMessageKind::TelemetryBatch,
                "evicting-telemetry",
                format!("sha256:{}", "e".repeat(64)),
                b"{}".to_vec(),
                TimestampMs::new(2),
                None,
            ))
            .expect_err("generic enqueue cannot evict pending lossless content");
        assert_eq!(
            error.reason(),
            Some(CloudLinkSpoolErrorReason::CapacityExceeded)
        );
        assert_eq!(state.records.len(), 1);
    }

    #[test]
    fn more_than_100000_ordinary_lossless_acks_do_not_consume_alarm_receipts() {
        let mut state = CloudLinkSpoolState::new("telemetry", 2, 1).expect("state");
        let session = CloudLinkSessionBinding::new("session-1", 1);
        for sequence in 1_u64..=100_001 {
            let batch_id = format!("telemetry-{sequence}");
            let record = state
                .admit_lossless(
                    CloudLinkEnqueue::new(
                        CloudLinkMessageKind::TelemetryBatch,
                        batch_id,
                        format!("sha256:{}", "d".repeat(64)),
                        b"{}".to_vec(),
                        TimestampMs::new(sequence),
                        None,
                    ),
                    CloudLinkReceiptRetention::DiscardAfterAck,
                )
                .expect("ordinary lossless telemetry admission")
                .pending_record()
                .expect("pending telemetry")
                .clone();
            state
                .mark_offered(record.identity(), &session)
                .expect("offer");
            let ack = CloudLinkDurableAck::new(
                session.clone(),
                "telemetry",
                1,
                record.identity().position(),
                record.batch_id(),
                record.digest(),
                format!("receipt-{sequence}"),
            );
            assert_eq!(
                state.acknowledge(&ack).expect("ordinary ACK"),
                DurableAckOutcome::Applied { removed: 1 }
            );
        }

        assert_eq!(state.last_acknowledged_position, 100_001);
        assert!(state.acknowledged_receipts.is_empty());
    }
}
