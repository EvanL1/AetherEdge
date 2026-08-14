//! Core abstractions for the Industrial Gateway.
//!
//! This module provides the foundational types and traits that all protocols implement.

pub mod data;
pub mod data_event_ingress;
pub mod diagnostics;
pub mod error;
pub mod file_logging;
pub mod log_handlers;
pub mod logging;
pub mod metadata;
pub mod point;
pub mod traits;

pub use data::*;
pub use data_event_ingress::{
    DataEventAdmission, DataEventIngressObserver, DataEventIngressStats, DataEventReceiver,
    DataEventSink,
};
pub use diagnostics::{AtomicDiagnostics, DiagnosticsSnapshot};
pub use error::{GatewayError, Result};
pub use file_logging::{ChannelFileLogHandler, FileLogLevel};
pub use metadata::{
    DriverMetadata, HasMetadata, ParameterMetadata, ParameterType, ProtocolMetadata,
};
pub use point::*;
pub use traits::*;
