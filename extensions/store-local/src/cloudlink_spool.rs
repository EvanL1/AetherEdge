//! In-memory CloudLink spool and shared durable-state transition logic.

use std::collections::BTreeMap;
use std::sync::Mutex;

use aether_ports::{
    CloudLinkDataLossEvidence, CloudLinkDurableAck, CloudLinkEnqueue, CloudLinkRecord,
    CloudLinkRecordIdentity, CloudLinkReplayWindow, CloudLinkSessionBinding, CloudLinkSpool,
    CloudLinkSpoolError, CloudLinkSpoolErrorReason, CloudLinkSpoolStatus, DurableAckOutcome,
};
use async_trait::async_trait;
pub(crate) const MAX_SPOOL_PAYLOAD_BYTES: usize = 256 * 1024;
const MAX_SPOOL_RECORDS: usize = 65_536;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CloudLinkSpoolState {
    pub(crate) stream_id: String,
    pub(crate) stream_epoch: u64,
    pub(crate) next_position: u64,
    pub(crate) capacity: usize,
    pub(crate) records: BTreeMap<u64, CloudLinkRecord>,
    pub(crate) last_ack: Option<CloudLinkDurableAck>,
    pub(crate) last_acknowledged_position: u64,
    pub(crate) data_loss: Option<CloudLinkDataLossEvidence>,
}

impl CloudLinkSpoolState {
    pub(crate) fn new(
        stream_id: impl Into<String>,
        capacity: usize,
    ) -> Result<Self, CloudLinkSpoolError> {
        let stream_id = stream_id.into();
        validate_stream_id(&stream_id)?;
        if capacity == 0 || capacity > MAX_SPOOL_RECORDS {
            return Err(error(
                CloudLinkSpoolErrorReason::InvalidData,
                "CloudLink spool capacity must be between 1 and 65536 records",
            ));
        }
        Ok(Self {
            stream_id,
            stream_epoch: 1,
            next_position: 1,
            capacity,
            records: BTreeMap::new(),
            last_ack: None,
            last_acknowledged_position: 0,
            data_loss: None,
        })
    }

    pub(crate) fn validate_open(
        &mut self,
        stream_id: &str,
        capacity: usize,
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
        if capacity == 0 || capacity > MAX_SPOOL_RECORDS || capacity < self.records.len() {
            return Err(error(
                CloudLinkSpoolErrorReason::InvalidData,
                "configured CloudLink spool capacity is smaller than retained state",
            ));
        }
        let changed = self.capacity != capacity;
        self.capacity = capacity;
        Ok(changed)
    }

    pub(crate) fn enqueue(
        &mut self,
        input: CloudLinkEnqueue,
    ) -> Result<CloudLinkRecord, CloudLinkSpoolError> {
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
                return Ok(existing.clone());
            }
            return Err(error(
                CloudLinkSpoolErrorReason::ConflictingIdentity,
                format!(
                    "CloudLink batch identity {:?} was reused with different business content",
                    input.batch_id()
                ),
            ));
        }

        let evicted = if self.records.len() == self.capacity {
            let position = self.records.keys().next().copied().ok_or_else(|| {
                error(
                    CloudLinkSpoolErrorReason::Storage,
                    "CloudLink spool capacity accounting is inconsistent",
                )
            })?;
            self.records.remove(&position);
            Some(position)
        } else {
            None
        };

        let position = self.next_position;
        self.next_position = self.next_position.checked_add(1).ok_or_else(|| {
            error(
                CloudLinkSpoolErrorReason::Storage,
                "CloudLink stream position exhausted uint64",
            )
        })?;
        let recorded_at = input.created_at();
        let identity =
            CloudLinkRecordIdentity::new(self.stream_id.clone(), self.stream_epoch, position);
        let record = CloudLinkRecord::from_enqueue(identity, input);
        self.records.insert(position, record.clone());

        if let Some(lost_position) = evicted {
            let earliest_retained = self
                .records
                .keys()
                .next()
                .copied()
                .unwrap_or(self.next_position);
            match self.data_loss.as_mut() {
                Some(evidence)
                    if evidence.stream_epoch() == self.stream_epoch
                        && evidence.last_lost_position().checked_add(1) == Some(lost_position) =>
                {
                    evidence.extend_overflow(lost_position, earliest_retained);
                },
                _ => {
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

        Ok(record)
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

        let positions = self
            .records
            .range(..=ack.acknowledged_position())
            .map(|(position, _)| *position)
            .collect::<Vec<_>>();
        for position in &positions {
            self.records.remove(position);
        }
        self.last_acknowledged_position = ack.acknowledged_position();
        self.last_ack = Some(ack.clone());
        Ok(DurableAckOutcome::Applied {
            removed: positions.len(),
        })
    }

    pub(crate) fn status(&self) -> CloudLinkSpoolStatus {
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
            self.data_loss.clone(),
        )
    }

    pub(crate) fn rotate_stream_epoch(&mut self) -> Result<u64, CloudLinkSpoolError> {
        if !self.records.is_empty() {
            return Err(error(
                CloudLinkSpoolErrorReason::PendingRecords,
                "CloudLink stream epoch cannot rotate while records are pending",
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
        self.data_loss = None;
        Ok(self.stream_epoch)
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
        Ok(Self {
            state: Mutex::new(CloudLinkSpoolState::new(stream_id, capacity)?),
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

    #[test]
    fn equal_batch_is_idempotent_and_conflicting_content_fails_closed() {
        let mut state = CloudLinkSpoolState::new("telemetry", 4).expect("state");
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
}
