//! Reusable conformance checks and deterministic test doubles for extension authors.

use std::collections::VecDeque;
use std::sync::Mutex;

use aether_domain::{
    DataProcessingRequest, PointSample, ProcessingOptions, ProcessingOutput, ProcessingResult,
    ProcessingStatus, SourceKind, TaskKind,
};
use aether_ports::{
    CloudLinkTransport, CloudLinkTransportEvent, CloudLinkTransportMessage, DataProcessor,
    DataProcessorDescriptor, DurableOutbox, HistoryQuery, HistoryWindow, LiveState,
    LiveStateWriter, OutboxMessage, PortError, PortErrorKind, PortResult, ProcessorHealth,
    SourcedSegment,
};
use async_trait::async_trait;
use tokio::sync::{Mutex as AsyncMutex, mpsc};

/// One endpoint of a bounded in-memory CloudLink transport pair.
///
/// `send` delivers an inbound event to the peer. Durable messages also produce
/// transport-published evidence for the sender; no application ACK is invented.
pub struct MemoryCloudLinkTransport {
    own_events: mpsc::Sender<PortResult<CloudLinkTransportEvent>>,
    peer_events: mpsc::Sender<PortResult<CloudLinkTransportEvent>>,
    events: AsyncMutex<mpsc::Receiver<PortResult<CloudLinkTransportEvent>>>,
}

impl MemoryCloudLinkTransport {
    /// Creates two connected bounded endpoints.
    pub fn pair(capacity: usize) -> PortResult<(Self, Self)> {
        if capacity == 0 {
            return Err(contract_error(
                "memory CloudLink transport capacity must be greater than zero",
            ));
        }
        let (a_tx, a_rx) = mpsc::channel(capacity);
        let (b_tx, b_rx) = mpsc::channel(capacity);
        a_tx.try_send(Ok(CloudLinkTransportEvent::Connected))
            .map_err(|_| contract_error("cannot initialize memory CloudLink endpoint A"))?;
        b_tx.try_send(Ok(CloudLinkTransportEvent::Connected))
            .map_err(|_| contract_error("cannot initialize memory CloudLink endpoint B"))?;
        Ok((
            Self {
                own_events: a_tx.clone(),
                peer_events: b_tx.clone(),
                events: AsyncMutex::new(a_rx),
            },
            Self {
                own_events: b_tx,
                peer_events: a_tx,
                events: AsyncMutex::new(b_rx),
            },
        ))
    }

    /// Injects a deterministic disconnect observation at both endpoints.
    pub async fn disconnect(&self) -> PortResult<()> {
        self.own_events
            .send(Ok(CloudLinkTransportEvent::Disconnected))
            .await
            .map_err(|_| contract_error("memory CloudLink endpoint is closed"))?;
        self.peer_events
            .send(Ok(CloudLinkTransportEvent::Disconnected))
            .await
            .map_err(|_| contract_error("memory CloudLink peer is closed"))
    }
}

#[async_trait]
impl CloudLinkTransport for MemoryCloudLinkTransport {
    async fn send(&self, message: CloudLinkTransportMessage) -> PortResult<()> {
        self.peer_events
            .send(Ok(CloudLinkTransportEvent::Inbound(message.clone())))
            .await
            .map_err(|_| contract_error("memory CloudLink peer is closed"))?;
        if let Some(identity) = message.delivery().cloned() {
            self.own_events
                .send(Ok(CloudLinkTransportEvent::TransportPublished(identity)))
                .await
                .map_err(|_| contract_error("memory CloudLink endpoint is closed"))?;
        }
        Ok(())
    }

    async fn receive(&self) -> PortResult<CloudLinkTransportEvent> {
        self.events.lock().await.recv().await.unwrap_or_else(|| {
            Err(PortError::new(
                PortErrorKind::Unavailable,
                "memory CloudLink event stream ended",
            ))
        })
    }
}

/// Verifies exact projection, feature ordering, half-open bounds, and hard limits.
///
/// The query must already contain the fixture represented by `expected`. When
/// the fixture has more than one timestamp, this check also repeats the same
/// query with a smaller `max_samples` and requires an explicit rejection rather
/// than truncation.
pub async fn assert_history_query_bounded(
    query: &dyn HistoryQuery,
    window: HistoryWindow,
    expected: SourcedSegment,
) -> PortResult<SourcedSegment> {
    let actual = query.query(window.clone()).await?;
    if actual != expected {
        return Err(contract_error(
            "history query changed projection values, ordering, or provenance",
        ));
    }
    if actual.segment().sample_count() > window.max_samples()
        || actual
            .segment()
            .timestamps()
            .iter()
            .any(|timestamp| *timestamp < window.start() || *timestamp >= window.end())
        || actual.segment().series().len() != window.features().len()
        || actual
            .segment()
            .series()
            .iter()
            .zip(window.features())
            .any(|(series, requested)| series.definition() != requested)
    {
        return Err(contract_error(
            "history query violated its feature, time, or sample bounds",
        ));
    }

    let sample_count = actual.segment().sample_count();
    if sample_count > 1 {
        let default_policy = window
            .policies()
            .first()
            .ok_or_else(|| contract_error("history window has no feature policy"))?;
        let stricter_window = HistoryWindow::new(
            window.task().clone(),
            window.binding().clone(),
            window.features().to_vec(),
            window.start(),
            window.end(),
            sample_count - 1,
            default_policy.aggregation(),
            default_policy.duplicate_policy(),
        )?
        .with_feature_policies(window.policies().to_vec())?;
        match query.query(stricter_window).await {
            Err(error) if error.kind() == PortErrorKind::Rejected => {},
            Err(_) => {
                return Err(contract_error(
                    "history query used the wrong error kind for an exceeded sample bound",
                ));
            },
            Ok(_) => {
                return Err(contract_error(
                    "history query silently truncated or exceeded its sample bound",
                ));
            },
        }
    }

    Ok(actual)
}

/// Verifies exact, one-to-one provenance for a bounded history response.
pub async fn assert_history_query_provenance(
    query: &dyn HistoryQuery,
    window: HistoryWindow,
    expected: &[aether_domain::SourceProvenance],
) -> PortResult<SourcedSegment> {
    let actual = query.query(window.clone()).await?;
    if actual.provenance() != expected
        || actual.provenance().len() != actual.segment().series().len()
        || actual
            .segment()
            .series()
            .iter()
            .zip(actual.provenance())
            .any(|(series, provenance)| {
                provenance.segment() != aether_domain::SegmentKind::History
                    || provenance.feature() != series.definition().name()
                    || !matches!(
                        provenance.source_kind(),
                        SourceKind::History
                            | SourceKind::Live
                            | SourceKind::HistoryAndLive
                            | SourceKind::Calendar
                    )
                    || provenance.issued_at().is_some()
                    || provenance.watermark() >= window.end()
            })
    {
        return Err(contract_error(
            "history query did not return exact bounded per-feature provenance",
        ));
    }

    Ok(actual)
}

/// Verifies descriptor limits plus exact request/result correlation for a processor.
///
/// The supplied request is complete and may be executed. Use only synthetic or
/// explicitly approved fixture data in conformance tests.
pub async fn assert_data_processor_correlation(
    processor: &dyn DataProcessor,
    request: DataProcessingRequest,
) -> PortResult<ProcessingResult> {
    let descriptor = processor.descriptor();
    let task_kind = match request.options() {
        ProcessingOptions::Forecast(_) => TaskKind::Forecast,
    };
    if !descriptor.supports(task_kind)
        || !descriptor.supports_contract(request.processor_contract())
        || request.frame().cell_count() > descriptor.max_frame_samples()
    {
        return Err(contract_error(
            "processor descriptor does not admit the supplied request",
        ));
    }

    let expected_request_id = request.request_id().to_string();
    let expected_task = request.task().clone();
    let expected_binding = request.binding().clone();
    let expected_digest = request.input_digest().to_string();
    let expected_watermark = request.frame().quality().input_watermark();
    let expected_contract = request.processor_contract().to_string();
    let result = processor.process(request).await?;

    if result.request_id() != expected_request_id
        || result.task() != &expected_task
        || result.binding() != &expected_binding
        || result.input_digest() != expected_digest
        || result.input_watermark() != expected_watermark
        || result.processor().id() != descriptor.id()
        || result.processor().version() != descriptor.version()
        || result.processor().contract() != expected_contract
    {
        return Err(contract_error(
            "processor changed request identity, digest, watermark, or provenance",
        ));
    }

    match (result.status(), result.output()) {
        (ProcessingStatus::Unavailable, None) => {},
        (ProcessingStatus::Unavailable, Some(_)) => {
            return Err(contract_error(
                "unavailable processor result exposed derived output",
            ));
        },
        (ProcessingStatus::Produced | ProcessingStatus::Fallback, Some(output)) => {
            validate_processing_output(output)?;
        },
        (ProcessingStatus::Produced | ProcessingStatus::Fallback, None) => {
            return Err(contract_error(
                "usable processor result omitted its derived output",
            ));
        },
    }

    Ok(result)
}

fn validate_processing_output(output: &ProcessingOutput) -> PortResult<()> {
    match output {
        ProcessingOutput::Forecast(forecast) => {
            if forecast.points().is_empty()
                || forecast.points().iter().any(|point| {
                    !point.value().is_finite()
                        || point.quantiles().iter().any(|quantile| {
                            !quantile.probability().is_finite() || !quantile.value().is_finite()
                        })
                })
                || forecast.points().windows(2).any(|pair| {
                    pair[0].timestamp() >= pair[1].timestamp()
                        || pair[1].timestamp().get() - pair[0].timestamp().get()
                            != forecast.cadence_ms()
                })
            {
                return Err(contract_error(
                    "processor returned non-finite or unordered forecast output",
                ));
            }
        },
    }
    Ok(())
}

/// Queue-driven `DataProcessor` test double that records every complete request.
#[derive(Debug)]
pub struct ScriptedDataProcessor {
    descriptor: DataProcessorDescriptor,
    health: Mutex<PortResult<ProcessorHealth>>,
    outcomes: Mutex<VecDeque<PortResult<ProcessingResult>>>,
    requests: Mutex<Vec<DataProcessingRequest>>,
}

impl ScriptedDataProcessor {
    /// Creates a healthy processor with an empty response queue.
    #[must_use]
    pub fn new(descriptor: DataProcessorDescriptor) -> Self {
        Self {
            descriptor,
            health: Mutex::new(Ok(ProcessorHealth::Healthy)),
            outcomes: Mutex::new(VecDeque::new()),
            requests: Mutex::new(Vec::new()),
        }
    }

    /// Queues one successful typed processor result.
    pub fn enqueue_result(&self, result: ProcessingResult) -> PortResult<()> {
        self.enqueue(Ok(result))
    }

    /// Queues one typed port failure.
    pub fn enqueue_error(&self, error: PortError) -> PortResult<()> {
        self.enqueue(Err(error))
    }

    fn enqueue(&self, outcome: PortResult<ProcessingResult>) -> PortResult<()> {
        self.outcomes
            .lock()
            .map_err(|_| scripted_lock_error("outcome queue"))?
            .push_back(outcome);
        Ok(())
    }

    /// Replaces the health response returned by subsequent probes.
    pub fn set_health(&self, health: PortResult<ProcessorHealth>) -> PortResult<()> {
        *self
            .health
            .lock()
            .map_err(|_| scripted_lock_error("health"))? = health;
        Ok(())
    }

    /// Returns complete requests in invocation order.
    pub fn requests(&self) -> PortResult<Vec<DataProcessingRequest>> {
        self.requests
            .lock()
            .map(|requests| requests.clone())
            .map_err(|_| scripted_lock_error("request log"))
    }

    /// Returns the number of queued outcomes not yet consumed.
    pub fn queued_outcomes(&self) -> PortResult<usize> {
        self.outcomes
            .lock()
            .map(|outcomes| outcomes.len())
            .map_err(|_| scripted_lock_error("outcome queue"))
    }
}

#[async_trait]
impl DataProcessor for ScriptedDataProcessor {
    fn descriptor(&self) -> &DataProcessorDescriptor {
        &self.descriptor
    }

    async fn health(&self) -> PortResult<ProcessorHealth> {
        self.health
            .lock()
            .map_err(|_| scripted_lock_error("health"))?
            .clone()
    }

    async fn process(&self, request: DataProcessingRequest) -> PortResult<ProcessingResult> {
        self.requests
            .lock()
            .map_err(|_| scripted_lock_error("request log"))?
            .push(request);
        self.outcomes
            .lock()
            .map_err(|_| scripted_lock_error("outcome queue"))?
            .pop_front()
            .ok_or_else(|| {
                PortError::new(
                    PortErrorKind::Permanent,
                    "scripted processor response queue is empty",
                )
            })?
    }
}

fn scripted_lock_error(resource: &str) -> PortError {
    PortError::new(
        PortErrorKind::Permanent,
        format!("scripted processor {resource} lock was poisoned"),
    )
}

/// Verifies the required read/write and ordered batch behavior of `LiveState`.
pub async fn assert_live_state_round_trip(
    reader: &dyn LiveState,
    writer: &dyn LiveStateWriter,
    first: PointSample,
    second: PointSample,
) -> PortResult<()> {
    writer.write(first).await?;
    writer.write(second).await?;

    if reader.read(first.address()).await? != Some(first) {
        return Err(contract_error("live-state single read did not round trip"));
    }

    let actual = reader
        .read_many(&[second.address(), first.address()])
        .await?;
    if actual != vec![Some(second), Some(first)] {
        return Err(contract_error(
            "live-state batch read did not preserve input order",
        ));
    }

    Ok(())
}

/// Verifies FIFO visibility and acknowledgement behavior of `DurableOutbox`.
pub async fn assert_outbox_fifo(
    outbox: &dyn DurableOutbox,
    first: OutboxMessage,
    second: OutboxMessage,
) -> PortResult<()> {
    let first_id = outbox.enqueue(first).await?;
    let second_id = outbox.enqueue(second).await?;
    let pending = outbox.peek(2).await?;

    if pending.len() != 2 || pending[0].id() != first_id || pending[1].id() != second_id {
        return Err(contract_error(
            "outbox did not expose entries in FIFO order",
        ));
    }

    if outbox.acknowledge(&[first_id]).await? != 1 {
        return Err(contract_error("outbox did not acknowledge the first entry"));
    }

    let remaining = outbox.peek(2).await?;
    if remaining.len() != 1 || remaining[0].id() != second_id {
        return Err(contract_error(
            "outbox acknowledgement removed the wrong entry",
        ));
    }

    Ok(())
}

fn contract_error(message: &str) -> PortError {
    PortError::new(PortErrorKind::InvalidData, message)
}
