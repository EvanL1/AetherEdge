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

use crate::{
    CLOUDLINK_MQTT_QOS, CLOUDLINK_MQTT_RETAIN, CloudLinkMqttConfig, CloudLinkMqttError,
    CloudLinkTlsConfig, DeploymentSecurity, TopicNamespace,
};

/// Reconnecting MQTT v3.1.1 CloudLink transport.
pub struct MqttCloudLinkTransport {
    outbound: mpsc::Sender<CloudLinkTransportMessage>,
    events: Mutex<mpsc::Receiver<PortResult<CloudLinkTransportEvent>>>,
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
        let maximum_packet_bytes = config.maximum_packet_bytes;
        tokio::spawn(run_manager(config, topics, outbound_rx, event_tx));
        Ok(Arc::new(Self {
            outbound,
            events: Mutex::new(events),
            maximum_packet_bytes,
        }))
    }
}

#[async_trait]
impl CloudLinkTransport for MqttCloudLinkTransport {
    async fn send(&self, message: CloudLinkTransportMessage) -> PortResult<()> {
        if message.payload().is_empty() || message.payload().len() > self.maximum_packet_bytes {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "CloudLink MQTT payload is empty or exceeds its configured bound",
            ));
        }
        let allowed = matches!(
            message.route(),
            CloudLinkTransportRoute::SessionUp
                | CloudLinkTransportRoute::HeartbeatUp
                | CloudLinkTransportRoute::ManifestUp
                | CloudLinkTransportRoute::TelemetryUp
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
                | CloudLinkTransportRoute::DataLossUp
        );
        if durable_route != message.delivery().is_some() {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "CloudLink durable routes require identity and session routes forbid it",
            ));
        }
        self.outbound.send(message).await.map_err(|_| {
            PortError::new(
                PortErrorKind::Unavailable,
                "CloudLink MQTT transport manager is unavailable",
            )
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
    mut outbound: mpsc::Receiver<CloudLinkTransportMessage>,
    events: mpsc::Sender<PortResult<CloudLinkTransportEvent>>,
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

        loop {
            tokio::select! {
                outgoing = outbound.recv() => {
                    let Some(message) = outgoing else {
                        outbound_closed = true;
                        break;
                    };
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
                            let Some(route) = topics.inbound_route(&publication.topic) else {
                                let _ = events
                                    .send(Err(PortError::new(
                                        PortErrorKind::InvalidData,
                                        "CloudLink MQTT inbound publication violated route, QoS, retain, or size policy",
                                    )))
                                    .await;
                                continue;
                            };
                            if !valid_transport {
                                let _ = events
                                    .send(Err(PortError::new(
                                        PortErrorKind::InvalidData,
                                        "CloudLink MQTT inbound publication violated route, QoS, retain, or size policy",
                                    )))
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
