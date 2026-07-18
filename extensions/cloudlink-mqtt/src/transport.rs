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
#[cfg(feature = "integration-control")]
use tokio::sync::oneshot;
use tokio::sync::{Mutex, mpsc};

#[cfg(feature = "integration-control")]
use crate::IntegrationControlTopicNamespace;
use crate::{
    CLOUDLINK_MQTT_QOS, CLOUDLINK_MQTT_RETAIN, CloudLinkMqttConfig, CloudLinkMqttError,
    CloudLinkTlsConfig, DeploymentSecurity, TopicNamespace,
};

enum ManagerCommand {
    Baseline(CloudLinkTransportMessage),
    #[cfg(feature = "integration-control")]
    EnableIntegrationControl {
        topics: IntegrationControlTopicNamespace,
        result: oneshot::Sender<PortResult<()>>,
    },
    #[cfg(feature = "integration-control")]
    IntegrationControlReceipt {
        payload: Vec<u8>,
        result: oneshot::Sender<PortResult<()>>,
    },
}

#[cfg(feature = "integration-control")]
#[derive(Default)]
struct IntegrationControlRouteState {
    topics: Option<IntegrationControlTopicNamespace>,
}

#[cfg(feature = "integration-control")]
impl IntegrationControlRouteState {
    fn is_active(&self) -> bool {
        self.topics.is_some()
    }

    fn activate(&mut self, topics: IntegrationControlTopicNamespace) -> PortResult<()> {
        if self.is_active() {
            return Err(PortError::new(
                PortErrorKind::Conflict,
                "Integration-control MQTT subscription is already active",
            ));
        }
        self.topics = Some(topics);
        Ok(())
    }

    fn matches_offer(&self, topic: &str) -> bool {
        self.topics
            .as_ref()
            .is_some_and(|topics| topic == topics.offer_topic())
    }

    fn receipt_topic(&self) -> PortResult<String> {
        self.topics
            .as_ref()
            .map(IntegrationControlTopicNamespace::receipt_topic)
            .ok_or_else(|| {
                PortError::new(
                    PortErrorKind::Rejected,
                    "Integration-control MQTT route is not active",
                )
            })
    }
}

/// Reconnecting MQTT v3.1.1 CloudLink transport.
pub struct MqttCloudLinkTransport {
    outbound: mpsc::Sender<ManagerCommand>,
    events: Mutex<mpsc::Receiver<PortResult<CloudLinkTransportEvent>>>,
    #[cfg(feature = "integration-control")]
    integration_control_offers: Mutex<mpsc::Receiver<PortResult<Vec<u8>>>>,
    maximum_packet_bytes: usize,
}

impl MqttCloudLinkTransport {
    /// Validates configuration and starts one isolated reconnecting MQTT owner.
    pub fn connect(
        config: CloudLinkMqttConfig,
        topics: TopicNamespace,
        security: DeploymentSecurity,
    ) -> Result<Arc<Self>, CloudLinkMqttError> {
        config.validate(security)?;
        let (outbound, outbound_rx) = mpsc::channel(config.request_capacity);
        let (event_tx, events) = mpsc::channel(config.request_capacity);
        #[cfg(feature = "integration-control")]
        let (integration_control_offer_tx, integration_control_offers) =
            mpsc::channel(config.request_capacity);
        let maximum_packet_bytes = config.maximum_packet_bytes;
        tokio::spawn(run_manager(
            config,
            topics,
            outbound_rx,
            event_tx,
            #[cfg(feature = "integration-control")]
            integration_control_offer_tx,
        ));
        Ok(Arc::new(Self {
            outbound,
            events: Mutex::new(events),
            #[cfg(feature = "integration-control")]
            integration_control_offers: Mutex::new(integration_control_offers),
            maximum_packet_bytes,
        }))
    }

    /// Activates the one exact governed-control offer subscription.
    ///
    /// The composition root must call this only after authenticating and
    /// persisting the current CloudLink session. Reconnection clears the
    /// subscription and requires a new authenticated call.
    #[cfg(feature = "integration-control")]
    pub async fn enable_integration_control(
        &self,
        topics: IntegrationControlTopicNamespace,
    ) -> PortResult<()> {
        let (result, response) = oneshot::channel();
        self.outbound
            .send(ManagerCommand::EnableIntegrationControl { topics, result })
            .await
            .map_err(|_| manager_unavailable())?;
        response.await.map_err(|_| manager_unavailable())?
    }

    /// Receives only exact, post-activation governed-control offer payloads.
    #[cfg(feature = "integration-control")]
    pub async fn receive_integration_control_offer(&self) -> PortResult<Vec<u8>> {
        self.integration_control_offers
            .lock()
            .await
            .recv()
            .await
            .unwrap_or_else(|| Err(manager_unavailable()))
    }

    /// Publishes one authenticated durable business receipt.
    ///
    /// MQTT PUBACK is deliberately not surfaced as an application durable
    /// acknowledgement and never removes the local receipt.
    #[cfg(feature = "integration-control")]
    pub async fn send_integration_control_receipt(&self, payload: Vec<u8>) -> PortResult<()> {
        validate_payload(&payload, self.maximum_packet_bytes)?;
        let (result, response) = oneshot::channel();
        self.outbound
            .send(ManagerCommand::IntegrationControlReceipt { payload, result })
            .await
            .map_err(|_| manager_unavailable())?;
        response.await.map_err(|_| manager_unavailable())?
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
            .send(ManagerCommand::Baseline(message))
            .await
            .map_err(|_| manager_unavailable())
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
    #[cfg(feature = "integration-control")] integration_control_offers: mpsc::Sender<
        PortResult<Vec<u8>>,
    >,
) {
    loop {
        let (client, mut event_loop) = match mqtt_client(&config) {
            Ok(value) => value,
            Err(error) => {
                let _ = events
                    .send(Err(PortError::new(
                        PortErrorKind::Permanent,
                        error.to_string(),
                    )))
                    .await;
                return;
            },
        };
        let mut waiting_packet_id = VecDeque::<Option<CloudLinkRecordIdentity>>::new();
        let mut inflight = BTreeMap::<u16, CloudLinkRecordIdentity>::new();
        let mut outbound_closed = false;
        #[cfg(feature = "integration-control")]
        let mut integration_control_routes = IntegrationControlRouteState::default();

        loop {
            tokio::select! {
                outgoing = outbound.recv() => {
                    let Some(command) = outgoing else {
                        outbound_closed = true;
                        break;
                    };
                    match command {
                        ManagerCommand::Baseline(message) => {
                            waiting_packet_id.push_back(message.delivery().cloned());
                            if client
                                .publish(
                                    topics.topic(message.route()),
                                    CLOUDLINK_MQTT_QOS,
                                    CLOUDLINK_MQTT_RETAIN,
                                    message.payload(),
                                )
                                .await
                                .is_err()
                            {
                                waiting_packet_id.pop_back();
                                break;
                            }
                        },
                        #[cfg(feature = "integration-control")]
                        ManagerCommand::EnableIntegrationControl {
                            topics: requested,
                            result,
                        } => {
                            if integration_control_routes.is_active() {
                                let _ = result.send(integration_control_routes.activate(requested));
                                continue;
                            }
                            if client
                                .subscribe(requested.offer_topic(), CLOUDLINK_MQTT_QOS)
                                .await
                                .is_err()
                            {
                                let _ = result.send(Err(manager_unavailable()));
                                break;
                            }
                            let _ = result.send(integration_control_routes.activate(requested));
                        },
                        #[cfg(feature = "integration-control")]
                        ManagerCommand::IntegrationControlReceipt { payload, result } => {
                            let receipt_topic = match integration_control_routes.receipt_topic() {
                                Ok(topic) => topic,
                                Err(error) => {
                                    let _ = result.send(Err(error));
                                    continue;
                                },
                            };
                            waiting_packet_id.push_back(None);
                            if client
                                .publish(
                                    receipt_topic,
                                    CLOUDLINK_MQTT_QOS,
                                    CLOUDLINK_MQTT_RETAIN,
                                    payload,
                                )
                                .await
                                .is_err()
                            {
                                waiting_packet_id.pop_back();
                                let _ = result.send(Err(manager_unavailable()));
                                break;
                            }
                            let _ = result.send(Ok(()));
                        },
                    }
                },
                event = event_loop.poll() => {
                    match event {
                        Ok(Event::Incoming(Incoming::ConnAck(_))) => {
                            let subscriptions = topics.subscribe_topics();
                            let mut failed = false;
                            for topic in subscriptions {
                                if client.subscribe(topic, CLOUDLINK_MQTT_QOS).await.is_err() {
                                    failed = true;
                                    break;
                                }
                            }
                            if failed {
                                break;
                            }
                            let _ = events.send(Ok(CloudLinkTransportEvent::Connected)).await;
                        },
                        Ok(Event::Outgoing(Outgoing::Publish(packet_id))) => {
                            if let Some(Some(identity)) = waiting_packet_id.pop_front() {
                                inflight.insert(packet_id, identity);
                            }
                        },
                        Ok(Event::Incoming(Incoming::PubAck(ack))) => {
                            if let Some(identity) = inflight.remove(&ack.pkid) {
                                let _ = events
                                    .send(Ok(CloudLinkTransportEvent::TransportPublished(identity)))
                                    .await;
                            }
                        },
                        Ok(Event::Incoming(Incoming::Publish(publication))) => {
                            let valid_transport = publication.qos == CLOUDLINK_MQTT_QOS
                                && !publication.retain
                                && publication.payload.len() <= config.maximum_packet_bytes;
                            #[cfg(feature = "integration-control")]
                            if integration_control_routes.matches_offer(&publication.topic) {
                                let event = if valid_transport {
                                    Ok(publication.payload.to_vec())
                                } else {
                                    Err(invalid_inbound_publication())
                                };
                                let _ = integration_control_offers.send(event).await;
                                continue;
                            }
                            let Some(route) = topics.inbound_route(&publication.topic) else {
                                let _ = events
                                    .send(Err(invalid_inbound_publication()))
                                    .await;
                                continue;
                            };
                            if !valid_transport {
                                let _ = events
                                    .send(Err(invalid_inbound_publication()))
                                    .await;
                                continue;
                            }
                            let message = CloudLinkTransportMessage::new(
                                route,
                                publication.payload.to_vec(),
                                None,
                            );
                            let _ = events
                                .send(Ok(CloudLinkTransportEvent::Inbound(message)))
                                .await;
                        },
                        Ok(Event::Incoming(Incoming::Disconnect)) | Err(_) => break,
                        Ok(_) => {},
                    }
                }
            }
        }
        let _ = client.disconnect().await;
        let _ = events.send(Ok(CloudLinkTransportEvent::Disconnected)).await;
        if outbound_closed {
            return;
        }
        tokio::time::sleep(Duration::from_secs(config.reconnect_delay_secs)).await;
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

fn invalid_inbound_publication() -> PortError {
    PortError::new(
        PortErrorKind::InvalidData,
        "CloudLink MQTT inbound publication violated route, QoS, retain, or size policy",
    )
}

#[cfg(all(test, feature = "integration-control"))]
mod integration_control_tests {
    use super::*;

    #[test]
    fn control_routes_are_connection_local_and_default_off() {
        let topics = IntegrationControlTopicNamespace::new(
            "aether-test",
            "33333333-3333-4333-8333-333333333333",
        )
        .expect("topics");
        let mut connection = IntegrationControlRouteState::default();
        assert!(!connection.matches_offer(&topics.offer_topic()));
        assert!(connection.receipt_topic().is_err());

        connection
            .activate(topics.clone())
            .expect("explicit activation");
        assert!(connection.matches_offer(&topics.offer_topic()));
        assert_eq!(
            connection.receipt_topic().expect("receipt route"),
            topics.receipt_topic()
        );
        assert!(connection.activate(topics.clone()).is_err());

        let mut reconnected = IntegrationControlRouteState::default();
        assert!(
            !reconnected.matches_offer(&topics.offer_topic()),
            "a reconnect requires a newly authenticated activation"
        );
        assert!(reconnected.receipt_topic().is_err());
        reconnected
            .activate(topics.clone())
            .expect("new session explicitly reactivates the routes");
        assert!(reconnected.matches_offer(&topics.offer_topic()));
        assert_eq!(
            reconnected
                .receipt_topic()
                .expect("reactivated receipt route"),
            topics.receipt_topic()
        );
    }
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
