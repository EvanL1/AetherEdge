//! In-memory ledger for tests and explicitly non-durable compositions.

use std::collections::BTreeMap;
use std::sync::Mutex;

use aether_ports::{CloudLinkDurableAck, DurableAckOutcome};
use async_trait::async_trait;

use crate::ports::{
    ControlStoreError, IntegrationControlLedger, LedgerClaim, LedgerClaimOutcome, LedgerEntry,
    LedgerEntryState, LedgerJobKey, SpooledActionReceipt,
};
use crate::wire::ActionReceiptPayload;

struct MemoryState {
    jobs: BTreeMap<LedgerJobKey, LedgerEntry>,
    deliveries: BTreeMap<String, SpooledActionReceipt>,
    pending: BTreeMap<u64, SpooledActionReceipt>,
    next_position: u64,
    last_ack: Option<CloudLinkDurableAck>,
}

impl Default for MemoryState {
    fn default() -> Self {
        Self {
            jobs: BTreeMap::new(),
            deliveries: BTreeMap::new(),
            pending: BTreeMap::new(),
            next_position: 1,
            last_ack: None,
        }
    }
}

/// Process-local ledger useful for conformance tests.
///
/// Production control should use a persistent implementation because process
/// loss while a provider call is in flight must not lead to automatic retry.
pub struct MemoryIntegrationControlLedger {
    state: Mutex<MemoryState>,
}

impl MemoryIntegrationControlLedger {
    /// Creates an empty process-local ledger.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Mutex::new(MemoryState::default()),
        }
    }

    /// Returns the number of unique pending terminal receipts.
    pub async fn pending_receipt_count(&self) -> usize {
        self.state
            .lock()
            .map(|state| state.pending.len())
            .unwrap_or_default()
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, MemoryState>, ControlStoreError> {
        self.state.lock().map_err(|_source| ControlStoreError)
    }
}

impl Default for MemoryIntegrationControlLedger {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IntegrationControlLedger for MemoryIntegrationControlLedger {
    async fn find(&self, key: &LedgerJobKey) -> Result<Option<LedgerEntry>, ControlStoreError> {
        Ok(self.lock()?.jobs.get(key).cloned())
    }

    async fn claim(&self, claim: LedgerClaim) -> Result<LedgerClaimOutcome, ControlStoreError> {
        let mut state = self.lock()?;
        if let Some(existing) = state.jobs.get(claim.key()) {
            return if existing.intent_digest() == claim.intent_digest() {
                Ok(LedgerClaimOutcome::Existing(Box::new(existing.clone())))
            } else {
                Ok(LedgerClaimOutcome::DigestConflict)
            };
        }
        state
            .jobs
            .insert(claim.key().clone(), LedgerEntry::in_progress(claim));
        Ok(LedgerClaimOutcome::Claimed)
    }

    async fn complete(
        &self,
        key: &LedgerJobKey,
        intent_digest: &str,
        receipt: ActionReceiptPayload,
    ) -> Result<(), ControlStoreError> {
        if receipt.job_id() != key.job_id() || receipt.intent_digest() != intent_digest {
            return Err(ControlStoreError);
        }
        let mut state = self.lock()?;
        let Some(existing) = state.jobs.get(key) else {
            return Err(ControlStoreError);
        };
        if existing.intent_digest() != intent_digest {
            return Err(ControlStoreError);
        }
        if let LedgerEntryState::Complete(stored) = existing.state() {
            return if stored == &receipt {
                Ok(())
            } else {
                Err(ControlStoreError)
            };
        }
        let delivery = match state.deliveries.get(receipt.receipt_id()) {
            Some(delivery) => delivery.clone(),
            None => {
                let position = state.next_position;
                state.next_position = state
                    .next_position
                    .checked_add(1)
                    .ok_or(ControlStoreError)?;
                let delivery = SpooledActionReceipt::new(position, receipt.clone())?;
                state
                    .deliveries
                    .insert(receipt.receipt_id().to_string(), delivery.clone());
                delivery
            },
        };
        state.pending.insert(delivery.position(), delivery);
        state.jobs.insert(
            key.clone(),
            LedgerEntry::complete(intent_digest.to_string(), receipt),
        );
        Ok(())
    }

    async fn requeue(&self, key: &LedgerJobKey) -> Result<(), ControlStoreError> {
        let mut state = self.lock()?;
        let receipt = match state.jobs.get(key).map(LedgerEntry::state) {
            Some(LedgerEntryState::Complete(receipt)) => receipt.clone(),
            Some(LedgerEntryState::InProgress(_)) | None => return Err(ControlStoreError),
        };
        let mut delivery = state
            .deliveries
            .get(receipt.receipt_id())
            .cloned()
            .ok_or(ControlStoreError)?;
        if state
            .last_ack
            .as_ref()
            .is_some_and(|ack| delivery.position() <= ack.acknowledged_position())
        {
            let position = state.next_position;
            state.next_position = state
                .next_position
                .checked_add(1)
                .ok_or(ControlStoreError)?;
            delivery = SpooledActionReceipt::new(position, receipt.clone())?;
            state
                .deliveries
                .insert(receipt.receipt_id().to_string(), delivery.clone());
        }
        state.pending.insert(delivery.position(), delivery);
        Ok(())
    }

    async fn interrupted(&self) -> Result<Vec<LedgerClaim>, ControlStoreError> {
        Ok(self
            .lock()?
            .jobs
            .values()
            .filter_map(|entry| match entry.state() {
                LedgerEntryState::InProgress(claim) => Some(claim.clone()),
                LedgerEntryState::Complete(_) => None,
            })
            .collect())
    }

    async fn pending_receipts(
        &self,
        limit: usize,
    ) -> Result<Vec<SpooledActionReceipt>, ControlStoreError> {
        Ok(self.lock()?.pending.values().take(limit).cloned().collect())
    }

    async fn acknowledge(
        &self,
        ack: &CloudLinkDurableAck,
    ) -> Result<DurableAckOutcome, ControlStoreError> {
        let mut state = self.lock()?;
        acknowledge(&mut state, ack)
    }
}

fn acknowledge(
    state: &mut MemoryState,
    ack: &CloudLinkDurableAck,
) -> Result<DurableAckOutcome, ControlStoreError> {
    if state.last_ack.as_ref() == Some(ack) {
        return Ok(DurableAckOutcome::Duplicate);
    }
    if ack.stream_id() != "integration-control-receipts"
        || ack.stream_epoch() != 1
        || ack.acknowledged_position() == 0
        || ack.session().session_epoch() == 0
        || state
            .last_ack
            .as_ref()
            .is_some_and(|last| ack.acknowledged_position() <= last.acknowledged_position())
    {
        return Err(ControlStoreError);
    }
    crate::validation::identifier(ack.session().session_id())
        .map_err(|_source| ControlStoreError)?;
    crate::validation::identifier(ack.receipt_id()).map_err(|_source| ControlStoreError)?;
    let terminal = state
        .deliveries
        .values()
        .find(|record| record.position() == ack.acknowledged_position())
        .ok_or(ControlStoreError)?;
    if terminal.batch_id() != ack.batch_id()
        || terminal.digest() != ack.digest()
        || state.pending.get(&terminal.position()) != Some(terminal)
    {
        return Err(ControlStoreError);
    }
    let positions: Vec<_> = state
        .pending
        .range(..=ack.acknowledged_position())
        .map(|(position, _record)| *position)
        .collect();
    for position in &positions {
        state.pending.remove(position);
    }
    state.last_ack = Some(ack.clone());
    Ok(DurableAckOutcome::Applied {
        removed: positions.len(),
    })
}
