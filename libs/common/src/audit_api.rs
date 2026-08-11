//! Shared translation for the services' command-audit read endpoints.
//!
//! Each service owns its own audit table, so each exposes its own endpoint.
//! The query vocabulary and the response shape live here so those endpoints
//! stay one contract instead of four copies that drift apart — the same reason
//! the command-permission list has a single exported home.
//!
//! Only the translation is shared. Every service still opens its own store and
//! answers for its own trail.

use aether_domain::TimestampMs;
use aether_ports::{AuditOutcome, AuditQueryFilter, AuditRecord, MAX_AUDIT_PAGE};
use serde::Deserialize;

/// Query string accepted by every command-audit read endpoint.
#[derive(Debug, Default, Deserialize)]
pub struct AuditEventsQuery {
    /// Correlates every event emitted for one governed command.
    pub request_id: Option<String>,
    /// Identity the application authorized, such as `user:7`.
    pub actor_id: Option<String>,
    /// Capability name, such as `io.channel.manage`.
    pub capability: Option<String>,
    /// One of `rejected`, `attempted`, `succeeded`, `failed`.
    pub outcome: Option<String>,
    /// Inclusive lower bound in Unix milliseconds.
    pub since_ms: Option<u64>,
    /// Inclusive upper bound in Unix milliseconds.
    pub until_ms: Option<u64>,
    /// Page size, clamped to [`MAX_AUDIT_PAGE`].
    pub limit: Option<u32>,
    /// Page offset.
    pub offset: Option<u32>,
}

/// Rejected query vocabulary, reported to the caller as a bad request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditQueryRejection {
    message: String,
}

impl AuditQueryRejection {
    /// Returns the caller-facing explanation.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for AuditQueryRejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

/// Translates a query string into a port filter.
///
/// An unrecognised outcome is rejected rather than ignored: dropping the
/// constraint would answer "show me the rejected commands" with every command,
/// and an audit reader that quietly returns more than was asked for is worse
/// than one that refuses.
pub fn filter_from_query(
    query: &AuditEventsQuery,
) -> Result<AuditQueryFilter, AuditQueryRejection> {
    let mut filter = AuditQueryFilter::new();
    if let Some(request_id) = &query.request_id {
        filter = filter.with_request_id(request_id);
    }
    if let Some(actor_id) = &query.actor_id {
        filter = filter.with_actor_id(actor_id);
    }
    if let Some(capability) = &query.capability {
        filter = filter.with_capability(capability);
    }
    if let Some(outcome) = &query.outcome {
        filter = filter.with_outcome(parse_outcome(outcome)?);
    }
    if let Some(since_ms) = query.since_ms {
        filter = filter.with_since(TimestampMs::new(since_ms));
    }
    if let Some(until_ms) = query.until_ms {
        filter = filter.with_until(TimestampMs::new(until_ms));
    }
    if query.limit.is_some() || query.offset.is_some() {
        filter = filter.with_page(query.limit.unwrap_or(0), query.offset.unwrap_or(0));
    }
    Ok(filter)
}

/// Parses one recorded outcome name.
pub fn parse_outcome(value: &str) -> Result<AuditOutcome, AuditQueryRejection> {
    match value {
        "rejected" => Ok(AuditOutcome::Rejected),
        "attempted" => Ok(AuditOutcome::Attempted),
        "succeeded" => Ok(AuditOutcome::Succeeded),
        "failed" => Ok(AuditOutcome::Failed),
        other => Err(AuditQueryRejection {
            message: format!(
                "unknown outcome '{other}'; expected rejected, attempted, succeeded, or failed"
            ),
        }),
    }
}

/// Returns the name an outcome is recorded under.
#[must_use]
pub const fn outcome_name(outcome: AuditOutcome) -> &'static str {
    match outcome {
        AuditOutcome::Rejected => "rejected",
        AuditOutcome::Attempted => "attempted",
        AuditOutcome::Succeeded => "succeeded",
        AuditOutcome::Failed => "failed",
    }
}

/// Shapes the response body for a page of audit events.
///
/// `total` counts every match rather than the returned page, so a caller can
/// tell whether more remain.
#[must_use]
pub fn events_payload(
    records: Vec<AuditRecord>,
    total: u64,
    filter: &AuditQueryFilter,
) -> serde_json::Value {
    let events = records
        .into_iter()
        .map(|record| {
            serde_json::json!({
                "request_id": record.request_id(),
                "actor_id": record.actor_id(),
                "capability": record.capability(),
                "outcome": outcome_name(record.outcome()),
                "occurred_at_ms": record.timestamp().get(),
                "detail": record.detail(),
            })
        })
        .collect::<Vec<_>>();

    serde_json::json!({
        "events": events,
        "total": total,
        "limit": filter.limit(),
        "offset": filter.offset(),
        "max_limit": MAX_AUDIT_PAGE,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_query_reads_the_default_page_of_everything() {
        let filter = filter_from_query(&AuditEventsQuery::default()).expect("default filter");

        assert_eq!(filter.request_id(), None);
        assert_eq!(filter.outcome(), None);
        assert_eq!(filter.offset(), 0);
        assert!(filter.limit() > 0, "a page size must always be applied");
    }

    #[test]
    fn an_unknown_outcome_is_refused_rather_than_silently_widened() {
        let query = AuditEventsQuery {
            outcome: Some("denied".to_owned()),
            ..AuditEventsQuery::default()
        };

        assert!(filter_from_query(&query).is_err());
    }

    #[test]
    fn a_page_larger_than_the_port_maximum_is_clamped() {
        let query = AuditEventsQuery {
            limit: Some(u32::MAX),
            offset: Some(40),
            ..AuditEventsQuery::default()
        };

        let filter = filter_from_query(&query).expect("clamped filter");

        assert_eq!(filter.limit(), MAX_AUDIT_PAGE);
        assert_eq!(filter.offset(), 40);
    }

    #[test]
    fn every_recorded_outcome_survives_a_round_trip() {
        for outcome in [
            AuditOutcome::Rejected,
            AuditOutcome::Attempted,
            AuditOutcome::Succeeded,
            AuditOutcome::Failed,
        ] {
            let parsed = parse_outcome(outcome_name(outcome)).expect("recorded name parses back");
            assert_eq!(parsed, outcome);
        }
    }

    #[test]
    fn the_payload_reports_the_full_match_count_not_the_page_length() {
        let filter = AuditQueryFilter::new().with_page(1, 0);
        let record = AuditRecord::new(
            "req-1",
            "user:7",
            "device.control",
            AuditOutcome::Succeeded,
            TimestampMs::new(1_100),
            None,
        );

        let payload = events_payload(vec![record], 12, &filter);

        assert_eq!(payload["events"].as_array().expect("events").len(), 1);
        assert_eq!(payload["total"], 12);
        assert_eq!(payload["limit"], 1);
        assert_eq!(payload["events"][0]["outcome"], "succeeded");
        assert_eq!(payload["events"][0]["capability"], "device.control");
    }
}
