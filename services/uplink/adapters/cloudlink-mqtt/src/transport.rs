//! Reconnecting rumqttc implementation of the transport-neutral CloudLink port.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use aether_ports::{
    CloudLinkRecordIdentity, CloudLinkTransport, CloudLinkTransportEvent,
    CloudLinkTransportMessage, CloudLinkTransportRoute, PortError, PortErrorKind, PortResult,
};
use async_trait::async_trait;
use rumqttc::tokio_native_tls::native_tls::{Certificate, Identity, TlsConnector};
use rumqttc::{
    AsyncClient, Event, Incoming, MqttOptions, NetworkOptions, Outgoing, TlsConfiguration,
    Transport,
};
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

use crate::{
    CLOUDLINK_MQTT_QOS, CLOUDLINK_MQTT_RETAIN, CloudLinkMqttConfig, CloudLinkMqttError,
    CloudLinkTlsConfig, DeploymentSecurity, TopicNamespace,
};

enum ManagerCommand {
    Baseline(CloudLinkTransportMessage),
}

/// Reconnecting MQTT v3.1.1 CloudLink transport.
pub struct MqttCloudLinkTransport {
    outbound: mpsc::Sender<ManagerCommand>,
    events: Mutex<mpsc::Receiver<PortResult<CloudLinkTransportEvent>>>,
    maximum_packet_bytes: usize,
}

/// Explicit reconnecting MQTT owner driven by the service task supervisor.
pub struct MqttCloudLinkManager {
    config: CloudLinkMqttConfig,
    topics: TopicNamespace,
    outbound: mpsc::Receiver<ManagerCommand>,
    events: mpsc::Sender<PortResult<CloudLinkTransportEvent>>,
}

impl MqttCloudLinkManager {
    /// Owns all broker I/O until service cancellation or a permanent failure.
    pub async fn run(self, shutdown: CancellationToken) -> PortResult<()> {
        run_manager(
            self.config,
            self.topics,
            self.outbound,
            self.events,
            shutdown,
        )
        .await
    }
}

impl MqttCloudLinkTransport {
    /// Validates configuration and returns an inert transport plus its manager.
    ///
    /// No task or network I/O starts until [`MqttCloudLinkManager::run`] is
    /// polled by the composition root.
    pub fn new(
        config: CloudLinkMqttConfig,
        topics: TopicNamespace,
        security: DeploymentSecurity,
    ) -> Result<(Arc<Self>, MqttCloudLinkManager), CloudLinkMqttError> {
        config.validate(security)?;
        let (outbound, outbound_rx) = mpsc::channel(config.request_capacity);
        let (event_tx, events) = mpsc::channel(config.request_capacity);
        let maximum_packet_bytes = config.maximum_packet_bytes;
        Ok((
            Arc::new(Self {
                outbound,
                events: Mutex::new(events),
                maximum_packet_bytes,
            }),
            MqttCloudLinkManager {
                config,
                topics,
                outbound: outbound_rx,
                events: event_tx,
            },
        ))
    }
}

#[async_trait]
impl CloudLinkTransport for MqttCloudLinkTransport {
    async fn send(&self, message: CloudLinkTransportMessage) -> PortResult<()> {
        validate_payload(message.payload(), self.maximum_packet_bytes)?;
        let allowed = matches!(
            message.route(),
            CloudLinkTransportRoute::SessionUp
                | CloudLinkTransportRoute::HeartbeatUp
                | CloudLinkTransportRoute::ManifestUp
                | CloudLinkTransportRoute::TelemetryUp
                | CloudLinkTransportRoute::AlarmUp
                | CloudLinkTransportRoute::IntegrationTopologyUp
                | CloudLinkTransportRoute::IntegrationObservationsUp
                | CloudLinkTransportRoute::DataLossUp
        );
        if !allowed {
            return Err(PortError::new(
                PortErrorKind::Rejected,
                "CloudLink edge transport cannot publish a downlink route",
            ));
        }
        let durable_route = matches!(
            message.route(),
            CloudLinkTransportRoute::ManifestUp
                | CloudLinkTransportRoute::TelemetryUp
                | CloudLinkTransportRoute::AlarmUp
                | CloudLinkTransportRoute::IntegrationTopologyUp
                | CloudLinkTransportRoute::IntegrationObservationsUp
                | CloudLinkTransportRoute::DataLossUp
        );
        if durable_route != message.delivery().is_some() {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "CloudLink durable routes require identity and session routes forbid it",
            ));
        }
        self.outbound
            .try_send(ManagerCommand::Baseline(message))
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => outbound_backpressure(),
                mpsc::error::TrySendError::Closed(_) => manager_unavailable(),
            })
    }

    async fn receive(&self) -> PortResult<CloudLinkTransportEvent> {
        self.events.lock().await.recv().await.unwrap_or_else(|| {
            Err(PortError::new(
                PortErrorKind::Unavailable,
                "CloudLink MQTT transport event stream ended",
            ))
        })
    }
}

async fn run_manager(
    config: CloudLinkMqttConfig,
    topics: TopicNamespace,
    mut outbound: mpsc::Receiver<ManagerCommand>,
    events: mpsc::Sender<PortResult<CloudLinkTransportEvent>>,
    shutdown: CancellationToken,
) -> PortResult<()> {
    // A command which has left the service-facing channel but has not yet
    // entered rumqttc must survive request-channel backpressure and reconnects.
    // In particular, the task which owns `event_loop.poll()` must never await a
    // full rumqttc request queue: only polling the event loop can make space in
    // that queue.
    let mut pending_outbound = None::<ManagerCommand>;
    loop {
        let (client, mut event_loop) = match mqtt_client(&config) {
            Ok(value) => value,
            Err(error) => {
                let error = PortError::new(PortErrorKind::Permanent, error.to_string());
                let _ = emit_event(
                    &events,
                    Err(PortError::new(PortErrorKind::Permanent, error.to_string())),
                );
                return Err(error);
            },
        };
        let mut waiting_packet_id = VecDeque::<Option<CloudLinkRecordIdentity>>::new();
        let mut inflight = BTreeMap::<u16, CloudLinkRecordIdentity>::new();
        let mut outbound_closed = false;
        let mut connection_acknowledged = false;
        let mut connected_announced = false;
        let mut pending_subscriptions = VecDeque::<String>::new();

        loop {
            if connection_acknowledged {
                while let Some(topic) = pending_subscriptions.front() {
                    if client
                        .try_subscribe(topic.clone(), CLOUDLINK_MQTT_QOS)
                        .is_err()
                    {
                        break;
                    }
                    pending_subscriptions.pop_front();
                }
                if pending_subscriptions.is_empty() && !connected_announced {
                    emit_event(&events, Ok(CloudLinkTransportEvent::Connected))?;
                    connected_announced = true;
                }
            }

            if connected_announced
                && let Some(ManagerCommand::Baseline(message)) = pending_outbound.as_ref()
                && client
                    .try_publish(
                        topics.topic(message.route()),
                        CLOUDLINK_MQTT_QOS,
                        CLOUDLINK_MQTT_RETAIN,
                        message.payload().to_vec(),
                    )
                    .is_ok()
            {
                waiting_packet_id.push_back(message.delivery().cloned());
                pending_outbound = None;
            }

            tokio::select! {
                _ = shutdown.cancelled() => {
                    let _ = client.try_disconnect();
                    return Ok(());
                },
                outgoing = outbound.recv(), if pending_outbound.is_none() => {
                    let Some(command) = outgoing else {
                        outbound_closed = true;
                        break;
                    };
                    pending_outbound = Some(command);
                },
                event = event_loop.poll() => {
                    match event {
                        Ok(Event::Incoming(Incoming::ConnAck(_))) => {
                            connection_acknowledged = true;
                            connected_announced = false;
                            pending_subscriptions = topics.subscribe_topics().into();
                        },
                        Ok(Event::Outgoing(Outgoing::Publish(packet_id))) => {
                            if let Some(Some(identity)) = waiting_packet_id.pop_front() {
                                inflight.insert(packet_id, identity);
                            }
                        },
                        Ok(Event::Incoming(Incoming::PubAck(ack))) => {
                            if let Some(identity) = inflight.remove(&ack.pkid) {
                                emit_event(
                                    &events,
                                    Ok(CloudLinkTransportEvent::TransportPublished(identity)),
                                )?;
                            }
                        },
                        Ok(Event::Incoming(Incoming::Publish(publication))) => {
                            let valid_transport = publication.qos == CLOUDLINK_MQTT_QOS
                                && !publication.retain
                                && publication.payload.len() <= config.maximum_packet_bytes;
                            let Some(route) = topics.inbound_route(&publication.topic) else {
                                emit_event(&events, Err(invalid_inbound_publication()))?;
                                continue;
                            };
                            if !valid_transport {
                                emit_event(&events, Err(invalid_inbound_publication()))?;
                                continue;
                            }
                            let message = CloudLinkTransportMessage::new(
                                route,
                                publication.payload.to_vec(),
                                None,
                            );
                            emit_event(&events, Ok(CloudLinkTransportEvent::Inbound(message)))?;
                        },
                        Ok(Event::Incoming(Incoming::Disconnect)) | Err(_) => break,
                        Ok(_) => {},
                    }
                }
            }
        }
        let _ = client.try_disconnect();
        emit_event(&events, Ok(CloudLinkTransportEvent::Disconnected))?;
        if outbound_closed {
            return Ok(());
        }
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => return Ok(()),
            _ = tokio::time::sleep(Duration::from_secs(config.reconnect_delay_secs)) => {},
        }
    }
}

fn validate_payload(payload: &[u8], maximum_packet_bytes: usize) -> PortResult<()> {
    if payload.is_empty() || payload.len() > maximum_packet_bytes {
        return Err(PortError::new(
            PortErrorKind::InvalidData,
            "CloudLink MQTT payload is empty or exceeds its configured bound",
        ));
    }
    Ok(())
}

fn manager_unavailable() -> PortError {
    PortError::new(
        PortErrorKind::Unavailable,
        "CloudLink MQTT transport manager is unavailable",
    )
}

fn outbound_backpressure() -> PortError {
    PortError::new(
        PortErrorKind::Unavailable,
        "CloudLink MQTT outbound queue is full; durable delivery remains pending",
    )
}

fn emit_event(
    events: &mpsc::Sender<PortResult<CloudLinkTransportEvent>>,
    event: PortResult<CloudLinkTransportEvent>,
) -> PortResult<()> {
    events.try_send(event).map_err(|error| match error {
        mpsc::error::TrySendError::Full(_) => PortError::new(
            PortErrorKind::Unavailable,
            "CloudLink MQTT event consumer exceeded its bounded capacity",
        ),
        mpsc::error::TrySendError::Closed(_) => manager_unavailable(),
    })
}

fn invalid_inbound_publication() -> PortError {
    PortError::new(
        PortErrorKind::InvalidData,
        "CloudLink MQTT inbound publication violated route, QoS, retain, or size policy",
    )
}

fn mqtt_client(
    config: &CloudLinkMqttConfig,
) -> Result<(AsyncClient, rumqttc::EventLoop), CloudLinkMqttError> {
    let mut options = MqttOptions::new(&config.client_id, &config.broker_host, config.broker_port);
    options.set_keep_alive(Duration::from_secs(config.keep_alive_secs));
    options.set_clean_session(true);
    options.set_max_packet_size(config.maximum_packet_bytes, config.maximum_packet_bytes);
    options.set_request_channel_capacity(config.request_capacity);
    if let Some(username) = &config.username {
        options.set_credentials(
            username,
            config
                .password
                .as_ref()
                .map_or("", super::SecretString::expose),
        );
    }
    match &config.tls {
        CloudLinkTlsConfig::Disabled => {},
        CloudLinkTlsConfig::SystemRoots => {
            options.set_transport(Transport::tls_with_config(TlsConfiguration::Native));
        },
        CloudLinkTlsConfig::Custom {
            ca_path,
            client_identity,
        } => {
            let ca_bytes = std::fs::read(ca_path).map_err(|_| {
                CloudLinkMqttError::InvalidTlsMaterial("cannot read CA certificate")
            })?;
            let ca = Certificate::from_pem(&ca_bytes).map_err(|_| {
                CloudLinkMqttError::InvalidTlsMaterial("CA certificate is not valid PEM")
            })?;
            let mut connector = TlsConnector::builder();
            connector.add_root_certificate(ca);
            if let Some(identity) = client_identity {
                let certificate = std::fs::read(&identity.certificate_path).map_err(|_| {
                    CloudLinkMqttError::InvalidTlsMaterial("cannot read client certificate")
                })?;
                let private_key = std::fs::read(&identity.private_key_path).map_err(|_| {
                    CloudLinkMqttError::InvalidTlsMaterial("cannot read client private key")
                })?;
                let identity = Identity::from_pkcs8(&certificate, &private_key).map_err(|_| {
                    CloudLinkMqttError::InvalidTlsMaterial(
                        "client certificate/private key is not valid PKCS#8 PEM",
                    )
                })?;
                connector.identity(identity);
            }
            let connector = connector.build().map_err(|_| {
                CloudLinkMqttError::InvalidTlsMaterial("cannot build TLS connector")
            })?;
            options.set_transport(Transport::tls_with_config(
                TlsConfiguration::NativeConnector(connector),
            ));
        },
    }
    let (client, mut event_loop) = AsyncClient::new(options, config.request_capacity);
    let mut network_options = NetworkOptions::new();
    network_options.set_connection_timeout(30);
    event_loop.set_network_options(network_options);
    Ok((client, event_loop))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_backpressure_fails_closed_without_blocking_the_poll_owner() {
        let (events, _receiver) = mpsc::channel(1);
        emit_event(&events, Ok(CloudLinkTransportEvent::Connected)).expect("first event");

        let error = emit_event(&events, Ok(CloudLinkTransportEvent::Disconnected))
            .expect_err("a full event queue must fail the manager");

        assert_eq!(error.kind(), PortErrorKind::Unavailable);
        assert!(error.is_retryable());
    }
}
