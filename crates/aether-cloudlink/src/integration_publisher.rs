//! Session-bound publisher for the two durable Integration streams.

use std::sync::Arc;

use crate::{
    CandidateMessage, CloudLinkCodec, CloudLinkIntegrationExtension, SessionBinding,
    UplinkAuthentication,
};
use aether_ports::{
    CloudLinkRecord, CloudLinkRecordIdentity, CloudLinkSpool, CloudLinkTransport,
    CloudLinkTransportEvent, CloudLinkTransportMessage, CloudLinkTransportRoute, DurableAckOutcome,
    PortError, PortErrorKind, PortResult,
};

const REPLAY_PAGE_SIZE: usize = 256;

/// Observable outcome of processing one transport event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudLinkIntegrationPublishEvent {
    /// A connection caused all retained records to be offered again.
    Connected {
        /// Number of records offered across both streams.
        offered: usize,
    },
    /// The transport disconnected; retained records remain untouched.
    Disconnected,
    /// One transport publication was recorded without removing the record.
    TransportPublished,
    /// A cloud application receipt was written back to its durable stream.
    DurableAck(DurableAckOutcome),
    /// A server-authoritative replay cursor was served.
    Replay {
        /// Number of retained records offered.
        offered: usize,
    },
}

/// Publishes retained Integration facts over any real CloudLink transport.
///
/// Construction requires a verified current session. The MQTT adapter can be
/// supplied directly as the transport; session establishment and credential
/// signing remain outside this publisher. MQTT PUBACK only advances local
/// publication evidence. Records are removed exclusively by a strict
/// current-session application ACK.
pub struct CloudLinkIntegrationPublisher {
    extension: CloudLinkIntegrationExtension,
    topology_spool: Arc<dyn CloudLinkSpool>,
    observation_spool: Arc<dyn CloudLinkSpool>,
    transport: Arc<dyn CloudLinkTransport>,
    session: SessionBinding,
    authentication: UplinkAuthentication,
}

impl CloudLinkIntegrationPublisher {
    /// Binds two extension-owned streams to one verified CloudLink session.
    #[must_use]
    pub fn new(
        extension: CloudLinkIntegrationExtension,
        topology_spool: Arc<dyn CloudLinkSpool>,
        observation_spool: Arc<dyn CloudLinkSpool>,
        transport: Arc<dyn CloudLinkTransport>,
        session: SessionBinding,
        authentication: UplinkAuthentication,
    ) -> Self {
        Self {
            extension,
            topology_spool,
            observation_spool,
            transport,
            session,
            authentication,
        }
    }

    /// Waits for and processes one connection, PUBACK, ACK, or replay event.
    pub async fn receive_and_process(&self) -> PortResult<CloudLinkIntegrationPublishEvent> {
        let event = self.transport.receive().await?;
        self.process_event(event).await
    }

    /// Processes one already-received transport event.
    pub async fn process_event(
        &self,
        event: CloudLinkTransportEvent,
    ) -> PortResult<CloudLinkIntegrationPublishEvent> {
        match event {
            CloudLinkTransportEvent::Connected => {
                let topology = self.offer_all(self.topology_spool.as_ref()).await?;
                let observations = self.offer_all(self.observation_spool.as_ref()).await?;
                Ok(CloudLinkIntegrationPublishEvent::Connected {
                    offered: topology.saturating_add(observations),
                })
            },
            CloudLinkTransportEvent::Disconnected => {
                Ok(CloudLinkIntegrationPublishEvent::Disconnected)
            },
            CloudLinkTransportEvent::TransportPublished(identity) => {
                let spool = self.spool_for_identity(&identity).await?;
                spool
                    .mark_transport_published(&identity, &self.session.spool_binding())
                    .await
                    .map_err(spool_error)?;
                Ok(CloudLinkIntegrationPublishEvent::TransportPublished)
            },
            CloudLinkTransportEvent::Inbound(message) => self.process_inbound(message).await,
        }
    }

    async fn process_inbound(
        &self,
        message: CloudLinkTransportMessage,
    ) -> PortResult<CloudLinkIntegrationPublishEvent> {
        match message.route() {
            CloudLinkTransportRoute::AckDown => {
                let CandidateMessage::DurableAck(message) =
                    CloudLinkCodec::decode(message.payload()).map_err(codec_error)?
                else {
                    return Err(rejected(
                        "CloudLink ACK route carried a non-acknowledgement message",
                    ));
                };
                let ack = message.to_spool_ack(&self.session).map_err(codec_error)?;
                let spool = self
                    .spool_for_stream(ack.stream_id(), ack.stream_epoch())
                    .await?;
                let outcome = spool.acknowledge(&ack).await.map_err(spool_error)?;
                Ok(CloudLinkIntegrationPublishEvent::DurableAck(outcome))
            },
            CloudLinkTransportRoute::ReplayDown => {
                let CandidateMessage::ReplayRequest(request) =
                    CloudLinkCodec::decode(message.payload()).map_err(codec_error)?
                else {
                    return Err(rejected(
                        "CloudLink replay route carried a non-replay message",
                    ));
                };
                request
                    .validate_session(&self.session)
                    .map_err(codec_error)?;
                let spool = self
                    .spool_for_stream(request.stream_id(), request.stream_epoch())
                    .await?;
                let offered = self
                    .offer_from(spool.as_ref(), request.from_position())
                    .await?;
                Ok(CloudLinkIntegrationPublishEvent::Replay { offered })
            },
            _ => Err(rejected(
                "Integration publisher received a message on an unsupported downlink route",
            )),
        }
    }

    async fn offer_all(&self, spool: &dyn CloudLinkSpool) -> PortResult<usize> {
        let status = spool.status().await.map_err(spool_error)?;
        if status.pending_records() == 0 {
            return Ok(0);
        }
        self.offer_from(spool, status.earliest_retained_position())
            .await
    }

    async fn offer_from(
        &self,
        spool: &dyn CloudLinkSpool,
        from_position: u64,
    ) -> PortResult<usize> {
        let status = spool.status().await.map_err(spool_error)?;
        let mut cursor = from_position;
        let mut offered = 0_usize;
        while cursor < status.next_position() {
            let window = spool
                .replay_from(cursor, REPLAY_PAGE_SIZE)
                .await
                .map_err(spool_error)?;
            if window.data_loss().is_some() {
                return Err(PortError::new(
                    PortErrorKind::Conflict,
                    "Integration CloudLink replay cursor predates retained durable data",
                ));
            }
            if window.records().is_empty() {
                break;
            }
            for record in window.records() {
                self.offer_record(spool, record).await?;
                cursor = record
                    .identity()
                    .position()
                    .checked_add(1)
                    .ok_or_else(|| permanent("CloudLink stream position exhausted"))?;
                offered = offered.saturating_add(1);
            }
        }
        Ok(offered)
    }

    async fn offer_record(
        &self,
        spool: &dyn CloudLinkSpool,
        record: &CloudLinkRecord,
    ) -> PortResult<()> {
        let route = self
            .extension
            .route_for_record(record)
            .map_err(codec_error)?;
        spool
            .mark_offered(record.identity(), &self.session.spool_binding())
            .await
            .map_err(spool_error)?;
        let payload = self.encoded_payload(record)?;
        self.transport
            .send(CloudLinkTransportMessage::new(
                route,
                payload,
                Some(record.identity().clone()),
            ))
            .await
    }

    fn encoded_payload(&self, record: &CloudLinkRecord) -> PortResult<Vec<u8>> {
        let envelope =
            CloudLinkCodec::delivery_envelope(&self.session, record, None, &self.authentication)
                .map_err(codec_error)?;
        CloudLinkCodec::encode(&envelope).map_err(codec_error)
    }

    async fn spool_for_identity(
        &self,
        identity: &CloudLinkRecordIdentity,
    ) -> PortResult<&Arc<dyn CloudLinkSpool>> {
        self.spool_for_stream(identity.stream_id(), identity.stream_epoch())
            .await
    }

    async fn spool_for_stream(
        &self,
        stream_id: &str,
        stream_epoch: u64,
    ) -> PortResult<&Arc<dyn CloudLinkSpool>> {
        let topology = self.topology_spool.status().await.map_err(spool_error)?;
        if topology.stream_id() == stream_id && topology.stream_epoch() == stream_epoch {
            return Ok(&self.topology_spool);
        }
        let observations = self.observation_spool.status().await.map_err(spool_error)?;
        if observations.stream_id() == stream_id && observations.stream_epoch() == stream_epoch {
            return Ok(&self.observation_spool);
        }
        Err(rejected(
            "CloudLink event does not belong to either Integration stream",
        ))
    }
}

fn codec_error(error: impl std::fmt::Display) -> PortError {
    PortError::new(
        PortErrorKind::InvalidData,
        format!("Integration CloudLink message is invalid: {error}"),
    )
}

fn spool_error(error: impl std::fmt::Display) -> PortError {
    PortError::new(
        PortErrorKind::Unavailable,
        format!("Integration CloudLink durable spool is unavailable: {error}"),
    )
}

fn rejected(message: &str) -> PortError {
    PortError::new(PortErrorKind::Rejected, message)
}

fn permanent(message: &str) -> PortError {
    PortError::new(PortErrorKind::Permanent, message)
}
