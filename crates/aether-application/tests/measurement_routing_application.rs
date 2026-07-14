use std::sync::{Arc, Mutex};

use aether_application::{
    Actor, ApplicationError, MeasurementRoutingApplication, RequestContext, SafetyPolicy,
};
use aether_domain::{ChannelId, ChannelPointAddress, InstanceId, PointId, PointKind, TimestampMs};
use aether_ports::{
    AuditOutcome, AuditRecord, AuditSink, AutomationMeasurementRoutingMutator,
    LogicalRoutingRevision, MeasurementRoute, MeasurementRouteKey, MeasurementRoutingMutation,
    MeasurementRoutingMutationReceipt, PortError, PortErrorKind, PortResult,
};
use async_trait::async_trait;

#[derive(Default)]
struct RecordingMutator {
    mutations: Mutex<Vec<MeasurementRoutingMutation>>,
}

#[async_trait]
impl AutomationMeasurementRoutingMutator for RecordingMutator {
    async fn mutate(
        &self,
        mutation: MeasurementRoutingMutation,
    ) -> PortResult<MeasurementRoutingMutationReceipt> {
        self.mutations.lock().expect("mutation lock").push(mutation);
        Ok(MeasurementRoutingMutationReceipt::new(
            mutation.kind(),
            mutation.target(),
            1,
            mutation
                .expected_revision()
                .checked_next()
                .expect("revision capacity"),
        ))
    }
}

#[derive(Default)]
struct RecordingAudit {
    records: Mutex<Vec<AuditRecord>>,
    unavailable: bool,
}

#[async_trait]
impl AuditSink for RecordingAudit {
    async fn record(&self, record: AuditRecord) -> PortResult<()> {
        if self.unavailable {
            return Err(PortError::new(
                PortErrorKind::Unavailable,
                "audit unavailable",
            ));
        }
        self.records.lock().expect("audit lock").push(record);
        Ok(())
    }
}

fn mutation() -> MeasurementRoutingMutation {
    let key = MeasurementRouteKey::new(InstanceId::new(7), PointId::new(11));
    let destination =
        ChannelPointAddress::new(ChannelId::new(3), PointKind::Status, PointId::new(19))
            .expect("acquisition-owned destination");
    MeasurementRoutingMutation::upsert(
        MeasurementRoute::new(key, destination, true),
        LogicalRoutingRevision::new(8),
    )
}

fn context(permission: bool) -> RequestContext {
    let actor = if permission {
        Actor::new("operator").with_permission("automation.routing.manage")
    } else {
        Actor::new("operator")
    };
    RequestContext::new(
        "measurement-routing-1",
        actor,
        true,
        TimestampMs::new(2_000),
    )
}

#[tokio::test]
async fn authorized_measurement_mutation_is_durably_audited_around_the_port() {
    let mutator = Arc::new(RecordingMutator::default());
    let audit = Arc::new(RecordingAudit::default());
    let application =
        MeasurementRoutingApplication::new(mutator.clone(), audit.clone(), SafetyPolicy);

    let acceptance = application
        .mutate(&context(true), mutation())
        .await
        .expect("governed measurement mutation");

    assert_eq!(
        acceptance.resulting_revision(),
        LogicalRoutingRevision::new(9)
    );
    assert_eq!(mutator.mutations.lock().expect("mutation lock").len(), 1);
    let records = audit.records.lock().expect("audit lock");
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].outcome(), AuditOutcome::Attempted);
    assert_eq!(records[1].outcome(), AuditOutcome::Succeeded);
    assert!(records.iter().all(|record| {
        record.capability() == "automation.routing.manage"
            && record
                .detail()
                .is_some_and(|detail| detail.contains("expected_revision=8"))
    }));
}

#[tokio::test]
async fn denied_or_unauditable_measurement_mutation_never_reaches_storage() {
    for (request_context, unavailable) in [(context(false), false), (context(true), true)] {
        let mutator = Arc::new(RecordingMutator::default());
        let audit = Arc::new(RecordingAudit {
            unavailable,
            ..RecordingAudit::default()
        });
        let application = MeasurementRoutingApplication::new(mutator.clone(), audit, SafetyPolicy);

        let result = application.mutate(&request_context, mutation()).await;

        assert!(matches!(
            result,
            Err(ApplicationError::PermissionDenied { .. })
                | Err(ApplicationError::AuditUnavailable(_))
        ));
        assert!(mutator.mutations.lock().expect("mutation lock").is_empty());
    }
}
