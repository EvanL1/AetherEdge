use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aether_cloudlink::{
    CandidateMessage, CloudLinkCodec, DataLossPayload, GatewaySessionAuthenticator,
    HeartbeatMessage, ResumeCursor, SessionBinding, SessionChallengeRequest, TopologyBinding,
    UplinkAuthentication,
};
use aether_cloudlink_mqtt::{
    DeploymentSecurity, MqttCloudLinkManager, MqttCloudLinkTransport, TopicNamespace,
};
use aether_domain::TimestampMs;
use aether_ports::{
    ClaimedGatewayIdentitySource, CloudLinkDataLossEvidence, CloudLinkMessageKind,
    CloudLinkReceiptRetention, CloudLinkRecord, CloudLinkSpool, CloudLinkTransport,
    CloudLinkTransportEvent, CloudLinkTransportMessage, CloudLinkTransportRoute, DurableAckOutcome,
};
use aether_store_local::{
    CloudLinkChallengeReservation, FileClaimedGatewayIdentitySource, FileCloudLinkChallengeLedger,
    FileCloudLinkSpool,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngCore as _;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use sqlx::SqlitePool;
use tokio::sync::{Notify, RwLock};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::config::CloudLinkSettings;
use crate::live_values::UplinkTopologyHandle;

/// Read-only process status shared with the loopback health route.
pub struct CloudLinkRuntimeStatus {
    enabled: bool,
    task_running: AtomicBool,
    transport_connected: AtomicBool,
    session_established: AtomicBool,
    session_epoch: AtomicU64,
    last_error: RwLock<Option<String>>,
}

impl CloudLinkRuntimeStatus {
    #[must_use]
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            task_running: AtomicBool::new(false),
            transport_connected: AtomicBool::new(false),
            session_established: AtomicBool::new(false),
            session_epoch: AtomicU64::new(0),
            last_error: RwLock::new(None),
        }
    }

    pub async fn snapshot(&self) -> Value {
        json!({
            "configured": self.enabled,
            "task_running": self.task_running.load(Ordering::Relaxed),
            "transport_connected": self.transport_connected.load(Ordering::Relaxed),
            "session_established": self.session_established.load(Ordering::Relaxed),
            "session_epoch": self.session_epoch.load(Ordering::Relaxed),
            "last_error": self.last_error.read().await.clone(),
        })
    }

    #[must_use]
    pub fn ready(&self) -> bool {
        !self.enabled
            || (self.task_running.load(Ordering::Relaxed)
                && self.session_established.load(Ordering::Relaxed))
    }

    async fn record_error(&self, message: impl Into<String>) {
        *self.last_error.write().await = Some(message.into());
    }

    async fn clear_error(&self) {
        *self.last_error.write().await = None;
    }
}

struct RuntimeTaskGuard {
    status: Arc<CloudLinkRuntimeStatus>,
}

impl Drop for RuntimeTaskGuard {
    fn drop(&mut self) {
        self.status.task_running.store(false, Ordering::Relaxed);
        self.status
            .transport_connected
            .store(false, Ordering::Relaxed);
        self.status
            .session_established
            .store(false, Ordering::Relaxed);
    }
}

/// The only production binary composition for cloud transport and replay.
pub struct CloudLinkRuntime {
    settings: CloudLinkSettings,
    gateway_id: String,
    transport: Arc<MqttCloudLinkTransport>,
    spool: Arc<FileCloudLinkSpool>,
    challenge_ledger: Arc<FileCloudLinkChallengeLedger>,
    authenticator: GatewaySessionAuthenticator,
    uplink_authentication: UplinkAuthentication,
    topology: Arc<UplinkTopologyHandle>,
    sqlite: SqlitePool,
    status: Arc<CloudLinkRuntimeStatus>,
    delivery_wake: Arc<Notify>,
}

impl CloudLinkRuntime {
    pub async fn compose(
        settings: CloudLinkSettings,
        spool: Arc<FileCloudLinkSpool>,
        topology: Arc<UplinkTopologyHandle>,
        sqlite: SqlitePool,
        status: Arc<CloudLinkRuntimeStatus>,
        delivery_wake: Arc<Notify>,
    ) -> anyhow::Result<(Self, MqttCloudLinkManager)> {
        let identity_source =
            FileClaimedGatewayIdentitySource::new(&settings.identity_directory)
                .map_err(|error| anyhow::anyhow!("invalid Gateway identity source: {error}"))?;
        let identity = identity_source
            .load_claimed_identity()
            .await
            .map_err(|error| anyhow::anyhow!("cannot read claimed Gateway identity: {error}"))?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "CloudLink requires a claimed Gateway identity; run `aether gateway enroll` first"
                )
            })?;
        let gateway_id = identity.target().gateway_id().to_string();
        let gateway_key_id = format!("gateway-{}", identity.fingerprint().as_str());
        let authenticator = GatewaySessionAuthenticator::from_base64url(
            settings.cloud_key_id.clone(),
            settings.cloud_verifying_key.clone(),
            gateway_key_id,
            URL_SAFE_NO_PAD.encode(identity.private_seed().expose()),
        )?;
        let uplink_authentication = authenticator.uplink_authentication();
        let topics = TopicNamespace::new(&settings.topic_prefix, &gateway_id)?;
        let (transport, transport_manager) = MqttCloudLinkTransport::new(
            settings.mqtt_config(&gateway_id),
            topics,
            DeploymentSecurity::Production,
        )?;
        let challenge_ledger = Arc::new(FileCloudLinkChallengeLedger::open(
            &settings.challenge_ledger_path,
            settings.challenge_ledger_capacity,
        )?);
        create_session_state(&sqlite).await?;

        let runtime = Self {
            settings,
            gateway_id,
            transport,
            spool,
            challenge_ledger,
            authenticator,
            uplink_authentication,
            topology,
            sqlite,
            status,
            delivery_wake,
        };
        Ok((runtime, transport_manager))
    }

    pub async fn run(self, shutdown: CancellationToken) -> anyhow::Result<()> {
        self.status.task_running.store(true, Ordering::Relaxed);
        let _task_guard = RuntimeTaskGuard {
            status: Arc::clone(&self.status),
        };
        let mut session = None::<EstablishedSession>;
        let mut pending_request = None::<Vec<u8>>;
        let mut pending_challenge_id = None::<String>;
        let mut runtime_manifest_resolved = false;
        let mut maintenance = tokio::time::interval(Duration::from_secs(1));
        maintenance.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut telemetry = tokio::time::interval(Duration::from_secs(
            self.settings.telemetry_interval_secs.max(1),
        ));
        telemetry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => return Ok(()),
                event = self.transport.receive() => {
                    let event = event.map_err(|error| anyhow::anyhow!("CloudLink transport stopped: {error}"))?;
                    match event {
                        CloudLinkTransportEvent::Connected => {
                            self.status.transport_connected.store(true, Ordering::Relaxed);
                            self.status.session_established.store(false, Ordering::Relaxed);
                            pending_challenge_id = None;
                            session = None;
                            match self.publish_challenge_request().await {
                                Ok(request) => {
                                    pending_request = Some(request);
                                    self.status.clear_error().await;
                                },
                                Err(error) => {
                                    self.status.record_error(error.to_string()).await;
                                    return Err(error);
                                },
                            }
                        },
                        CloudLinkTransportEvent::Disconnected => {
                            self.status.transport_connected.store(false, Ordering::Relaxed);
                            self.status.session_established.store(false, Ordering::Relaxed);
                            pending_request = None;
                            pending_challenge_id = None;
                            session = None;
                        },
                        CloudLinkTransportEvent::TransportPublished(identity) => {
                            if let Some(established) = &session
                                && let Err(error) = self.spool
                                    .mark_transport_published(&identity, &established.binding.spool_binding())
                                    .await
                            {
                                warn!(%error, "CloudLink PUBACK state was not applicable to the current session");
                            }
                        },
                        CloudLinkTransportEvent::Inbound(message) => {
                            if let Err(error) = self.handle_inbound(
                                message,
                                &mut session,
                                pending_request.as_deref(),
                                &mut pending_challenge_id,
                            ).await {
                                self.status.record_error(error.to_string()).await;
                                warn!(%error, "CloudLink inbound message failed closed");
                            }
                        },
                    }
                },
                _ = maintenance.tick() => {
                    if let Some(established) = &mut session
                        && Instant::now() >= established.next_heartbeat
                    {
                        if let Err(error) = self.publish_heartbeat(established).await {
                            self.status.record_error(error.to_string()).await;
                            warn!(%error, "CloudLink heartbeat publication failed");
                        }
                        established.next_heartbeat = Instant::now() + established.heartbeat_interval;
                    }
                    if let Some(established) = &mut session {
                        if !runtime_manifest_resolved {
                            match self.ensure_runtime_manifest().await {
                                Ok(()) => {
                                    runtime_manifest_resolved = true;
                                },
                                Err(error) => {
                                    self.status.record_error(error.to_string()).await;
                                    warn!(%error, "CloudLink Runtime Manifest admission will retry after retained backlog drains");
                                },
                            }
                        }
                        if let Err(error) = self.pump_delivery(established).await {
                            self.status.record_error(error.to_string()).await;
                            warn!(%error, "CloudLink delivery safety pass retained local state");
                        }
                    }
                },
                _ = self.delivery_wake.notified() => {
                    if let Some(established) = &mut session
                        && let Err(error) = self.pump_delivery(established).await
                    {
                        self.status.record_error(error.to_string()).await;
                        warn!(%error, "CloudLink delivery wake retained local state");
                    }
                },
                _ = telemetry.tick() => {
                    if session.is_some()
                        && let Err(error) = self.collect_and_publish_telemetry().await
                    {
                        self.status.record_error(error.to_string()).await;
                        warn!(%error, "CloudLink telemetry pass retained local state");
                    }
                    if let Some(established) = &mut session
                        && let Err(error) = self.pump_delivery(established).await
                    {
                        self.status.record_error(error.to_string()).await;
                        warn!(%error, "CloudLink delivery after telemetry retained local state");
                    }
                },
            }
        }
    }

    async fn publish_challenge_request(&self) -> anyhow::Result<Vec<u8>> {
        let spool = self.spool.status().await?;
        let resume = vec![ResumeCursor::new(
            spool.stream_id(),
            spool.stream_epoch(),
            spool.last_acknowledged_position(),
        )?];
        let mut nonce = [0_u8; 32];
        rand::rngs::OsRng
            .try_fill_bytes(&mut nonce)
            .map_err(|error| anyhow::anyhow!("operating-system randomness unavailable: {error}"))?;
        let request = SessionChallengeRequest::new(
            &self.gateway_id,
            &self.settings.credential_id,
            self.settings.credential_generation,
            URL_SAFE_NO_PAD.encode(nonce),
            resume,
        )?;
        let encoded = CloudLinkCodec::encode(&request)?;
        let now = now_ms()?;
        let expires_at = now
            .checked_add(self.settings.challenge_request_ttl_ms)
            .ok_or_else(|| anyhow::anyhow!("CloudLink challenge request deadline overflow"))?;
        let request_bytes = encoded.clone();
        let pending = self
            .challenge_ledger_io(move |ledger| {
                ledger.prepare_request(&request_bytes, expires_at, now)
            })
            .await?;
        let payload = pending.payload().to_vec();
        self.transport
            .send(CloudLinkTransportMessage::new(
                CloudLinkTransportRoute::SessionUp,
                payload.clone(),
                None,
            ))
            .await?;
        Ok(payload)
    }

    async fn handle_inbound(
        &self,
        message: CloudLinkTransportMessage,
        session: &mut Option<EstablishedSession>,
        pending_request: Option<&[u8]>,
        pending_challenge_id: &mut Option<String>,
    ) -> anyhow::Result<()> {
        let decoded = CloudLinkCodec::decode(message.payload())?;
        match (message.route(), decoded) {
            (
                CloudLinkTransportRoute::SessionDown,
                CandidateMessage::SessionChallenge(challenge),
            ) => {
                let request_bytes = pending_request.ok_or_else(|| {
                    anyhow::anyhow!("CloudLink challenge arrived without a current request")
                })?;
                let now = now_ms()?;
                self.authenticator
                    .verify_challenge(&challenge, &self.gateway_id, now)?;
                let challenge_id = challenge.challenge_id().to_owned();
                let challenge_deadline = challenge.expires_at_ms()?;
                let challenge_bytes = message.payload().to_vec();
                let request_bytes = request_bytes.to_vec();
                let reservation_challenge_id = challenge_id.clone();
                let reservation = self
                    .challenge_ledger_io(move |ledger| {
                        ledger.reserve(
                            &reservation_challenge_id,
                            challenge_deadline,
                            &challenge_bytes,
                            &request_bytes,
                            now,
                        )
                    })
                    .await?;
                let hello = match reservation {
                    CloudLinkChallengeReservation::RetryHello(hello) => hello,
                    CloudLinkChallengeReservation::Prepare { challenge, request } => {
                        let challenge = match CloudLinkCodec::decode(&challenge)? {
                            CandidateMessage::SessionChallenge(value) => value,
                            _ => anyhow::bail!("persisted CloudLink challenge has the wrong kind"),
                        };
                        let request = match CloudLinkCodec::decode(&request)? {
                            CandidateMessage::SessionChallengeRequest(value) => value,
                            _ => anyhow::bail!("persisted CloudLink request has the wrong kind"),
                        };
                        let verified = self.authenticator.verify_challenge(
                            &challenge,
                            &self.gateway_id,
                            now,
                        )?;
                        let hello = self.authenticator.sign_hello(&verified, &request)?;
                        let hello = CloudLinkCodec::encode(&hello)?;
                        let hello_challenge_id = challenge_id.clone();
                        self.challenge_ledger_io(move |ledger| {
                            ledger.store_hello(&hello_challenge_id, &hello)
                        })
                        .await?
                    },
                };
                self.transport
                    .send(CloudLinkTransportMessage::new(
                        CloudLinkTransportRoute::SessionUp,
                        hello,
                        None,
                    ))
                    .await?;
                *pending_challenge_id = Some(challenge_id);
            },
            (CloudLinkTransportRoute::SessionDown, CandidateMessage::SessionAccepted(accepted)) => {
                let challenge_id = pending_challenge_id.as_deref().ok_or_else(|| {
                    anyhow::anyhow!("CloudLink acceptance was not preceded by a verified challenge")
                })?;
                let previous_epoch = load_session_epoch(&self.sqlite).await?;
                let binding = accepted.bind(
                    &self.gateway_id,
                    self.settings.credential_generation,
                    previous_epoch,
                )?;
                validate_resume_cursors(&accepted, self.spool.as_ref()).await?;
                persist_session_epoch(&self.sqlite, binding.session_epoch()).await?;
                let completed_challenge_id = challenge_id.to_owned();
                self.challenge_ledger_io(move |ledger| ledger.complete(&completed_challenge_id))
                    .await?;
                let heartbeat_interval = Duration::from_millis(accepted.heartbeat_interval_ms()?);
                let next_offer_position = self.spool.status().await?.earliest_retained_position();
                self.status
                    .session_epoch
                    .store(binding.session_epoch(), Ordering::Relaxed);
                self.status
                    .session_established
                    .store(true, Ordering::Relaxed);
                self.status.clear_error().await;
                *session = Some(EstablishedSession {
                    binding,
                    heartbeat_interval,
                    next_heartbeat: Instant::now(),
                    next_offer_position,
                });
                *pending_challenge_id = None;
                if let Some(established) = session.as_mut() {
                    self.pump_delivery(established).await?;
                }
                info!("CloudLink session established");
            },
            (CloudLinkTransportRoute::AckDown, CandidateMessage::DurableAck(ack)) => {
                let established = session
                    .as_mut()
                    .ok_or_else(|| anyhow::anyhow!("CloudLink ACK arrived without a session"))?;
                let ack = ack.to_spool_ack(&established.binding)?;
                match self.spool.acknowledge(&ack).await? {
                    DurableAckOutcome::Applied { removed } => {
                        info!(removed, "CloudLink durable ACK applied");
                        self.pump_delivery(established).await?;
                    },
                    DurableAckOutcome::Duplicate => {},
                }
            },
            (CloudLinkTransportRoute::AckDown, CandidateMessage::Heartbeat(heartbeat)) => {
                let established = session.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("CloudLink heartbeat ACK arrived without a session")
                })?;
                heartbeat.validate_session(&established.binding)?;
            },
            (CloudLinkTransportRoute::ReplayDown, CandidateMessage::ReplayRequest(request)) => {
                let established = session
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("CloudLink replay arrived without a session"))?;
                request.validate_session(&established.binding)?;
                let status = self.spool.status().await?;
                if request.stream_id() != status.stream_id()
                    || request.stream_epoch()? != status.stream_epoch()
                {
                    anyhow::bail!("CloudLink replay requested another stream identity");
                }
                self.reoffer_from(established, request.from_position()?)
                    .await?;
            },
            _ => anyhow::bail!("CloudLink message kind is not allowed on this route"),
        }
        Ok(())
    }

    async fn publish_heartbeat(&self, established: &EstablishedSession) -> anyhow::Result<()> {
        let spool = self.spool.status().await?;
        let cursor = ResumeCursor::new(
            spool.stream_id(),
            spool.stream_epoch(),
            spool.last_acknowledged_position(),
        )?;
        let heartbeat = HeartbeatMessage::new(
            &established.binding,
            TimestampMs::new(now_ms()?),
            vec![cursor],
            &self.uplink_authentication,
        )?;
        self.transport
            .send(CloudLinkTransportMessage::new(
                CloudLinkTransportRoute::HeartbeatUp,
                CloudLinkCodec::encode(&heartbeat)?,
                None,
            ))
            .await?;
        Ok(())
    }

    async fn challenge_ledger_io<T, F>(&self, operation: F) -> anyhow::Result<T>
    where
        T: Send + 'static,
        F: FnOnce(
                &FileCloudLinkChallengeLedger,
            ) -> Result<T, aether_store_local::CloudLinkChallengeLedgerError>
            + Send
            + 'static,
    {
        let ledger = Arc::clone(&self.challenge_ledger);
        tokio::task::spawn_blocking(move || operation(ledger.as_ref()))
            .await
            .map_err(|error| anyhow::anyhow!("CloudLink challenge storage task failed: {error}"))?
            .map_err(Into::into)
    }

    async fn ensure_runtime_manifest(&self) -> anyhow::Result<()> {
        ensure_runtime_manifest_record(self.spool.as_ref(), &self.settings.runtime_manifest_path)
            .await
    }

    async fn collect_and_publish_telemetry(&self) -> anyhow::Result<()> {
        let generation = self.topology.load();
        let samples = generation.collect_point_samples(&["inst:*:M".to_owned()], &[])?;
        if samples.is_empty() {
            return Ok(());
        }
        let topology = TopologyBinding::new(
            generation.publication_epoch(),
            generation.cloudlink_snapshot_digest(),
        )?;
        let created_at = TimestampMs::new(now_ms()?);
        for chunk in samples.chunks(aether_cloudlink::MAX_POINT_SAMPLES) {
            let batch = CloudLinkCodec::telemetry_batch(topology.clone(), chunk)?;
            let batch_id = format!("telemetry-{}", fresh_batch_token()?);
            let input = CloudLinkCodec::prepare(
                CloudLinkMessageKind::TelemetryBatch,
                batch_id,
                &batch,
                created_at,
                None,
            )?;
            self.spool
                .admit_lossless(input, CloudLinkReceiptRetention::DiscardAfterAck)
                .await?;
        }
        Ok(())
    }

    async fn pump_delivery(&self, established: &mut EstablishedSession) -> anyhow::Result<()> {
        pump_delivery_window(
            self.spool.as_ref(),
            self.transport.as_ref(),
            &established.binding,
            &self.uplink_authentication,
            self.settings.request_capacity,
            &mut established.next_offer_position,
        )
        .await
    }

    async fn reoffer_from(
        &self,
        established: &EstablishedSession,
        from_position: u64,
    ) -> anyhow::Result<()> {
        reoffer_delivery_window(
            self.spool.as_ref(),
            self.transport.as_ref(),
            &established.binding,
            &self.uplink_authentication,
            self.settings.request_capacity,
            from_position,
        )
        .await
    }
}

async fn pump_delivery_window<S, T>(
    spool: &S,
    transport: &T,
    binding: &SessionBinding,
    authentication: &UplinkAuthentication,
    request_capacity: usize,
    next_offer_position: &mut u64,
) -> anyhow::Result<()>
where
    S: CloudLinkSpool + ?Sized,
    T: CloudLinkTransport + ?Sized,
{
    if request_capacity == 0 {
        anyhow::bail!("CloudLink delivery request capacity must be greater than zero");
    }
    let status = spool.status().await?;
    ensure_data_loss_record(spool, status.data_loss()).await?;
    let status = spool.status().await?;
    *next_offer_position = (*next_offer_position).max(status.earliest_retained_position());
    if *next_offer_position > status.next_position() {
        anyhow::bail!("CloudLink delivery cursor advanced beyond durable local state");
    }
    let current_session = binding.spool_binding();
    let mut remaining = request_capacity;
    let page_size = request_capacity.min(64);
    while remaining > 0 && *next_offer_position < status.next_position() {
        let window = spool.replay_from(*next_offer_position, page_size).await?;
        if window.data_loss().is_some() {
            anyhow::bail!("CloudLink delivery cursor entered a non-retained position gap");
        }
        if window.records().is_empty() {
            break;
        }
        for record in window.records() {
            let following_position = record
                .identity()
                .position()
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("CloudLink stream position exhausted"))?;
            if record.offered_session() != Some(&current_session) {
                offer_record(spool, transport, binding, authentication, record).await?;
                remaining -= 1;
            }
            *next_offer_position = following_position;
            if remaining == 0 {
                break;
            }
        }
    }
    Ok(())
}

async fn reoffer_delivery_window<S, T>(
    spool: &S,
    transport: &T,
    binding: &SessionBinding,
    authentication: &UplinkAuthentication,
    request_capacity: usize,
    from_position: u64,
) -> anyhow::Result<()>
where
    S: CloudLinkSpool + ?Sized,
    T: CloudLinkTransport + ?Sized,
{
    if request_capacity == 0 {
        anyhow::bail!("CloudLink replay request capacity must be greater than zero");
    }
    let status = spool.status().await?;
    ensure_data_loss_record(spool, status.data_loss()).await?;
    let status = spool.status().await?;
    if from_position == 0 || from_position > status.next_position() {
        spool.replay_from(from_position, 1).await?;
    }
    if from_position < status.earliest_retained_position() {
        let gap = spool.replay_from(from_position, 1).await?;
        if gap.data_loss().is_none() {
            anyhow::bail!("CloudLink replay cursor predates retained durable state");
        }
    }
    if from_position == status.next_position() {
        return Ok(());
    }

    let current_session = binding.spool_binding();
    let mut cursor = status.earliest_retained_position();
    let mut remaining = request_capacity;
    let page_size = request_capacity.min(64);
    while remaining > 0 && cursor < status.next_position() {
        let window = spool.replay_from(cursor, page_size).await?;
        if window.data_loss().is_some() || window.records().is_empty() {
            anyhow::bail!("CloudLink replay traversal encountered inconsistent retained state");
        }
        for record in window.records() {
            let following_position = record
                .identity()
                .position()
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("CloudLink stream position exhausted"))?;
            let requested_reoffer = record.identity().position() >= from_position;
            if requested_reoffer || record.offered_session() != Some(&current_session) {
                offer_record(spool, transport, binding, authentication, record).await?;
                remaining -= 1;
            }
            cursor = following_position;
            if remaining == 0 {
                break;
            }
        }
    }
    Ok(())
}

async fn ensure_data_loss_record<S>(
    spool: &S,
    evidence: Option<&CloudLinkDataLossEvidence>,
) -> anyhow::Result<Option<CloudLinkRecord>>
where
    S: CloudLinkSpool + ?Sized,
{
    if let Some(evidence) = evidence {
        let payload = DataLossPayload::from_evidence(evidence);
        let batch_id = format!(
            "data-loss-{}-{}-{}",
            evidence.stream_epoch(),
            evidence.first_lost_position(),
            evidence.last_lost_position()
        );
        let input = CloudLinkCodec::prepare(
            CloudLinkMessageKind::DataLoss,
            batch_id,
            &payload,
            evidence.recorded_at(),
            None,
        )?;
        let admission = spool.admit_data_loss(input, evidence).await?;
        return Ok(admission.pending_record().cloned());
    }
    Ok(None)
}

async fn offer_record<S, T>(
    spool: &S,
    transport: &T,
    binding: &SessionBinding,
    authentication: &UplinkAuthentication,
    record: &CloudLinkRecord,
) -> anyhow::Result<()>
where
    S: CloudLinkSpool + ?Sized,
    T: CloudLinkTransport + ?Sized,
{
    let envelope = CloudLinkCodec::delivery_envelope(binding, record, None, authentication)?;
    transport
        .send(CloudLinkTransportMessage::new(
            route_for(record.message_kind()),
            CloudLinkCodec::encode(&envelope)?,
            Some(record.identity().clone()),
        ))
        .await?;
    spool
        .mark_offered(record.identity(), &binding.spool_binding())
        .await?;
    Ok(())
}

async fn ensure_runtime_manifest_record<S>(
    spool: &S,
    runtime_manifest_path: &std::path::Path,
) -> anyhow::Result<()>
where
    S: CloudLinkSpool + ?Sized,
{
    let bytes = tokio::fs::read(runtime_manifest_path)
        .await
        .map_err(|error| {
            anyhow::anyhow!(
                "cannot read Runtime Manifest {}: {error}",
                runtime_manifest_path.display()
            )
        })?;
    let manifest_identity = URL_SAFE_NO_PAD.encode(Sha256::digest(&bytes));
    let batch_prefix = format!("manifest-{manifest_identity}-");
    let status = spool.status().await?;
    let mut cursor = status.earliest_retained_position();
    while cursor < status.next_position() {
        let retained = spool.replay_from(cursor, 64).await?;
        let records = retained.records();
        if records.is_empty() {
            anyhow::bail!(
                "CloudLink Runtime Manifest scan encountered inconsistent retained state"
            );
        }
        for record in records {
            if record.message_kind() == CloudLinkMessageKind::RuntimeManifestReport
                && record.batch_id().starts_with(&batch_prefix)
            {
                return Ok(());
            }
            cursor = record
                .identity()
                .position()
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("CloudLink stream position exhausted"))?;
        }
    }
    let observed_at = TimestampMs::new(now_ms()?);
    let report = CloudLinkCodec::runtime_manifest_report(&bytes, observed_at)?;
    let batch_id = format!("{batch_prefix}{}", fresh_batch_token()?);
    let input = CloudLinkCodec::prepare(
        CloudLinkMessageKind::RuntimeManifestReport,
        batch_id,
        &report,
        observed_at,
        None,
    )?;
    spool
        .admit_lossless(input, CloudLinkReceiptRetention::DiscardAfterAck)
        .await?;
    Ok(())
}

struct EstablishedSession {
    binding: SessionBinding,
    heartbeat_interval: Duration,
    next_heartbeat: Instant,
    next_offer_position: u64,
}

fn route_for(kind: CloudLinkMessageKind) -> CloudLinkTransportRoute {
    match kind {
        CloudLinkMessageKind::RuntimeManifestReport => CloudLinkTransportRoute::ManifestUp,
        CloudLinkMessageKind::TelemetryBatch => CloudLinkTransportRoute::TelemetryUp,
        CloudLinkMessageKind::AlarmEvent => CloudLinkTransportRoute::AlarmUp,
        CloudLinkMessageKind::IntegrationTopologySnapshot => {
            CloudLinkTransportRoute::IntegrationTopologyUp
        },
        CloudLinkMessageKind::IntegrationObservationBatch => {
            CloudLinkTransportRoute::IntegrationObservationsUp
        },
        CloudLinkMessageKind::DataLoss => CloudLinkTransportRoute::DataLossUp,
    }
}

async fn validate_resume_cursors(
    accepted: &aether_cloudlink::SessionAccepted,
    spool: &dyn CloudLinkSpool,
) -> anyhow::Result<()> {
    let status = spool.status().await?;
    let cursor = accepted
        .resume_cursors()
        .iter()
        .find(|cursor| cursor.stream_id() == status.stream_id())
        .ok_or_else(|| {
            anyhow::anyhow!("CloudLink acceptance omitted the business stream cursor")
        })?;
    if cursor.stream_epoch()? != status.stream_epoch()
        || cursor.acknowledged_position()? != status.last_acknowledged_position()
    {
        anyhow::bail!("CloudLink acceptance cursor conflicts with durable local state");
    }
    Ok(())
}

async fn create_session_state(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS cloudlink_session_state (
            singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
            session_epoch TEXT NOT NULL
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT OR IGNORE INTO cloudlink_session_state (singleton, session_epoch) VALUES (1, '0')",
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn load_session_epoch(pool: &SqlitePool) -> anyhow::Result<u64> {
    let value: String =
        sqlx::query_scalar("SELECT session_epoch FROM cloudlink_session_state WHERE singleton = 1")
            .fetch_one(pool)
            .await?;
    value
        .parse()
        .map_err(|error| anyhow::anyhow!("stored CloudLink session epoch is invalid: {error}"))
}

async fn persist_session_epoch(pool: &SqlitePool, epoch: u64) -> anyhow::Result<()> {
    let current = load_session_epoch(pool).await?;
    if epoch <= current {
        anyhow::bail!("CloudLink session epoch did not increase monotonically");
    }
    sqlx::query("UPDATE cloudlink_session_state SET session_epoch = ? WHERE singleton = 1")
        .bind(epoch.to_string())
        .execute(pool)
        .await?;
    Ok(())
}

pub fn now_ms() -> anyhow::Result<u64> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| anyhow::anyhow!("system clock is before the Unix epoch"))?;
    u64::try_from(elapsed.as_millis())
        .map_err(|_| anyhow::anyhow!("system clock exceeds CloudLink timestamp range"))
}

fn fresh_batch_token() -> anyhow::Result<String> {
    let mut bytes = [0_u8; 16];
    rand::rngs::OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|error| anyhow::anyhow!("operating-system randomness unavailable: {error}"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::Path;

    use aether_ports::{
        CloudLinkDurableAck, CloudLinkEnqueue, CloudLinkRecordIdentity, PortError, PortErrorKind,
        PortResult,
    };
    use async_trait::async_trait;
    use tokio::sync::Mutex;

    use super::*;

    const GATEWAY_ID: &str = "33333333-3333-4333-8333-333333333333";
    const SESSION_ID: &str = "44444444-4444-4444-8444-444444444444";

    #[derive(Default)]
    struct RecordingTransport {
        sent: Mutex<Vec<CloudLinkTransportMessage>>,
    }

    #[async_trait]
    impl CloudLinkTransport for RecordingTransport {
        async fn send(&self, message: CloudLinkTransportMessage) -> PortResult<()> {
            self.sent.lock().await.push(message);
            Ok(())
        }

        async fn receive(&self) -> PortResult<CloudLinkTransportEvent> {
            Err(PortError::new(
                PortErrorKind::Unavailable,
                "recording transport has no inbound stream",
            ))
        }
    }

    fn session() -> SessionBinding {
        SessionBinding::new(GATEWAY_ID, SESSION_ID, 1, 1).expect("session")
    }

    fn manifest_path() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config.template/runtime-manifest.json")
    }

    fn manifest_input(batch_id: &str, observed_at: u64) -> CloudLinkEnqueue {
        let bytes = std::fs::read(manifest_path()).expect("Runtime Manifest fixture");
        let observed_at = TimestampMs::new(observed_at);
        let report =
            CloudLinkCodec::runtime_manifest_report(&bytes, observed_at).expect("manifest report");
        CloudLinkCodec::prepare(
            CloudLinkMessageKind::RuntimeManifestReport,
            batch_id,
            &report,
            observed_at,
            None,
        )
        .expect("manifest input")
    }

    async fn acknowledge_through(
        spool: &FileCloudLinkSpool,
        binding: &SessionBinding,
        record: &CloudLinkRecord,
    ) {
        spool
            .acknowledge(&CloudLinkDurableAck::new(
                binding.spool_binding(),
                record.identity().stream_id(),
                record.identity().stream_epoch(),
                record.identity().position(),
                record.batch_id(),
                record.digest(),
                format!("receipt-{}", record.identity().position()),
            ))
            .await
            .expect("durable ACK");
    }

    fn accepted_with_cursor(acknowledged_position: u64) -> aether_cloudlink::SessionAccepted {
        let encoded = serde_json::to_vec(&json!({
            "schema": "aether.cloudlink.session-accepted",
            "protocol": "aether.cloudlink",
            "message_kind": "session-accepted",
            "gateway_id": GATEWAY_ID,
            "session_id": SESSION_ID,
            "session_epoch": "2",
            "credential_generation": "1",
            "server_time_ms": "1721000000123",
            "heartbeat_interval_ms": "30000",
            "resume": [{
                "stream_id": "business",
                "stream_epoch": "1",
                "acknowledged_position": acknowledged_position.to_string(),
            }],
        }))
        .expect("accepted session JSON");
        match CloudLinkCodec::decode(&encoded).expect("accepted session") {
            CandidateMessage::SessionAccepted(accepted) => accepted,
            other => panic!("unexpected CloudLink message: {other:?}"),
        }
    }

    #[test]
    fn every_business_kind_has_exactly_one_cloudlink_route() {
        assert_eq!(
            route_for(CloudLinkMessageKind::AlarmEvent),
            CloudLinkTransportRoute::AlarmUp
        );
        assert_eq!(
            route_for(CloudLinkMessageKind::TelemetryBatch),
            CloudLinkTransportRoute::TelemetryUp
        );
    }

    #[tokio::test]
    async fn session_epoch_state_is_monotonic_and_has_no_fallback_parser() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("pool");
        create_session_state(&pool).await.expect("schema");
        persist_session_epoch(&pool, 4).await.expect("epoch");
        assert_eq!(load_session_epoch(&pool).await.expect("load"), 4);
        assert!(persist_session_epoch(&pool, 4).await.is_err());
        sqlx::query(
            "UPDATE cloudlink_session_state SET session_epoch = 'not-an-integer' WHERE singleton = 1",
        )
        .execute(&pool)
        .await
        .expect("corrupt fixture");
        assert!(load_session_epoch(&pool).await.is_err());
    }

    #[tokio::test]
    async fn session_acceptance_requires_the_exact_durable_local_cursor() {
        let root = tempfile::tempdir().expect("temp dir");
        let spool = FileCloudLinkSpool::open(root.path().join("cloudlink.spool"), "business", 8)
            .expect("spool");
        let record = spool
            .admit_lossless(
                manifest_input("acknowledged-before-resume", 1_721_000_000_123),
                CloudLinkReceiptRetention::DiscardAfterAck,
            )
            .await
            .expect("admission")
            .pending_record()
            .expect("pending record")
            .clone();
        let binding = session();
        let transport = RecordingTransport::default();
        let mut next_offer_position = 1;
        pump_delivery_window(
            &spool,
            &transport,
            &binding,
            &UplinkAuthentication::trusted_connector_broker_attestation(),
            1,
            &mut next_offer_position,
        )
        .await
        .expect("offer acknowledged record");
        acknowledge_through(&spool, &binding, &record).await;
        assert_eq!(
            spool
                .status()
                .await
                .expect("status")
                .last_acknowledged_position(),
            1
        );

        validate_resume_cursors(&accepted_with_cursor(1), &spool)
            .await
            .expect("exact durable cursor");
        assert!(
            validate_resume_cursors(&accepted_with_cursor(0), &spool)
                .await
                .is_err(),
            "a server cursor behind discarded acknowledged payload must fail closed"
        );
        assert!(
            validate_resume_cursors(&accepted_with_cursor(2), &spool)
                .await
                .is_err(),
            "a server cursor ahead of durable local state must fail closed"
        );
    }

    #[tokio::test]
    async fn delivery_wake_sends_an_admission_after_an_initially_empty_window() {
        let root = tempfile::tempdir().expect("temp dir");
        let spool = Arc::new(
            FileCloudLinkSpool::open(root.path().join("cloudlink.spool"), "business", 8)
                .expect("spool"),
        );
        let transport = Arc::new(RecordingTransport::default());
        let binding = session();
        let authentication = UplinkAuthentication::trusted_connector_broker_attestation();
        let mut next_offer_position = 1;
        pump_delivery_window(
            spool.as_ref(),
            transport.as_ref(),
            &binding,
            &authentication,
            2,
            &mut next_offer_position,
        )
        .await
        .expect("empty initial pump");
        assert!(transport.sent.lock().await.is_empty());

        let wake = Arc::new(Notify::new());
        let worker_spool = Arc::clone(&spool);
        let worker_transport = Arc::clone(&transport);
        let worker_wake = Arc::clone(&wake);
        let worker = tokio::spawn(async move {
            worker_wake.notified().await;
            let mut next_offer_position = 1;
            pump_delivery_window(
                worker_spool.as_ref(),
                worker_transport.as_ref(),
                &binding,
                &authentication,
                2,
                &mut next_offer_position,
            )
            .await
        });
        spool
            .admit_lossless(
                manifest_input("late-admission", 1_721_000_000_123),
                CloudLinkReceiptRetention::DiscardAfterAck,
            )
            .await
            .expect("late admission");
        wake.notify_one();

        tokio::time::timeout(Duration::from_secs(1), worker)
            .await
            .expect("bounded delivery wake")
            .expect("delivery worker")
            .expect("delivery pump");
        let sent = transport.sent.lock().await;
        assert_eq!(sent.len(), 1);
        assert_eq!(
            sent[0].delivery().map(CloudLinkRecordIdentity::position),
            Some(1)
        );
    }

    #[tokio::test]
    async fn durable_acks_advance_every_bounded_delivery_window_without_reoffers() {
        let root = tempfile::tempdir().expect("temp dir");
        let spool = FileCloudLinkSpool::open(root.path().join("cloudlink.spool"), "business", 8)
            .expect("spool");
        let mut records = Vec::new();
        for position in 1_u64..=5 {
            let admission = spool
                .admit_lossless(
                    manifest_input(&format!("window-{position}"), 1_721_000_000_000 + position),
                    CloudLinkReceiptRetention::DiscardAfterAck,
                )
                .await
                .expect("admission");
            records.push(admission.pending_record().expect("pending").clone());
        }
        let transport = RecordingTransport::default();
        let binding = session();
        let authentication = UplinkAuthentication::trusted_connector_broker_attestation();
        let mut next_offer_position = 1;

        pump_delivery_window(
            &spool,
            &transport,
            &binding,
            &authentication,
            2,
            &mut next_offer_position,
        )
        .await
        .expect("first window");
        acknowledge_through(&spool, &binding, &records[1]).await;
        pump_delivery_window(
            &spool,
            &transport,
            &binding,
            &authentication,
            2,
            &mut next_offer_position,
        )
        .await
        .expect("second window");
        acknowledge_through(&spool, &binding, &records[3]).await;
        pump_delivery_window(
            &spool,
            &transport,
            &binding,
            &authentication,
            2,
            &mut next_offer_position,
        )
        .await
        .expect("third window");

        let positions = transport
            .sent
            .lock()
            .await
            .iter()
            .filter_map(|message| message.delivery().map(CloudLinkRecordIdentity::position))
            .collect::<Vec<_>>();
        assert_eq!(positions, vec![1, 2, 3, 4, 5]);
    }

    #[tokio::test]
    async fn capacity_one_offers_the_ordinary_prefix_before_the_data_loss_report() {
        let root = tempfile::tempdir().expect("temp dir");
        let spool = FileCloudLinkSpool::open(root.path().join("cloudlink.spool"), "business", 1)
            .expect("spool");
        spool
            .enqueue(manifest_input("evicted", 1_721_000_000_001))
            .await
            .expect("first lossy record");
        spool
            .enqueue(manifest_input("retained", 1_721_000_000_002))
            .await
            .expect("overflowing record");
        assert!(spool.status().await.expect("status").data_loss().is_some());

        let transport = RecordingTransport::default();
        let binding = session();
        let authentication = UplinkAuthentication::trusted_connector_broker_attestation();
        let mut next_offer_position = 1;
        pump_delivery_window(
            &spool,
            &transport,
            &binding,
            &authentication,
            1,
            &mut next_offer_position,
        )
        .await
        .expect("ordinary prefix pump");
        let first_pass = transport.sent.lock().await.clone();
        assert_eq!(first_pass.len(), 1, "one pump must honor its send budget");
        assert_eq!(first_pass[0].route(), CloudLinkTransportRoute::ManifestUp);
        drop(first_pass);
        let status = spool.status().await.expect("status after prefix offer");
        let report_before_second_pump = spool
            .replay_from(
                status.earliest_retained_position(),
                status.pending_records(),
            )
            .await
            .expect("retained records")
            .records()
            .iter()
            .find(|record| record.message_kind() == CloudLinkMessageKind::DataLoss)
            .expect("pending data-loss report")
            .clone();
        assert!(
            report_before_second_pump.offered_session().is_none(),
            "the cumulative report may not leapfrog its retained prefix"
        );
        pump_delivery_window(
            &spool,
            &transport,
            &binding,
            &authentication,
            1,
            &mut next_offer_position,
        )
        .await
        .expect("data-loss report pump");
        let sent = transport.sent.lock().await.clone();
        assert_eq!(sent.len(), 2, "one system report plus one ordinary window");
        assert_eq!(sent[0].route(), CloudLinkTransportRoute::ManifestUp);
        assert_eq!(sent[1].route(), CloudLinkTransportRoute::DataLossUp);
        let report_position = sent[1]
            .delivery()
            .expect("data-loss delivery identity")
            .position();
        drop(sent);

        let retained = spool.replay_from(report_position, 1).await.expect("replay");
        let report = retained
            .records()
            .iter()
            .find(|record| record.message_kind() == CloudLinkMessageKind::DataLoss)
            .expect("data-loss report")
            .clone();
        acknowledge_through(&spool, &binding, &report).await;
        assert!(spool.status().await.expect("status").data_loss().is_none());

        pump_delivery_window(
            &spool,
            &transport,
            &binding,
            &authentication,
            1,
            &mut next_offer_position,
        )
        .await
        .expect("post-ACK pump");
        assert_eq!(transport.sent.lock().await.len(), 2);
    }

    #[tokio::test]
    async fn evidence_expansion_after_report_admission_requires_one_new_ordered_report() {
        let root = tempfile::tempdir().expect("temp dir");
        let spool = FileCloudLinkSpool::open(root.path().join("cloudlink.spool"), "business", 1)
            .expect("spool");
        spool
            .enqueue(manifest_input("lost-one", 1_721_000_000_001))
            .await
            .expect("first record");
        spool
            .enqueue(manifest_input("lost-two-later", 1_721_000_000_002))
            .await
            .expect("first overflow");
        let transport = RecordingTransport::default();
        let binding = session();
        let authentication = UplinkAuthentication::trusted_connector_broker_attestation();
        let mut next_offer_position = 1;
        pump_delivery_window(
            &spool,
            &transport,
            &binding,
            &authentication,
            1,
            &mut next_offer_position,
        )
        .await
        .expect("first ordinary prefix");
        pump_delivery_window(
            &spool,
            &transport,
            &binding,
            &authentication,
            1,
            &mut next_offer_position,
        )
        .await
        .expect("first report");
        let first_report_position = transport.sent.lock().await[1]
            .delivery()
            .expect("data-loss delivery identity")
            .position();
        let first_report = spool
            .replay_from(first_report_position, 1)
            .await
            .expect("replay")
            .records()
            .iter()
            .find(|record| record.message_kind() == CloudLinkMessageKind::DataLoss)
            .expect("first report")
            .clone();

        spool
            .enqueue(manifest_input(
                "retained-after-expansion",
                1_721_000_000_003,
            ))
            .await
            .expect("evidence expansion");
        let expanded = spool.status().await.expect("expanded status");
        let expanded = expanded.data_loss().expect("expanded evidence");
        assert_eq!(expanded.first_lost_position(), 1);
        assert_eq!(expanded.last_lost_position(), 2);
        acknowledge_through(&spool, &binding, &first_report).await;
        assert!(
            spool.status().await.expect("status").data_loss().is_some(),
            "ACK for the old snapshot must not clear expanded evidence"
        );

        pump_delivery_window(
            &spool,
            &transport,
            &binding,
            &authentication,
            1,
            &mut next_offer_position,
        )
        .await
        .expect("expanded ordinary prefix");
        pump_delivery_window(
            &spool,
            &transport,
            &binding,
            &authentication,
            1,
            &mut next_offer_position,
        )
        .await
        .expect("replacement report");
        let sent = transport.sent.lock().await;
        let report_positions = sent
            .iter()
            .filter(|message| message.route() == CloudLinkTransportRoute::DataLossUp)
            .filter_map(|message| message.delivery().map(CloudLinkRecordIdentity::position))
            .collect::<Vec<_>>();
        assert_eq!(report_positions.len(), 2);
        assert_ne!(report_positions[0], report_positions[1]);
    }

    #[tokio::test]
    async fn pending_manifest_is_adopted_after_reopen_instead_of_conflicting() {
        let root = tempfile::tempdir().expect("temp dir");
        let path = root.path().join("cloudlink.spool");
        let first = FileCloudLinkSpool::open(&path, "business", 8).expect("first spool");
        ensure_runtime_manifest_record(&first, &manifest_path())
            .await
            .expect("first manifest");
        let original = first
            .replay_from(1, 8)
            .await
            .expect("first replay")
            .records()[0]
            .clone();
        drop(first);

        let reopened = FileCloudLinkSpool::open(&path, "business", 8).expect("reopened spool");
        ensure_runtime_manifest_record(&reopened, &manifest_path())
            .await
            .expect("adopt pending manifest");
        let retained = reopened.replay_from(1, 8).await.expect("retained");
        assert_eq!(retained.records().len(), 1);
        assert_eq!(retained.records()[0].batch_id(), original.batch_id());
        assert_eq!(retained.records()[0].digest(), original.digest());
    }

    #[tokio::test]
    async fn changed_manifest_is_admitted_alongside_the_stale_pending_report_after_reopen() {
        let root = tempfile::tempdir().expect("temp dir");
        let spool_path = root.path().join("cloudlink.spool");
        let changed_manifest_path = root.path().join("runtime-manifest.json");
        let mut changed_manifest =
            std::fs::read(manifest_path()).expect("Runtime Manifest fixture");
        changed_manifest.extend_from_slice(b" \n");
        std::fs::write(&changed_manifest_path, changed_manifest).expect("changed manifest fixture");

        let first = FileCloudLinkSpool::open(&spool_path, "business", 8).expect("first spool");
        ensure_runtime_manifest_record(&first, &manifest_path())
            .await
            .expect("original manifest");
        let original = first
            .replay_from(1, 8)
            .await
            .expect("first replay")
            .records()[0]
            .clone();
        drop(first);

        let reopened =
            FileCloudLinkSpool::open(&spool_path, "business", 8).expect("reopened spool");
        ensure_runtime_manifest_record(&reopened, &changed_manifest_path)
            .await
            .expect("changed manifest admission");
        let retained = reopened.replay_from(1, 8).await.expect("retained");
        assert_eq!(retained.records().len(), 2);
        assert_eq!(retained.records()[0].identity(), original.identity());
        assert_ne!(retained.records()[1].batch_id(), original.batch_id());
    }

    #[tokio::test]
    async fn changed_manifest_at_capacity_retries_only_after_the_stale_report_is_acked() {
        let root = tempfile::tempdir().expect("temp dir");
        let spool_path = root.path().join("cloudlink.spool");
        let changed_manifest_path = root.path().join("runtime-manifest.json");
        let mut changed_manifest =
            std::fs::read(manifest_path()).expect("Runtime Manifest fixture");
        changed_manifest.extend_from_slice(b" \n");
        std::fs::write(&changed_manifest_path, changed_manifest).expect("changed manifest fixture");

        let first = FileCloudLinkSpool::open(&spool_path, "business", 1).expect("first spool");
        ensure_runtime_manifest_record(&first, &manifest_path())
            .await
            .expect("original manifest");
        let original = first
            .replay_from(1, 1)
            .await
            .expect("first replay")
            .records()[0]
            .clone();
        drop(first);

        let reopened =
            FileCloudLinkSpool::open(&spool_path, "business", 1).expect("reopened spool");
        assert!(
            ensure_runtime_manifest_record(&reopened, &changed_manifest_path)
                .await
                .is_err(),
            "the current manifest must not evict its stale pending predecessor"
        );
        let retained = reopened
            .replay_from(1, 1)
            .await
            .expect("retained old report");
        assert_eq!(retained.records(), std::slice::from_ref(&original));

        let transport = RecordingTransport::default();
        let binding = session();
        let mut next_offer_position = 1;
        pump_delivery_window(
            &reopened,
            &transport,
            &binding,
            &UplinkAuthentication::trusted_connector_broker_attestation(),
            1,
            &mut next_offer_position,
        )
        .await
        .expect("offer stale report first");
        assert_eq!(
            transport.sent.lock().await[0]
                .delivery()
                .map(CloudLinkRecordIdentity::position),
            Some(original.identity().position())
        );
        acknowledge_through(&reopened, &binding, &original).await;

        ensure_runtime_manifest_record(&reopened, &changed_manifest_path)
            .await
            .expect("retry current manifest after capacity drains");
        let status = reopened.status().await.expect("current status");
        let retained = reopened
            .replay_from(status.earliest_retained_position(), 1)
            .await
            .expect("current report");
        assert_eq!(retained.records().len(), 1);
        assert_eq!(retained.records()[0].identity().position(), 2);
        assert_ne!(retained.records()[0].batch_id(), original.batch_id());
    }

    #[test]
    fn fresh_batch_tokens_are_unique_when_wall_clock_and_topology_are_unchanged() {
        let mut tokens = HashSet::new();
        for _ in 0..10_000 {
            assert!(tokens.insert(fresh_batch_token().expect("OS randomness")));
        }
    }
}
