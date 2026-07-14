//! Ops-telemetry egress for the channel-connectivity dataset.
//!
//! The channel-health SHM segment is the only one of the four runtime datasets
//! with no way off the gateway (ADR-0016). This module gives it one: it samples
//! the health plane that uplink already reads, encodes it in a JSON shape that
//! mirrors the OpenTelemetry metrics model, and hands it to the durable outbox.
//!
//! The gateway does not depend on `opentelemetry` and does not speak OTLP. It
//! borrows the data model — named metrics, typed points, explicit units, bounded
//! attributes — so a cloud consumer can terminate the stream into OTLP without
//! reconstructing any semantics. Terminating it is out of scope here.
//!
//! Cardinality is bounded by construction: metrics are per channel, never per
//! point. Point values are operational data and leave on the property topic.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use aether_ports::ChannelHealthObservation;
use chrono::Utc;
use serde::Serialize;
use tokio::time::{self, Duration};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::state::AppState;

/// Wire-format identifier. A consumer keys its decoder off this.
const SCHEMA: &str = "aether.telemetry.v1";
const SERVICE_NAME: &str = "aether-uplink";

// ── Wire contract ─────────────────────────────────────────────────────────────

/// One telemetry publication. Mirrors an OTel metrics envelope.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TelemetryPayload {
    pub schema: &'static str,
    pub resource: TelemetryResource,
    pub time_unix_ms: i64,
    pub metrics: Vec<Metric>,
}

/// Identifies the emitter. OTel calls this a Resource.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TelemetryResource {
    #[serde(rename = "device.sn")]
    pub device_sn: String,
    #[serde(rename = "product.sn")]
    pub product_sn: String,
    #[serde(rename = "service.name")]
    pub service_name: &'static str,
}

/// A named, typed, unit-carrying metric with zero or more points.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Metric {
    pub name: &'static str,
    pub unit: &'static str,
    #[serde(rename = "type")]
    pub kind: MetricKind,
    pub points: Vec<MetricPoint>,
}

/// Only gauges exist today. Cumulative sums arrive with the acquisition
/// counters (ADR-0016) and carry a process start time for reset detection;
/// adding that variant is a backward-compatible addition to this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MetricKind {
    Gauge,
}

/// One observation, keyed by its bounded attribute set.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MetricPoint {
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub attributes: BTreeMap<String, String>,
    pub value: f64,
}

impl MetricPoint {
    fn scalar(value: f64) -> Self {
        Self {
            attributes: BTreeMap::new(),
            value,
        }
    }

    fn for_channel(channel_id: u32, value: f64) -> Self {
        Self {
            attributes: BTreeMap::from([("channel.id".to_string(), channel_id.to_string())]),
            value,
        }
    }
}

// ── Payload construction ──────────────────────────────────────────────────────

/// What one channel's health plane yielded on one pass.
///
/// Three states, kept distinct in the type so they cannot be conflated. Folding
/// `ReadFailed` into `NeverObserved` would publish a *failure to observe* as the
/// positive claim *"acquisition observed nothing"* — and a topology
/// republication, which makes reads transiently conflict, would then look
/// exactly like a plant going dark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelSample {
    Observed(ChannelHealthObservation),
    /// Configured, but acquisition has never written a state for it.
    NeverObserved,
    /// The health plane could not be read this pass. Says nothing about the plant.
    ReadFailed,
}

/// Builds one publication from an already-taken sample set.
///
/// Pure on purpose: the wire contract is the part of this feature that cannot be
/// changed once devices are fielded, so it is testable without SHM or a broker.
///
/// A channel that is not `Observed` is deliberately absent from
/// `aether.channel.state` — in metric semantics an absent point means "no data",
/// which is exactly true, and is not the same claim as "offline". It appears
/// instead under `aether.channel.unobserved` or `aether.channel.health_read_errors`,
/// per channel, so an operator can see *which* channels are dark and *why*.
pub fn build_payload(
    resource: TelemetryResource,
    samples: &[(u32, ChannelSample)],
    mqtt_connected: bool,
    now_ms: i64,
) -> TelemetryPayload {
    let mut state_points = Vec::new();
    let mut duration_points = Vec::new();
    let mut unobserved_points = Vec::new();
    let mut read_error_points = Vec::new();

    for (channel_id, sample) in samples {
        let observation = match sample {
            ChannelSample::Observed(observation) => observation,
            ChannelSample::NeverObserved => {
                unobserved_points.push(MetricPoint::for_channel(*channel_id, 1.0));
                continue;
            },
            ChannelSample::ReadFailed => {
                read_error_points.push(MetricPoint::for_channel(*channel_id, 1.0));
                continue;
            },
        };

        state_points.push(MetricPoint::for_channel(
            *channel_id,
            f64::from(u8::from(observation.online())),
        ));

        // `observed_at` is the last state *transition*, not a heartbeat, so this
        // is time spent in the current state. Short durations on a channel that
        // keeps reappearing are what flapping looks like. Clamped because a
        // gateway clock can step backwards behind a late NTP sync.
        let observed_at = i64::try_from(observation.timestamp_ms()).unwrap_or(i64::MAX);
        let duration_ms = now_ms.saturating_sub(observed_at).max(0);
        duration_points.push(MetricPoint::for_channel(*channel_id, duration_ms as f64));
    }

    TelemetryPayload {
        schema: SCHEMA,
        resource,
        time_unix_ms: now_ms,
        metrics: vec![
            Metric {
                name: "aether.channel.state",
                unit: "1",
                kind: MetricKind::Gauge,
                points: state_points,
            },
            Metric {
                name: "aether.channel.state.duration_ms",
                unit: "ms",
                kind: MetricKind::Gauge,
                points: duration_points,
            },
            Metric {
                name: "aether.channel.unobserved",
                unit: "1",
                kind: MetricKind::Gauge,
                points: unobserved_points,
            },
            Metric {
                name: "aether.channel.health_read_errors",
                unit: "1",
                kind: MetricKind::Gauge,
                points: read_error_points,
            },
            Metric {
                name: "aether.mqtt.connected",
                unit: "1",
                kind: MetricKind::Gauge,
                points: vec![MetricPoint::scalar(f64::from(u8::from(mqtt_connected)))],
            },
        ],
    }
}

// ── Collection and publication ────────────────────────────────────────────────

/// Periodically samples the health plane and enqueues one publication.
///
/// Publishes through the durable outbox, not the live MQTT client, so a gateway
/// that was offline still reports the connectivity it observed while offline —
/// which is the window an operator most wants back.
pub async fn run_telemetry_reporter(state: Arc<AppState>, shutdown: CancellationToken) {
    loop {
        let (enabled, interval) = {
            let cfg = state.config.read().await;
            (cfg.telemetry_enabled, cfg.telemetry_interval_secs)
        };

        tokio::select! {
            _ = time::sleep(Duration::from_secs(interval)) => {}
            _ = shutdown.cancelled() => return,
        }

        if !enabled {
            continue;
        }

        report_once(&state).await;
    }
}

async fn report_once(state: &AppState) {
    let payload = collect(state);
    if let Err(error) = crate::uplink::enqueue_json(state, &state.topics.telemetry, &payload).await
    {
        warn!("Telemetry enqueue failed: {error}");
    }
}

/// Samples every configured channel from one pinned topology generation.
fn collect(state: &AppState) -> TelemetryPayload {
    // Pin one generation for the whole pass, as the property forwarder does: a
    // concurrent republication must not let two channels be read from different
    // manifests. Unlike the forwarder this does not fail the whole pass on a
    // read error — a telemetry pass that reports nothing is worse than one that
    // reports which channels it failed to read, so the failure is published as
    // itself rather than swallowed.
    let generation = state.live_topology.load();
    let samples: Vec<(u32, ChannelSample)> = generation
        .channel_ids()
        .map(|channel_id| {
            let sample = match generation.channel_health(channel_id) {
                Ok(Some(observation)) => ChannelSample::Observed(observation),
                Ok(None) => ChannelSample::NeverObserved,
                Err(error) => {
                    warn!(
                        channel_id,
                        retryable = error.is_retryable(),
                        publication_epoch = generation.publication_epoch(),
                        "Channel health read failed: {error}"
                    );
                    ChannelSample::ReadFailed
                },
            };
            (channel_id, sample)
        })
        .collect();

    build_payload(
        TelemetryResource {
            device_sn: state.device.device_sn.clone(),
            product_sn: state.device.product_sn.clone(),
            service_name: SERVICE_NAME,
        },
        &samples,
        state.mqtt_connected.load(Ordering::Relaxed),
        Utc::now().timestamp_millis(),
    )
}

#[cfg(test)]
mod tests {
    use aether_domain::{ChannelId, TimestampMs};
    use serde_json::json;

    use super::*;

    const NOW: i64 = 1_720_900_060_000;

    fn resource() -> TelemetryResource {
        TelemetryResource {
            device_sn: "GW-001".to_string(),
            product_sn: "P-01".to_string(),
            service_name: SERVICE_NAME,
        }
    }

    fn observed(channel_id: u32, online: bool, at_ms: u64) -> ChannelSample {
        ChannelSample::Observed(ChannelHealthObservation::new(
            ChannelId::new(channel_id),
            online,
            TimestampMs::new(at_ms),
        ))
    }

    fn metric<'a>(payload: &'a TelemetryPayload, name: &str) -> &'a Metric {
        payload
            .metrics
            .iter()
            .find(|metric| metric.name == name)
            .unwrap_or_else(|| panic!("metric {name} missing"))
    }

    fn channels(payload: &TelemetryPayload, name: &str) -> Vec<String> {
        metric(payload, name)
            .points
            .iter()
            .map(|point| point.attributes["channel.id"].clone())
            .collect()
    }

    #[test]
    fn online_channel_reports_state_one_and_time_in_state() {
        let samples = [(7, observed(7, true, 1_720_900_055_000))];
        let payload = build_payload(resource(), &samples, true, NOW);

        let state = metric(&payload, "aether.channel.state");
        assert_eq!(state.points.len(), 1);
        assert_eq!(state.points[0].value, 1.0);
        assert_eq!(state.points[0].attributes["channel.id"], "7");

        let duration = metric(&payload, "aether.channel.state.duration_ms");
        assert_eq!(duration.points[0].value, 5_000.0);
    }

    #[test]
    fn offline_channel_reports_state_zero() {
        let samples = [(3, observed(3, false, 1_720_900_000_000))];
        let payload = build_payload(resource(), &samples, true, NOW);

        let state = metric(&payload, "aether.channel.state");
        assert_eq!(state.points[0].value, 0.0);
        assert_eq!(state.points[0].attributes["channel.id"], "3");
    }

    #[test]
    fn unobserved_channel_is_absent_from_state_not_reported_as_offline() {
        let samples = [
            (1, observed(1, true, NOW as u64)),
            (2, ChannelSample::NeverObserved),
            (9, ChannelSample::NeverObserved),
        ];
        let payload = build_payload(resource(), &samples, true, NOW);

        // The claim "no data" must not be encoded as the claim "offline".
        assert_eq!(channels(&payload, "aether.channel.state"), ["1"]);
        // And an operator must be able to see *which* channels are dark.
        assert_eq!(channels(&payload, "aether.channel.unobserved"), ["2", "9"]);
    }

    /// The distinction the type exists to enforce. A health-plane read that
    /// fails — a topology republication conflicts, io is mid-restart — says
    /// nothing whatsoever about the plant. Reporting it as `unobserved` would
    /// publish a failure to look as though acquisition had looked and seen
    /// nothing, and every republication would read as a plant going dark.
    #[test]
    fn a_read_failure_is_never_reported_as_an_observation() {
        let samples = [
            (1, observed(1, true, NOW as u64)),
            (2, ChannelSample::ReadFailed),
            (3, ChannelSample::NeverObserved),
        ];
        let payload = build_payload(resource(), &samples, true, NOW);

        assert_eq!(channels(&payload, "aether.channel.state"), ["1"]);
        assert_eq!(channels(&payload, "aether.channel.unobserved"), ["3"]);
        assert_eq!(
            channels(&payload, "aether.channel.health_read_errors"),
            ["2"]
        );
    }

    #[test]
    fn dead_acquisition_is_distinguishable_from_a_dead_gateway() {
        // io is down: every channel has never been observed. Telemetry still
        // flows, carrying that fact. Without it, a live gateway whose
        // acquisition is dead looks exactly like a gateway that is simply gone.
        let samples = [
            (1, ChannelSample::NeverObserved),
            (2, ChannelSample::NeverObserved),
            (3, ChannelSample::NeverObserved),
        ];
        let payload = build_payload(resource(), &samples, true, NOW);

        assert!(metric(&payload, "aether.channel.state").points.is_empty());
        assert_eq!(
            channels(&payload, "aether.channel.unobserved"),
            ["1", "2", "3"]
        );
    }

    #[test]
    fn backward_clock_step_clamps_time_in_state_to_zero() {
        // A gateway that boots with a bad RTC and then syncs NTP can observe a
        // transition timestamp in its own future. A negative duration would
        // render as a nonsense spike rather than a missing sample.
        let samples = [(4, observed(4, true, (NOW + 60_000) as u64))];
        let payload = build_payload(resource(), &samples, true, NOW);

        assert_eq!(
            metric(&payload, "aether.channel.state.duration_ms").points[0].value,
            0.0
        );
    }

    #[test]
    fn mqtt_disconnected_reports_zero() {
        let payload = build_payload(resource(), &[], false, NOW);
        assert_eq!(
            metric(&payload, "aether.mqtt.connected").points[0].value,
            0.0
        );
    }

    #[test]
    fn no_channels_configured_still_publishes_a_well_formed_envelope() {
        let payload = build_payload(resource(), &[], true, NOW);
        assert!(
            metric(&payload, "aether.channel.unobserved")
                .points
                .is_empty()
        );
        assert!(metric(&payload, "aether.channel.state").points.is_empty());
    }

    /// The wire contract. This is the assertion that cannot be relaxed: once
    /// gateways are fielded and clouds parse this shape, changing it is a
    /// breaking change for every deployed consumer.
    #[test]
    fn wire_format_is_stable() {
        let samples = [
            (7, observed(7, true, 1_720_900_055_000)),
            (8, ChannelSample::NeverObserved),
            (9, ChannelSample::ReadFailed),
        ];
        let payload = build_payload(resource(), &samples, true, NOW);

        assert_eq!(
            serde_json::to_value(&payload).expect("payload serializes"),
            json!({
                "schema": "aether.telemetry.v1",
                "resource": {
                    "device.sn": "GW-001",
                    "product.sn": "P-01",
                    "service.name": "aether-uplink"
                },
                "time_unix_ms": 1_720_900_060_000i64,
                "metrics": [
                    {
                        "name": "aether.channel.state",
                        "unit": "1",
                        "type": "gauge",
                        "points": [{"attributes": {"channel.id": "7"}, "value": 1.0}]
                    },
                    {
                        "name": "aether.channel.state.duration_ms",
                        "unit": "ms",
                        "type": "gauge",
                        "points": [{"attributes": {"channel.id": "7"}, "value": 5000.0}]
                    },
                    {
                        "name": "aether.channel.unobserved",
                        "unit": "1",
                        "type": "gauge",
                        "points": [{"attributes": {"channel.id": "8"}, "value": 1.0}]
                    },
                    {
                        "name": "aether.channel.health_read_errors",
                        "unit": "1",
                        "type": "gauge",
                        "points": [{"attributes": {"channel.id": "9"}, "value": 1.0}]
                    },
                    {
                        "name": "aether.mqtt.connected",
                        "unit": "1",
                        "type": "gauge",
                        "points": [{"value": 1.0}]
                    }
                ]
            })
        );
    }
}
