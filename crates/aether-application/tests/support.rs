//! Test doubles shared by the aether-application integration tests.

use std::sync::{Arc, Mutex};

use aether_ports::{AuditOutcome, AuditRecord, AuditSink, PortError, PortErrorKind, PortResult};
use async_trait::async_trait;

/// Audit sink that captures every record and can fail on a chosen call index.
pub struct RecordingAudit {
    pub records: Mutex<Vec<AuditRecord>>,
    calls: Mutex<usize>,
    fail_on_call: Option<usize>,
    events: Arc<Mutex<Vec<&'static str>>>,
}

impl RecordingAudit {
    pub fn successful(events: Arc<Mutex<Vec<&'static str>>>) -> Self {
        Self {
            records: Mutex::new(Vec::new()),
            calls: Mutex::new(0),
            fail_on_call: None,
            events,
        }
    }

    pub fn failing_on(events: Arc<Mutex<Vec<&'static str>>>, call: usize) -> Self {
        Self {
            records: Mutex::new(Vec::new()),
            calls: Mutex::new(0),
            fail_on_call: Some(call),
            events,
        }
    }
}

#[async_trait]
impl AuditSink for RecordingAudit {
    async fn record(&self, record: AuditRecord) -> PortResult<()> {
        let call = {
            let mut calls = self.calls.lock().expect("call lock");
            *calls += 1;
            *calls
        };
        if self.fail_on_call == Some(call) {
            return Err(PortError::new(
                PortErrorKind::Unavailable,
                "audit sink unavailable",
            ));
        }
        let event = match record.outcome() {
            AuditOutcome::Rejected => "audit.rejected",
            AuditOutcome::Attempted => "audit.attempted",
            AuditOutcome::Succeeded => "audit.succeeded",
            AuditOutcome::Failed => "audit.failed",
        };
        self.events.lock().expect("event lock").push(event);
        self.records.lock().expect("record lock").push(record);
        Ok(())
    }
}
