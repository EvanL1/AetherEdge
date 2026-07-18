//! Bounded Home Assistant WebSocket client actor.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[cfg(feature = "integration-control")]
use aether_integration_control::ProviderAcceptance;
use aether_ports::{PortError, PortErrorKind, PortResult, SecretMaterial, SecretResolver};
use async_trait::async_trait;
use chrono::DateTime;
use futures::{SinkExt, StreamExt};
use serde_json::{Map, Value, json};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::AbortHandle;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async_with_config,
    tungstenite::{Error as WebSocketError, Message, protocol::WebSocketConfig},
};

#[cfg(feature = "integration-control")]
use crate::control::{HomeAssistantPowerRequest, decode_provider_acceptance};
use crate::{
    HomeAssistantArea, HomeAssistantConnectionConfig, HomeAssistantDevice, HomeAssistantEntity,
    HomeAssistantSnapshot, HomeAssistantState, HomeAssistantStateChanged, HomeAssistantTransport,
};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

const TRANSPORT_SHUTDOWN_MESSAGE: &str = "Home Assistant WebSocket transport is shut down";

enum ActorRequest {
    FetchSnapshot {
        reply: oneshot::Sender<PortResult<HomeAssistantSnapshot>>,
    },
    NextStateChanged {
        reply: oneshot::Sender<PortResult<HomeAssistantStateChanged>>,
    },
}

#[cfg(feature = "integration-control")]
struct ControlRequest {
    request: HomeAssistantPowerRequest,
    reply: oneshot::Sender<PortResult<ProviderAcceptance>>,
}

enum StateWaitOutcome {
    Shutdown,
    #[cfg(feature = "integration-control")]
    Control(Option<ControlRequest>),
    State(PortResult<HomeAssistantStateChanged>),
}

enum ActorInput {
    Shutdown,
    Request(Option<ActorRequest>),
    #[cfg(feature = "integration-control")]
    Control(Option<ControlRequest>),
}

/// Actor-backed Home Assistant WebSocket transport.
///
/// A single task owns the socket. Callers communicate through bounded channels,
/// so no mutex is held across network awaits and request IDs remain serialized.
/// Clones share that task: dropping a non-final clone has no effect, while
/// dropping the last clone aborts the task without blocking the runtime.
#[derive(Clone)]
pub struct WebSocketHomeAssistantTransport {
    lifecycle: Arc<TransportLifecycle>,
}

struct TransportLifecycle {
    requests: mpsc::Sender<ActorRequest>,
    #[cfg(feature = "integration-control")]
    control_requests: mpsc::Sender<ControlRequest>,
    shutdown: watch::Sender<bool>,
    actor_done: watch::Receiver<bool>,
    actor_abort: AbortHandle,
    stopped: Arc<AtomicBool>,
    shutdown_timeout: std::time::Duration,
}

impl TransportLifecycle {
    fn begin_shutdown(&self) {
        self.stopped.store(true, Ordering::Release);
        let _previous = self.shutdown.send_replace(true);
    }

    fn stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }
}

impl Drop for TransportLifecycle {
    fn drop(&mut self) {
        self.begin_shutdown();
        self.actor_abort.abort();
    }
}

struct ActorCompletion {
    actor_done: watch::Sender<bool>,
    stopped: Arc<AtomicBool>,
}

impl Drop for ActorCompletion {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        let _previous = self.actor_done.send_replace(true);
    }
}

impl WebSocketHomeAssistantTransport {
    /// Authenticates, subscribes to state changes, and starts the socket actor.
    pub async fn connect(
        config: HomeAssistantConnectionConfig,
        secrets: Arc<dyn SecretResolver>,
    ) -> PortResult<Self> {
        let session = Session::connect(&config, secrets.as_ref()).await?;
        let (requests, receiver) = mpsc::channel(config.actor_queue_capacity());
        #[cfg(feature = "integration-control")]
        let (control_requests, control_receiver) = mpsc::channel(config.actor_queue_capacity());
        let (shutdown, shutdown_receiver) = watch::channel(false);
        let (actor_done_sender, actor_done) = watch::channel(false);
        let stopped = Arc::new(AtomicBool::new(false));
        let shutdown_timeout = config.request_timeout();
        let actor = tokio::spawn(run_actor(
            config,
            secrets,
            session,
            receiver,
            #[cfg(feature = "integration-control")]
            control_receiver,
            shutdown_receiver,
            ActorCompletion {
                actor_done: actor_done_sender,
                stopped: Arc::clone(&stopped),
            },
        ));
        let actor_abort = actor.abort_handle();
        drop(actor);

        Ok(Self {
            lifecycle: Arc::new(TransportLifecycle {
                requests,
                #[cfg(feature = "integration-control")]
                control_requests,
                shutdown,
                actor_done,
                actor_abort,
                stopped,
                shutdown_timeout,
            }),
        })
    }

    /// Stops the shared socket actor and waits for bounded cleanup.
    ///
    /// All clones refer to the same actor. Shutting down through one clone
    /// permanently shuts down every clone.
    pub async fn shutdown(&self) -> PortResult<()> {
        self.lifecycle.begin_shutdown();
        let mut actor_done = self.lifecycle.actor_done.clone();
        if *actor_done.borrow() {
            return Ok(());
        }

        match timeout(self.lifecycle.shutdown_timeout, actor_done.changed()).await {
            Ok(Ok(())) if *actor_done.borrow() => Ok(()),
            Ok(Ok(())) => Err(unavailable("Home Assistant WebSocket actor stopped")),
            Ok(Err(_)) if self.lifecycle.actor_abort.is_finished() => Ok(()),
            Ok(Err(_)) => Err(unavailable("Home Assistant WebSocket actor stopped")),
            Err(_) => {
                self.lifecycle.actor_abort.abort();
                Err(timed_out(
                    "Home Assistant WebSocket transport shutdown timed out",
                ))
            },
        }
    }

    async fn request_snapshot(&self) -> PortResult<HomeAssistantSnapshot> {
        if self.lifecycle.stopped() {
            return Err(transport_shutdown());
        }
        let (reply, response) = oneshot::channel();
        let mut shutdown = self.lifecycle.shutdown.subscribe();
        if *shutdown.borrow() {
            return Err(transport_shutdown());
        }
        tokio::select! {
            biased;
            _ = shutdown.changed() => return Err(transport_shutdown()),
            result = self.lifecycle.requests.send(ActorRequest::FetchSnapshot { reply }) => {
                result.map_err(|_| self.actor_stopped_error())?;
            },
        }
        tokio::select! {
            biased;
            _ = shutdown.changed() => Err(transport_shutdown()),
            result = response => {
                result.map_err(|_| self.actor_stopped_error())?
            },
        }
    }

    async fn request_state_changed(&self) -> PortResult<HomeAssistantStateChanged> {
        if self.lifecycle.stopped() {
            return Err(transport_shutdown());
        }
        let (reply, response) = oneshot::channel();
        let mut shutdown = self.lifecycle.shutdown.subscribe();
        if *shutdown.borrow() {
            return Err(transport_shutdown());
        }
        tokio::select! {
            biased;
            _ = shutdown.changed() => return Err(transport_shutdown()),
            result = self.lifecycle.requests.send(ActorRequest::NextStateChanged { reply }) => {
                result.map_err(|_| self.actor_stopped_error())?;
            },
        }
        tokio::select! {
            biased;
            _ = shutdown.changed() => Err(transport_shutdown()),
            result = response => {
                result.map_err(|_| self.actor_stopped_error())?
            },
        }
    }

    #[cfg(feature = "integration-control")]
    pub(crate) async fn request_power(
        &self,
        request: HomeAssistantPowerRequest,
    ) -> PortResult<ProviderAcceptance> {
        if self.lifecycle.stopped() {
            return Err(transport_shutdown());
        }
        let (reply, response) = oneshot::channel();
        let mut shutdown = self.lifecycle.shutdown.subscribe();
        if *shutdown.borrow() {
            return Err(transport_shutdown());
        }
        tokio::select! {
            biased;
            _ = shutdown.changed() => return Err(transport_shutdown()),
            result = self.lifecycle.control_requests.send(ControlRequest { request, reply }) => {
                result.map_err(|_| self.actor_stopped_error())?;
            },
        }
        tokio::select! {
            biased;
            _ = shutdown.changed() => Err(transport_shutdown()),
            result = response => {
                result.map_err(|_| self.actor_stopped_error())?
            },
        }
    }

    fn actor_stopped_error(&self) -> PortError {
        if self.lifecycle.stopped() {
            transport_shutdown()
        } else {
            unavailable("Home Assistant WebSocket actor stopped")
        }
    }
}

#[async_trait]
impl HomeAssistantTransport for WebSocketHomeAssistantTransport {
    async fn fetch_snapshot(&self) -> PortResult<HomeAssistantSnapshot> {
        self.request_snapshot().await
    }

    async fn next_state_changed(&self) -> PortResult<HomeAssistantStateChanged> {
        self.request_state_changed().await
    }
}

async fn run_actor(
    config: HomeAssistantConnectionConfig,
    secrets: Arc<dyn SecretResolver>,
    initial_session: Session,
    mut requests: mpsc::Receiver<ActorRequest>,
    #[cfg(feature = "integration-control")] mut control_requests: mpsc::Receiver<ControlRequest>,
    mut shutdown: watch::Receiver<bool>,
    _completion: ActorCompletion,
) {
    let mut actor = WebSocketActor {
        config,
        secrets,
        session: Some(initial_session),
        resync_required: false,
    };
    'actor: loop {
        #[cfg(feature = "integration-control")]
        let input = tokio::select! {
            biased;
            changed = shutdown.changed() => {
                let _ignored = changed;
                ActorInput::Shutdown
            },
            control = control_requests.recv() => ActorInput::Control(control),
            request = requests.recv() => ActorInput::Request(request),
        };
        #[cfg(not(feature = "integration-control"))]
        let input = tokio::select! {
            biased;
            changed = shutdown.changed() => {
                let _ignored = changed;
                ActorInput::Shutdown
            },
            request = requests.recv() => ActorInput::Request(request),
        };
        let request = match input {
            ActorInput::Shutdown | ActorInput::Request(None) => break,
            #[cfg(feature = "integration-control")]
            ActorInput::Control(None) => break,
            #[cfg(feature = "integration-control")]
            ActorInput::Control(Some(control)) => {
                if execute_control(&mut actor, control, &mut shutdown).await {
                    break;
                }
                continue;
            },
            ActorInput::Request(Some(request)) => request,
        };
        match request {
            ActorRequest::FetchSnapshot { reply } => {
                let (result, shutting_down) = tokio::select! {
                    biased;
                    changed = shutdown.changed() => {
                        let _ignored = changed;
                        (Err(transport_shutdown()), true)
                    },
                    result = actor.fetch_snapshot() => (result, false),
                };
                let _ignored = reply.send(result);
                if shutting_down {
                    break;
                }
            },
            ActorRequest::NextStateChanged { reply } => {
                let deadline = tokio::time::Instant::now() + actor.config.request_timeout();
                #[cfg(feature = "integration-control")]
                loop {
                    let outcome = {
                        let state_wait = actor.next_state_changed_before(deadline);
                        tokio::pin!(state_wait);
                        tokio::select! {
                            biased;
                            changed = shutdown.changed() => {
                                let _ignored = changed;
                                StateWaitOutcome::Shutdown
                            },
                            control = control_requests.recv() => StateWaitOutcome::Control(control),
                            result = &mut state_wait => StateWaitOutcome::State(result),
                        }
                    };
                    match outcome {
                        StateWaitOutcome::Shutdown => {
                            let _ignored = reply.send(Err(transport_shutdown()));
                            break 'actor;
                        },
                        StateWaitOutcome::State(result) => {
                            let _ignored = reply.send(result);
                            break;
                        },
                        StateWaitOutcome::Control(None) => {
                            let _ignored = reply.send(Err(transport_shutdown()));
                            break 'actor;
                        },
                        StateWaitOutcome::Control(Some(control)) => {
                            if execute_control(&mut actor, control, &mut shutdown).await {
                                let _ignored = reply.send(Err(transport_shutdown()));
                                break 'actor;
                            }
                        },
                    }
                }
                #[cfg(not(feature = "integration-control"))]
                {
                    let state_wait = actor.next_state_changed_before(deadline);
                    tokio::pin!(state_wait);
                    let outcome = tokio::select! {
                        biased;
                        changed = shutdown.changed() => {
                            let _ignored = changed;
                            StateWaitOutcome::Shutdown
                        },
                        result = &mut state_wait => StateWaitOutcome::State(result),
                    };
                    match outcome {
                        StateWaitOutcome::Shutdown => {
                            let _ignored = reply.send(Err(transport_shutdown()));
                            break 'actor;
                        },
                        StateWaitOutcome::State(result) => {
                            let _ignored = reply.send(result);
                        },
                    }
                }
            },
        }
    }

    requests.close();
    while let Ok(request) = requests.try_recv() {
        match request {
            ActorRequest::FetchSnapshot { reply } => {
                let _ignored = reply.send(Err(transport_shutdown()));
            },
            ActorRequest::NextStateChanged { reply } => {
                let _ignored = reply.send(Err(transport_shutdown()));
            },
        }
    }
    #[cfg(feature = "integration-control")]
    {
        control_requests.close();
        while let Ok(control) = control_requests.try_recv() {
            let _ignored = control.reply.send(Err(transport_shutdown()));
        }
    }
}

#[cfg(feature = "integration-control")]
async fn execute_control(
    actor: &mut WebSocketActor,
    control: ControlRequest,
    shutdown: &mut watch::Receiver<bool>,
) -> bool {
    let (result, shutting_down) = tokio::select! {
        biased;
        changed = shutdown.changed() => {
            let _ignored = changed;
            (Err(transport_shutdown()), true)
        },
        result = actor.set_power(control.request) => (result, false),
    };
    let _ignored = control.reply.send(result);
    shutting_down
}

struct WebSocketActor {
    config: HomeAssistantConnectionConfig,
    secrets: Arc<dyn SecretResolver>,
    session: Option<Session>,
    resync_required: bool,
}

impl WebSocketActor {
    async fn ensure_connected(&mut self) -> PortResult<()> {
        if self.session.is_none() {
            self.session = Some(Session::connect(&self.config, self.secrets.as_ref()).await?);
        }
        Ok(())
    }

    async fn fetch_snapshot(&mut self) -> PortResult<HomeAssistantSnapshot> {
        let mut attempt = 0_u8;
        loop {
            let result = match self.ensure_connected().await {
                Ok(()) => match self.session.as_mut() {
                    Some(session) => session.fetch_snapshot(&self.config).await,
                    None => Err(unavailable(
                        "Home Assistant WebSocket session was not established",
                    )),
                },
                Err(error) => Err(error),
            };
            match result {
                Ok(snapshot) => {
                    self.resync_required = false;
                    return Ok(snapshot);
                },
                Err(error)
                    if error.is_retryable() && attempt < self.config.reconnect_attempts() =>
                {
                    self.session = None;
                    self.resync_required = true;
                    let multiplier = 1_u32 << u32::from(attempt.min(7));
                    sleep(self.config.reconnect_base_delay() * multiplier).await;
                    attempt = attempt.saturating_add(1);
                },
                Err(error) => {
                    if error.is_retryable() {
                        self.session = None;
                        self.resync_required = true;
                    }
                    return Err(error);
                },
            }
        }
    }

    async fn next_state_changed_before(
        &mut self,
        deadline: tokio::time::Instant,
    ) -> PortResult<HomeAssistantStateChanged> {
        if self.resync_required {
            return Err(resynchronization_required());
        }
        self.ensure_connected().await?;
        let result = match self.session.as_mut() {
            Some(session) => {
                tokio::time::timeout_at(deadline, session.next_state_changed(&self.config))
                    .await
                    .map_err(|_| timed_out("Home Assistant state wait timed out"))?
            },
            None => Err(unavailable(
                "Home Assistant WebSocket session was not established",
            )),
        };
        match result {
            Ok(event) => Ok(event),
            Err(error) if error.kind() == PortErrorKind::Timeout => Err(error),
            Err(error) if error.kind() == PortErrorKind::Unavailable => {
                self.session = None;
                self.resync_required = true;
                Err(resynchronization_required())
            },
            Err(error) if error.kind() == PortErrorKind::Conflict => {
                self.resync_required = true;
                Err(error)
            },
            Err(error) if error.kind() == PortErrorKind::InvalidData => {
                self.session = None;
                self.resync_required = true;
                Err(error)
            },
            Err(error) => Err(error),
        }
    }

    #[cfg(feature = "integration-control")]
    async fn set_power(
        &mut self,
        request: HomeAssistantPowerRequest,
    ) -> PortResult<ProviderAcceptance> {
        self.ensure_connected().await?;
        let result = match self.session.as_mut() {
            Some(session) => session.set_power(&self.config, &request).await,
            None => Err(unavailable(
                "Home Assistant WebSocket session was not established",
            )),
        };
        if result.as_ref().is_err_and(|error| error.is_retryable()) {
            self.session = None;
            self.resync_required = true;
        }
        result
    }
}

struct Session {
    socket: Socket,
    next_request_id: u64,
    subscription_id: u64,
    inbound: VecDeque<Value>,
    pending_events: VecDeque<Value>,
    state_event_fence: BTreeMap<String, u64>,
}

impl Session {
    async fn connect(
        config: &HomeAssistantConnectionConfig,
        secrets: &dyn SecretResolver,
    ) -> PortResult<Self> {
        let socket_config = WebSocketConfig::default()
            .read_buffer_size(16 * 1024)
            .write_buffer_size(16 * 1024)
            .max_write_buffer_size(config.max_message_bytes() * 2)
            .max_message_size(Some(config.max_message_bytes()))
            .max_frame_size(Some(config.max_message_bytes()));
        let connect = connect_async_with_config(config.websocket_url(), Some(socket_config), true);
        let (socket, _) = timeout(config.request_timeout(), connect)
            .await
            .map_err(|_| timed_out("Home Assistant WebSocket connect timed out"))?
            .map_err(connect_error)?;
        let mut session = Self {
            socket,
            next_request_id: 1,
            subscription_id: 0,
            inbound: VecDeque::new(),
            pending_events: VecDeque::new(),
            state_event_fence: BTreeMap::new(),
        };
        session.authenticate(config, secrets).await?;
        Ok(session)
    }

    async fn authenticate(
        &mut self,
        config: &HomeAssistantConnectionConfig,
        secrets: &dyn SecretResolver,
    ) -> PortResult<()> {
        let required = self.next_value(config).await?;
        if required.get("type").and_then(Value::as_str) != Some("auth_required") {
            return Err(invalid_data(
                "Home Assistant did not begin with auth_required",
            ));
        }

        let token = secrets
            .resolve(config.access_token_ref())
            .await
            .map_err(|error| {
                PortError::new(error.kind(), "Home Assistant credential resolution failed")
            })?;
        self.send_value(
            config,
            &json!({"type": "auth", "access_token": token.expose()}),
        )
        .await?;
        drop_secret(token);

        let authenticated = self.next_value(config).await?;
        match authenticated.get("type").and_then(Value::as_str) {
            Some("auth_ok") => {},
            Some("auth_invalid") => {
                return Err(PortError::new(
                    PortErrorKind::Rejected,
                    "Home Assistant rejected the configured credentials",
                ));
            },
            _ => {
                return Err(invalid_data(
                    "Home Assistant authentication reply is invalid",
                ));
            },
        }

        self.command(
            config,
            json!({
                "type": "supported_features",
                "features": {"coalesce_messages": 1}
            }),
        )
        .await?;
        self.subscription_id = self
            .send_command(config, json!({"type": "subscribe_events"}))
            .await?;
        self.wait_for_result_before_deadline(config, self.subscription_id)
            .await?;
        Ok(())
    }

    async fn fetch_snapshot(
        &mut self,
        config: &HomeAssistantConnectionConfig,
    ) -> PortResult<HomeAssistantSnapshot> {
        let areas = self
            .command(config, json!({"type": "config/area_registry/list"}))
            .await?;
        let devices = self
            .command(config, json!({"type": "config/device_registry/list"}))
            .await?;
        let entities = self
            .command(config, json!({"type": "config/entity_registry/list"}))
            .await?;
        let states = decode_states(
            self.command(config, json!({"type": "get_states"})).await?,
            config.max_collection_items(),
        )?;

        self.install_state_event_fence(&states);

        Ok(HomeAssistantSnapshot {
            areas: decode_areas(areas, config.max_collection_items())?,
            devices: decode_devices(devices, config.max_collection_items())?,
            entities: decode_entities(entities, config.max_collection_items())?,
            states,
        })
    }

    fn install_state_event_fence(&mut self, states: &[HomeAssistantState]) {
        let mut state_event_fence: BTreeMap<String, u64> = BTreeMap::new();
        for state in states {
            state_event_fence
                .entry(state.entity_id.clone())
                .and_modify(|observed_at| {
                    *observed_at = (*observed_at).max(state.observed_at_ms);
                })
                .or_insert(state.observed_at_ms);
        }
        self.state_event_fence = state_event_fence;
    }

    fn event_advances_state_fence(&mut self, changed: &HomeAssistantStateChanged) -> bool {
        let entity_id = changed.new_state.entity_id.as_str();
        let observed_at = changed.new_state.observed_at_ms;
        if self
            .state_event_fence
            .get(entity_id)
            .is_some_and(|fence| observed_at <= *fence)
        {
            return false;
        }
        self.state_event_fence
            .insert(entity_id.to_owned(), observed_at);
        true
    }

    async fn next_state_changed(
        &mut self,
        config: &HomeAssistantConnectionConfig,
    ) -> PortResult<HomeAssistantStateChanged> {
        loop {
            let value = match self.pending_events.pop_front() {
                Some(value) => value,
                None => self.next_value(config).await?,
            };
            if value.get("type").and_then(Value::as_str) != Some("event") {
                return Err(invalid_data(
                    "Home Assistant sent a non-event message while waiting for state",
                ));
            }
            self.validate_subscription_event(&value)?;
            let event = required_object(&value, "event")?;
            match required_string(event, "event_type", 128)? {
                "state_changed" => {
                    let changed = decode_state_changed(event)?;
                    if self.event_advances_state_fence(&changed) {
                        return Ok(changed);
                    }
                },
                "area_registry_updated"
                | "device_registry_updated"
                | "entity_registry_updated"
                | "floor_registry_updated"
                | "label_registry_updated"
                | "service_registered"
                | "service_removed" => {
                    return Err(PortError::new(
                        PortErrorKind::Conflict,
                        "Home Assistant registry changed and requires a complete resynchronization",
                    ));
                },
                _ => {},
            }
        }
    }

    #[cfg(feature = "integration-control")]
    async fn set_power(
        &mut self,
        config: &HomeAssistantConnectionConfig,
        request: &HomeAssistantPowerRequest,
    ) -> PortResult<ProviderAcceptance> {
        let result = self.command(config, request.command()).await?;
        decode_provider_acceptance(&result)
    }

    async fn command(
        &mut self,
        config: &HomeAssistantConnectionConfig,
        command: Value,
    ) -> PortResult<Value> {
        let id = self.send_command(config, command).await?;
        self.wait_for_result_before_deadline(config, id).await
    }

    async fn send_command(
        &mut self,
        config: &HomeAssistantConnectionConfig,
        mut command: Value,
    ) -> PortResult<u64> {
        let id = self.next_request_id;
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .ok_or_else(|| permanent("Home Assistant request identifier exhausted"))?;
        command
            .as_object_mut()
            .ok_or_else(|| permanent("Home Assistant command must be an object"))?
            .insert("id".into(), Value::from(id));
        self.send_value(config, &command).await?;
        Ok(id)
    }

    async fn wait_for_result_before_deadline(
        &mut self,
        config: &HomeAssistantConnectionConfig,
        expected_id: u64,
    ) -> PortResult<Value> {
        timeout(
            config.request_timeout(),
            self.wait_for_result(config, expected_id),
        )
        .await
        .map_err(|_| timed_out("Home Assistant WebSocket command timed out"))?
    }

    async fn wait_for_result(
        &mut self,
        config: &HomeAssistantConnectionConfig,
        expected_id: u64,
    ) -> PortResult<Value> {
        loop {
            let value = self.next_value(config).await?;
            match value.get("type").and_then(Value::as_str) {
                Some("event") => {
                    self.validate_subscription_event(&value)?;
                    if self.pending_events.len() >= config.max_collection_items() {
                        return Err(PortError::new(
                            PortErrorKind::Conflict,
                            "Home Assistant event buffer overflow requires resynchronization",
                        ));
                    }
                    self.pending_events.push_back(value);
                },
                Some("result") if value.get("id").and_then(Value::as_u64) == Some(expected_id) => {
                    if value.get("success").and_then(Value::as_bool) != Some(true) {
                        return Err(result_error(&value));
                    }
                    return Ok(value.get("result").cloned().unwrap_or(Value::Null));
                },
                Some("result") => {
                    return Err(invalid_data(
                        "Home Assistant returned a result for an unexpected request",
                    ));
                },
                _ => return Err(invalid_data("Home Assistant message type is invalid")),
            }
        }
    }

    fn validate_subscription_event(&self, value: &Value) -> PortResult<()> {
        if value.get("id").and_then(Value::as_u64) != Some(self.subscription_id) {
            return Err(invalid_data(
                "Home Assistant event used an unexpected subscription identifier",
            ));
        }
        Ok(())
    }

    async fn send_value(
        &mut self,
        config: &HomeAssistantConnectionConfig,
        value: &Value,
    ) -> PortResult<()> {
        let encoded = serde_json::to_string(value)
            .map_err(|_| permanent("Home Assistant request encoding failed"))?;
        if encoded.len() > config.max_message_bytes() {
            return Err(PortError::new(
                PortErrorKind::Rejected,
                "Home Assistant request exceeds the configured message bound",
            ));
        }
        timeout(
            config.request_timeout(),
            self.socket.send(Message::Text(encoded.into())),
        )
        .await
        .map_err(|_| timed_out("Home Assistant WebSocket send timed out"))?
        .map_err(send_error)
    }

    async fn next_value(&mut self, config: &HomeAssistantConnectionConfig) -> PortResult<Value> {
        loop {
            if let Some(value) = self.inbound.pop_front() {
                return Ok(value);
            }
            let message = timeout(config.request_timeout(), self.socket.next())
                .await
                .map_err(|_| timed_out("Home Assistant WebSocket response timed out"))?
                .ok_or_else(|| unavailable("Home Assistant WebSocket stream ended"))?
                .map_err(receive_error)?;
            match message {
                Message::Text(text) => {
                    let value: Value = serde_json::from_str(&text)
                        .map_err(|_| invalid_data("Home Assistant WebSocket JSON is invalid"))?;
                    match value {
                        Value::Array(values) => {
                            if values.len() > config.max_collection_items() {
                                return Err(invalid_data(
                                    "Home Assistant coalesced message exceeds the item bound",
                                ));
                            }
                            self.inbound.extend(values);
                        },
                        value @ Value::Object(_) => self.inbound.push_back(value),
                        _ => {
                            return Err(invalid_data(
                                "Home Assistant WebSocket payload must be an object or array",
                            ));
                        },
                    }
                },
                Message::Ping(payload) => {
                    timeout(
                        config.request_timeout(),
                        self.socket.send(Message::Pong(payload)),
                    )
                    .await
                    .map_err(|_| timed_out("Home Assistant WebSocket pong timed out"))?
                    .map_err(send_error)?;
                },
                Message::Pong(_) => {},
                Message::Close(_) => {
                    return Err(unavailable("Home Assistant WebSocket closed"));
                },
                Message::Binary(_) | Message::Frame(_) => {
                    return Err(invalid_data(
                        "Home Assistant WebSocket sent an unsupported frame type",
                    ));
                },
            }
        }
    }
}

fn decode_areas(value: Value, limit: usize) -> PortResult<Vec<HomeAssistantArea>> {
    bounded_array(value, limit, "area registry")?
        .into_iter()
        .map(|value| {
            let object = into_object(value, "area registry entry")?;
            let id = optional_string(&object, "area_id", 512)?
                .or(optional_string(&object, "id", 512)?)
                .ok_or_else(|| invalid_data("Home Assistant area has no stable identifier"))?;
            Ok(HomeAssistantArea {
                id,
                name: required_string_owned(&object, "name", 512)?,
            })
        })
        .collect()
}

fn decode_devices(value: Value, limit: usize) -> PortResult<Vec<HomeAssistantDevice>> {
    bounded_array(value, limit, "device registry")?
        .into_iter()
        .map(|value| {
            let object = into_object(value, "device registry entry")?;
            let id = required_string_owned(&object, "id", 512)?;
            let name = optional_string(&object, "name_by_user", 512)?
                .or(optional_string(&object, "name", 512)?)
                .unwrap_or_else(|| id.clone());
            Ok(HomeAssistantDevice {
                id,
                name,
                area_id: optional_string(&object, "area_id", 512)?,
            })
        })
        .collect()
}

fn decode_entities(value: Value, limit: usize) -> PortResult<Vec<HomeAssistantEntity>> {
    bounded_array(value, limit, "entity registry")?
        .into_iter()
        .map(|value| {
            let object = into_object(value, "entity registry entry")?;
            let id = required_string_owned(&object, "id", 512)?;
            let entity_id = required_string_owned(&object, "entity_id", 512)?;
            let domain = entity_id
                .split_once('.')
                .map(|(domain, _)| domain.to_owned())
                .ok_or_else(|| invalid_data("Home Assistant entity has no domain"))?;
            let name = optional_string(&object, "name", 512)?
                .or(optional_string(&object, "original_name", 512)?)
                .unwrap_or_else(|| entity_id.clone());
            Ok(HomeAssistantEntity {
                id,
                entity_id,
                name,
                domain,
                device_id: optional_string(&object, "device_id", 512)?,
                area_id: optional_string(&object, "area_id", 512)?,
            })
        })
        .collect()
}

fn decode_states(value: Value, limit: usize) -> PortResult<Vec<HomeAssistantState>> {
    bounded_array(value, limit, "state snapshot")?
        .into_iter()
        .map(decode_state)
        .collect()
}

fn decode_state(value: Value) -> PortResult<HomeAssistantState> {
    let object = into_object(value, "state entry")?;
    let attributes = object
        .get("attributes")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_data("Home Assistant state attributes are invalid"))?;
    let observed_at = required_string(&object, "last_updated", 128)?;
    let context_id = object
        .get("context")
        .and_then(Value::as_object)
        .map(|context| optional_string(context, "id", 512))
        .transpose()?
        .flatten();
    Ok(HomeAssistantState {
        entity_id: required_string_owned(&object, "entity_id", 512)?,
        state: required_string_owned(&object, "state", 8_192)?,
        attributes: filter_attributes(attributes)?,
        observed_at_ms: parse_timestamp_ms(observed_at)?,
        context_id,
    })
}

fn decode_state_changed(event: &Map<String, Value>) -> PortResult<HomeAssistantStateChanged> {
    let data = required_object_map(event, "data")?;
    let new_state = data
        .get("new_state")
        .cloned()
        .ok_or_else(|| invalid_data("Home Assistant state event omitted new_state"))?;
    if new_state.is_null() {
        return Err(PortError::new(
            PortErrorKind::Conflict,
            "Home Assistant entity removal requires a complete resynchronization",
        ));
    }
    Ok(HomeAssistantStateChanged {
        new_state: decode_state(new_state)?,
    })
}

const ALLOWED_ATTRIBUTES: &[&str] = &[
    "battery_level",
    "brightness",
    "color_temp_kelvin",
    "current_humidity",
    "current_position",
    "current_temperature",
    "current_tilt_position",
    "device_class",
    "event_type",
    "hvac_action",
    "is_volume_muted",
    "percentage",
    "preset_mode",
    "state_class",
    "temperature",
    "unit_of_measurement",
    "volume_level",
];

fn filter_attributes(attributes: &Map<String, Value>) -> PortResult<BTreeMap<String, Value>> {
    let mut filtered = BTreeMap::new();
    for key in ALLOWED_ATTRIBUTES {
        let Some(value) = attributes.get(*key) else {
            continue;
        };
        if value.to_string().len() > 8_192 {
            return Err(invalid_data(
                "Home Assistant mapped attribute exceeds the value bound",
            ));
        }
        filtered.insert((*key).to_owned(), value.clone());
    }
    Ok(filtered)
}

fn bounded_array(value: Value, limit: usize, name: &str) -> PortResult<Vec<Value>> {
    let values = match value {
        Value::Array(values) => values,
        _ => {
            return Err(invalid_data(&format!(
                "Home Assistant {name} must be an array"
            )));
        },
    };
    if values.len() > limit {
        return Err(PortError::new(
            PortErrorKind::Rejected,
            format!("Home Assistant {name} exceeds the item bound"),
        ));
    }
    Ok(values)
}

fn into_object(value: Value, name: &str) -> PortResult<Map<String, Value>> {
    value
        .as_object()
        .cloned()
        .ok_or_else(|| invalid_data(&format!("Home Assistant {name} must be an object")))
}

fn required_object<'a>(value: &'a Value, field: &str) -> PortResult<&'a Map<String, Value>> {
    value
        .get(field)
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_data("Home Assistant message object field is invalid"))
}

fn required_object_map<'a>(
    value: &'a Map<String, Value>,
    field: &str,
) -> PortResult<&'a Map<String, Value>> {
    value
        .get(field)
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_data("Home Assistant event object field is invalid"))
}

fn required_string<'a>(
    object: &'a Map<String, Value>,
    field: &str,
    max_bytes: usize,
) -> PortResult<&'a str> {
    let value = object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_data("Home Assistant required string field is invalid"))?;
    validate_string(value, max_bytes)?;
    Ok(value)
}

fn required_string_owned(
    object: &Map<String, Value>,
    field: &str,
    max_bytes: usize,
) -> PortResult<String> {
    required_string(object, field, max_bytes).map(str::to_owned)
}

fn optional_string(
    object: &Map<String, Value>,
    field: &str,
    max_bytes: usize,
) -> PortResult<Option<String>> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => {
            validate_string(value, max_bytes)?;
            Ok(Some(value.clone()))
        },
        Some(_) => Err(invalid_data(
            "Home Assistant optional string field is invalid",
        )),
    }
}

fn validate_string(value: &str, max_bytes: usize) -> PortResult<()> {
    if value.is_empty() || value.len() > max_bytes || value.chars().any(char::is_control) {
        return Err(invalid_data(
            "Home Assistant string field is empty, unbounded, or contains controls",
        ));
    }
    Ok(())
}

fn parse_timestamp_ms(value: &str) -> PortResult<u64> {
    let timestamp = DateTime::parse_from_rfc3339(value)
        .map_err(|_| invalid_data("Home Assistant timestamp is invalid"))?
        .timestamp_millis();
    u64::try_from(timestamp)
        .map_err(|_| invalid_data("Home Assistant timestamp predates the Unix epoch"))
}

fn result_error(value: &Value) -> PortError {
    let code = value
        .get("error")
        .and_then(Value::as_object)
        .and_then(|error| error.get("code"))
        .and_then(Value::as_str);
    match code {
        Some("unauthorized") => PortError::new(
            PortErrorKind::Rejected,
            "Home Assistant rejected the operation as unauthorized",
        ),
        Some("not_found") => PortError::new(
            PortErrorKind::NotFound,
            "Home Assistant operation target was not found",
        ),
        Some("invalid_format" | "service_validation_error") => PortError::new(
            PortErrorKind::InvalidData,
            "Home Assistant rejected invalid request data",
        ),
        Some("home_assistant_error" | "unknown_error") | None | Some(_) => PortError::new(
            PortErrorKind::Rejected,
            "Home Assistant rejected the operation",
        ),
    }
}

fn drop_secret(secret: SecretMaterial) {
    drop(secret);
}

fn invalid_data(message: &str) -> PortError {
    PortError::new(PortErrorKind::InvalidData, message)
}

fn unavailable(message: &str) -> PortError {
    PortError::new(PortErrorKind::Unavailable, message)
}

fn transport_shutdown() -> PortError {
    unavailable(TRANSPORT_SHUTDOWN_MESSAGE)
}

fn timed_out(message: &str) -> PortError {
    PortError::new(PortErrorKind::Timeout, message)
}

fn permanent(message: &str) -> PortError {
    PortError::new(PortErrorKind::Permanent, message)
}

fn connect_error(error: WebSocketError) -> PortError {
    match error {
        WebSocketError::Url(_) => permanent("Home Assistant WebSocket endpoint is invalid"),
        WebSocketError::Capacity(_)
        | WebSocketError::Protocol(_)
        | WebSocketError::Utf8(_)
        | WebSocketError::AttackAttempt
        | WebSocketError::Http(_)
        | WebSocketError::HttpFormat(_) => {
            invalid_data("Home Assistant WebSocket handshake violated the transport contract")
        },
        WebSocketError::ConnectionClosed
        | WebSocketError::AlreadyClosed
        | WebSocketError::Io(_)
        | WebSocketError::Tls(_)
        | WebSocketError::WriteBufferFull(_) => {
            unavailable("Home Assistant WebSocket connect failed")
        },
    }
}

fn send_error(error: WebSocketError) -> PortError {
    match error {
        WebSocketError::Capacity(_)
        | WebSocketError::Protocol(_)
        | WebSocketError::Utf8(_)
        | WebSocketError::AttackAttempt => {
            invalid_data("Home Assistant WebSocket send violated the transport contract")
        },
        WebSocketError::ConnectionClosed
        | WebSocketError::AlreadyClosed
        | WebSocketError::Io(_)
        | WebSocketError::Tls(_)
        | WebSocketError::WriteBufferFull(_)
        | WebSocketError::Url(_)
        | WebSocketError::Http(_)
        | WebSocketError::HttpFormat(_) => unavailable("Home Assistant WebSocket send failed"),
    }
}

fn receive_error(error: WebSocketError) -> PortError {
    match error {
        WebSocketError::Capacity(_)
        | WebSocketError::Protocol(_)
        | WebSocketError::Utf8(_)
        | WebSocketError::AttackAttempt => {
            invalid_data("Home Assistant WebSocket frame violated the transport contract")
        },
        WebSocketError::ConnectionClosed
        | WebSocketError::AlreadyClosed
        | WebSocketError::Io(_)
        | WebSocketError::Tls(_)
        | WebSocketError::WriteBufferFull(_)
        | WebSocketError::Url(_)
        | WebSocketError::Http(_)
        | WebSocketError::HttpFormat(_) => unavailable("Home Assistant WebSocket receive failed"),
    }
}

fn resynchronization_required() -> PortError {
    PortError::new(
        PortErrorKind::Conflict,
        "Home Assistant state stream gap requires a complete resynchronization",
    )
}
