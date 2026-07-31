//! CAN Protocol Client Implementation
//!
//! Implements the protocol layer's Protocol traits for LYNK CAN communication.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

use aether_core::PointType;
use arc_swap::ArcSwapOption;
use socketcan::{CanSocket, EmbeddedFrame, Frame, Socket};
use tokio::sync::RwLock;
use tokio::task::JoinHandle;

use crate::protocols::core::data::DataBatch;
use crate::protocols::core::error::{GatewayError, Result};

use async_trait::async_trait;

use crate::protocols::core::traits::{
    AdjustmentCommand, CommunicationMode, ConnectionState, ControlCommand, DataEvent,
    DataEventReceiver, DataEventSender, Diagnostics, PollResult, WriteResult, data_event_channel,
};
use crate::protocols::runtime::ChannelRuntime;

use super::config::{CanConfig, CanFrameCache, LynkCanId};
use super::decoder::PointManager;

// ============================================================================
// CanClient
// ============================================================================

/// CAN protocol client.
///
/// Implements event-driven communication over CAN bus using the LYNK protocol.
/// Uses CSV configuration for flexible point mapping.
pub struct CanClient {
    config: CanConfig,

    // Connection state (lock-free)
    connection_state: AtomicU8,
    is_connected: Arc<AtomicBool>,

    // Statistics (lock-free)
    read_count: Arc<AtomicU64>,
    error_count: Arc<AtomicU64>,
    last_error: Arc<ArcSwapOption<String>>,

    // Tasks
    receive_handle: Option<JoinHandle<()>>,
    read_handle: Option<JoinHandle<()>>,

    // Event queue for the unified channel task.
    event_tx: DataEventSender,
    event_rx: Option<DataEventReceiver>,

    // CAN frame cache
    frame_cache: Arc<RwLock<CanFrameCache>>,

    // Point manager
    point_manager: Arc<PointManager>,
}

impl CanClient {
    /// Create a new CAN client with the given configuration.
    pub fn new(config: CanConfig) -> Self {
        let point_manager = PointManager::new();
        let (event_tx, event_rx) = data_event_channel();

        Self {
            config,
            connection_state: AtomicU8::new(ConnectionState::Disconnected.into()),
            is_connected: Arc::new(AtomicBool::new(false)),
            read_count: Arc::new(AtomicU64::new(0)),
            error_count: Arc::new(AtomicU64::new(0)),
            last_error: Arc::new(ArcSwapOption::empty()),
            receive_handle: None,
            read_handle: None,
            event_tx,
            event_rx: Some(event_rx),
            frame_cache: Arc::new(RwLock::new(CanFrameCache::new())),
            point_manager: Arc::new(point_manager),
        }
    }

    /// Add CAN points to the client.
    /// This should be called after `new()` and before `connect()`.
    pub fn add_points(&mut self, points: Vec<super::config::CanPoint>) -> Result<()> {
        #[cfg(feature = "tracing-support")]
        tracing::info!("Adding {} CAN points to client", points.len());

        let point_manager = Arc::get_mut(&mut self.point_manager)
            .ok_or_else(|| GatewayError::Config("PointManager has multiple owners".into()))?;

        point_manager.add_points(points)?;

        #[cfg(feature = "tracing-support")]
        tracing::info!("CAN points added successfully");

        Ok(())
    }

    /// Start the CAN frame receive task.
    fn start_receive_task(&mut self) -> Result<()> {
        let can_interface = self.config.can_interface.clone();
        let is_connected = Arc::clone(&self.is_connected);
        let frame_cache = Arc::clone(&self.frame_cache);
        let error_count = Arc::clone(&self.error_count);
        let last_error = Arc::clone(&self.last_error);
        let rx_poll_interval = self.config.rx_poll_interval_ms;

        let handle = tokio::spawn(async move {
            #[cfg(feature = "tracing-support")]
            tracing::info!("Starting CAN socket open on interface: {}", can_interface);

            let socket = match CanSocket::open(&can_interface) {
                Ok(s) => {
                    #[cfg(feature = "tracing-support")]
                    tracing::info!("CAN socket opened successfully on {}", can_interface);
                    s
                },
                Err(e) => {
                    #[cfg(feature = "tracing-support")]
                    tracing::error!("Failed to open CAN socket on {}: {}", can_interface, e);

                    last_error.store(Some(Arc::new(format!("Failed to open CAN socket: {}", e))));
                    error_count.fetch_add(1, Ordering::Relaxed);
                    return;
                },
            };

            // Set non-blocking mode
            if let Err(e) = socket.set_nonblocking(true) {
                #[cfg(feature = "tracing-support")]
                tracing::error!("Failed to set non-blocking mode: {}", e);

                last_error.store(Some(Arc::new(format!(
                    "Failed to set non-blocking mode: {}",
                    e
                ))));
                error_count.fetch_add(1, Ordering::Relaxed);
                return;
            }

            #[cfg(feature = "tracing-support")]
            tracing::info!("CAN socket configured successfully, starting receive loop");

            #[cfg(feature = "tracing-support")]
            tracing::info!(
                "CAN receive task started on {} (rx_poll_interval={}ms)",
                can_interface,
                rx_poll_interval
            );

            #[cfg(feature = "tracing-support")]
            tracing::info!("Creating interval with {}ms period...", rx_poll_interval);

            let mut interval =
                tokio::time::interval(tokio::time::Duration::from_millis(rx_poll_interval));

            #[cfg(feature = "tracing-support")]
            tracing::info!("Interval created, starting receive loop");

            #[cfg(feature = "tracing-support")]
            let mut poll_count = 0u64;
            loop {
                #[cfg(feature = "tracing-support")]
                {
                    poll_count += 1;
                    if poll_count == 1 {
                        tracing::info!("First tick - waiting for interval...");
                    }
                }

                interval.tick().await;

                #[cfg(feature = "tracing-support")]
                {
                    if poll_count == 1 {
                        tracing::info!("First tick completed! Loop is working.");
                    }
                    if poll_count.is_multiple_of(20) {
                        tracing::debug!(
                            "CAN receive loop: {} polls, checking for frames...",
                            poll_count
                        );
                    }
                }

                if !is_connected.load(Ordering::SeqCst) {
                    #[cfg(feature = "tracing-support")]
                    tracing::info!("CAN receive loop stopping (disconnected)");
                    break;
                }

                // Try to read a CAN frame
                match socket.read_frame() {
                    Ok(frame) => {
                        // Use socketcan Frame trait's raw_id() method
                        let can_id = frame.raw_id();

                        #[cfg(feature = "tracing-support")]
                        tracing::info!(
                            "Raw CAN frame received: ID=0x{:03X} ({}), checking if LYNK...",
                            can_id,
                            can_id
                        );

                        // Check if this is a LYNK protocol frame
                        if LynkCanId::is_lynk_id(can_id) {
                            let data = frame.data();

                            #[cfg(feature = "tracing-support")]
                            tracing::info!(
                                "Received LYNK CAN frame: ID=0x{:03X}, Data={:02X?}",
                                can_id,
                                data
                            );

                            // Update cache with slice reference (no heap allocation)
                            frame_cache.write().await.update(can_id, data);
                        } else {
                            #[cfg(feature = "tracing-support")]
                            tracing::warn!("Ignoring non-LYNK CAN frame: ID=0x{:03X}", can_id);
                        }
                    },
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        // No data available, continue polling
                        // This is normal in non-blocking mode
                        continue;
                    },
                    Err(e) => {
                        #[cfg(feature = "tracing-support")]
                        tracing::error!("CAN read error: {:?}", e);

                        last_error.store(Some(Arc::new(format!("CAN read error: {}", e))));
                        error_count.fetch_add(1, Ordering::Relaxed);
                        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                    },
                }
            }

            #[cfg(feature = "tracing-support")]
            tracing::info!("CAN receive task stopped");
        });

        self.receive_handle = Some(handle);
        Ok(())
    }

    /// Start the data reading task.
    fn start_read_task(&mut self) -> Result<()> {
        let is_connected = Arc::clone(&self.is_connected);
        let frame_cache = Arc::clone(&self.frame_cache);
        let point_manager = Arc::clone(&self.point_manager);
        let read_count = Arc::clone(&self.read_count);
        let error_count = Arc::clone(&self.error_count);
        let last_error = Arc::clone(&self.last_error);
        let event_tx = self.event_tx.clone();
        let read_interval = self.config.data_read_interval_ms;

        let handle = tokio::spawn(async move {
            #[cfg(feature = "tracing-support")]
            tracing::info!("CAN data reading task started");

            let mut interval =
                tokio::time::interval(tokio::time::Duration::from_millis(read_interval));

            loop {
                interval.tick().await;

                if !is_connected.load(Ordering::SeqCst) {
                    break;
                }

                // Apply mappings to decode cached frames.
                // Scope the read lock to keep event dispatch independent of the frame cache.
                let mapping_result = {
                    let cache = frame_cache.read().await;

                    #[cfg(feature = "tracing-support")]
                    {
                        tracing::info!("Frame cache has {} CAN IDs", cache.iter().count());
                        for (can_id, frame_data) in cache.iter() {
                            tracing::debug!(
                                "  CAN ID 0x{:03X}: {} bytes",
                                can_id,
                                frame_data.as_slice().len()
                            );
                        }
                    }

                    point_manager.apply_mappings(&cache)
                    // cache (RwLockReadGuard) is dropped here
                };

                match mapping_result {
                    Ok(decoded_points) => {
                        #[cfg(feature = "tracing-support")]
                        tracing::info!("Decoded {} points from frame cache", decoded_points.len());

                        if decoded_points.is_empty() {
                            #[cfg(feature = "tracing-support")]
                            tracing::warn!("No points decoded from frame cache");
                            continue;
                        }

                        // Keep only validated acquisition-plane events. The
                        // channel task writes this batch directly to SHM, so
                        // the adapter must not maintain a second live-state cache.
                        let mut batch = DataBatch::with_capacity(decoded_points.len());

                        for point in decoded_points {
                            #[cfg(feature = "tracing-support")]
                            tracing::debug!(
                                "  {:?} point {}: {:?}",
                                point.point_type,
                                point.id,
                                point.value
                            );

                            match point.point_type {
                                PointType::Telemetry | PointType::Signal => {},
                                PointType::Control | PointType::Adjustment => {
                                    let message = format!(
                                        "CAN decoder produced unsupported {:?} point {}",
                                        point.point_type, point.id
                                    );
                                    last_error.store(Some(Arc::new(message)));
                                    error_count.fetch_add(1, Ordering::Relaxed);
                                    continue;
                                },
                            }
                            batch.add(point);
                        }

                        if !batch.is_empty() {
                            read_count.fetch_add(1, Ordering::Relaxed);

                            #[cfg(feature = "tracing-support")]
                            tracing::info!(
                                "Sending batch with {} data points to event system",
                                batch.len()
                            );

                            // Keep device I/O independent from consumer backpressure.
                            #[cfg(feature = "tracing-support")]
                            tracing::debug!("Sending DataUpdate event via event_tx");
                            let _ = event_tx.try_send(DataEvent::DataUpdate(batch));
                        }
                    },
                    Err(e) => {
                        #[cfg(feature = "tracing-support")]
                        tracing::error!("Failed to apply mappings: {}", e);
                        last_error
                            .store(Some(Arc::new(format!("Failed to apply mappings: {}", e))));
                        error_count.fetch_add(1, Ordering::Relaxed);
                    },
                }
            }

            #[cfg(feature = "tracing-support")]
            tracing::info!("CAN data reading task stopped");
        });

        self.read_handle = Some(handle);
        Ok(())
    }
}

// ============================================================================
// Trait Implementations
// ============================================================================

impl CanClient {
    fn name(&self) -> &'static str {
        "CAN"
    }

    fn supported_modes(&self) -> &[CommunicationMode] {
        &[CommunicationMode::EventDriven]
    }

    fn supports_client(&self) -> bool {
        true
    }

    fn supports_server(&self) -> bool {
        false
    }

    fn version(&self) -> &'static str {
        "LYNK Protocol"
    }
}

// ============================================================================
// HasMetadata Implementation
// ============================================================================

use crate::protocols::core::metadata::{
    DriverMetadata, HasMetadata, ParameterMetadata, ParameterType,
};

impl HasMetadata for CanClient {
    fn metadata() -> DriverMetadata {
        DriverMetadata {
            name: "can",
            display_name: "CAN Bus",
            description: "Controller Area Network (CAN) bus protocol for industrial and automotive applications.",
            is_recommended: true,
            example_config: serde_json::json!({
                "device": "can0",
                "bitrate": 250000,
                "connect_timeout_ms": 3000,
                "read_timeout_ms": 3000,
                "retry_interval_ms": 2000,
                "rx_poll_interval_ms": 50
            }),
            parameters: vec![
                ParameterMetadata::optional(
                    "device",
                    "CAN Device",
                    "SocketCAN device name (e.g., can0, vcan0). Legacy key 'interface' is also accepted.",
                    ParameterType::String,
                    serde_json::json!("can0"),
                ),
                ParameterMetadata::optional(
                    "bitrate",
                    "Bitrate",
                    "CAN bus bitrate in bits per second",
                    ParameterType::Integer,
                    serde_json::json!(250000),
                ),
                ParameterMetadata::optional(
                    "connect_timeout_ms",
                    "Connect Timeout (ms)",
                    "Timeout for opening the CAN socket",
                    ParameterType::Integer,
                    serde_json::json!(3000),
                ),
                ParameterMetadata::optional(
                    "read_timeout_ms",
                    "Read Timeout (ms)",
                    "Timeout for receiving a CAN frame",
                    ParameterType::Integer,
                    serde_json::json!(3000),
                ),
                ParameterMetadata::optional(
                    "retry_interval_ms",
                    "Retry Interval (ms)",
                    "Reconnect interval after a connection failure",
                    ParameterType::Integer,
                    serde_json::json!(2000),
                ),
                ParameterMetadata::optional(
                    "rx_poll_interval_ms",
                    "RX Poll Interval (ms)",
                    "Interval for polling received CAN frames",
                    ParameterType::Integer,
                    serde_json::json!(50),
                ),
            ],
        }
    }
}

// ============================================================================
// ChannelRuntime implementation (direct, no wrapper needed)
// ============================================================================

#[async_trait]
impl ChannelRuntime for CanClient {
    fn is_event_driven(&self) -> bool {
        true
    }

    async fn connect(&mut self) -> Result<()> {
        self.connection_state
            .store(ConnectionState::Connecting.into(), Ordering::Release);

        // Verify CAN interface exists
        let _socket = CanSocket::open(&self.config.can_interface).map_err(|e| {
            GatewayError::Connection(format!(
                "Failed to open CAN interface {}: {}",
                self.config.can_interface, e
            ))
        })?;

        #[cfg(feature = "tracing-support")]
        tracing::info!(
            "CAN interface {} opened successfully",
            self.config.can_interface
        );

        self.is_connected.store(true, Ordering::SeqCst);
        self.connection_state
            .store(ConnectionState::Connected.into(), Ordering::Release);

        // Start receive and read tasks
        self.start_receive_task()?;
        self.start_read_task()?;

        Ok(())
    }

    async fn disconnect(&mut self) -> Result<()> {
        self.is_connected.store(false, Ordering::SeqCst);

        // Stop receive task
        if let Some(handle) = self.receive_handle.take() {
            handle.abort();
        }

        // Stop read task
        if let Some(handle) = self.read_handle.take() {
            handle.abort();
        }

        self.connection_state
            .store(ConnectionState::Disconnected.into(), Ordering::Release);

        #[cfg(feature = "tracing-support")]
        tracing::info!("CAN client disconnected");

        Ok(())
    }

    async fn poll_once(&mut self) -> PollResult {
        PollResult::success(DataBatch::new())
    }

    fn take_event_receiver(&mut self) -> Option<DataEventReceiver> {
        self.event_rx.take()
    }

    async fn start_events(&mut self) -> Result<()> {
        // CAN client starts automatically on connect
        Ok(())
    }

    async fn stop_events(&mut self) -> Result<()> {
        // Stop receive and read tasks
        if let Some(handle) = self.receive_handle.take() {
            handle.abort();
        }
        if let Some(handle) = self.read_handle.take() {
            handle.abort();
        }
        Ok(())
    }

    async fn diagnostics(&self) -> Result<Diagnostics> {
        Ok(Diagnostics {
            protocol: "CAN".to_string(),
            connection_state: ConnectionState::from(self.connection_state.load(Ordering::Acquire)),
            read_count: self.read_count.load(Ordering::Relaxed),
            write_count: 0,
            error_count: self.error_count.load(Ordering::Relaxed),
            last_error: self.last_error.load().as_ref().map(|s| (**s).clone()),
            extra: serde_json::json!({
                "device": self.config.can_interface,
                "bitrate": self.config.bitrate,
                "connect_timeout_ms": self.config.connect_timeout_ms,
                "retry_interval_ms": self.config.retry_interval_ms,
            }),
        })
    }

    fn connection_state(&self) -> ConnectionState {
        ConnectionState::from(self.connection_state.load(Ordering::Acquire))
    }
}
