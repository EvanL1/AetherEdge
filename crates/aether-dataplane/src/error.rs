//! Typed failures exposed by the physical data plane.

use std::io;
use std::path::PathBuf;

/// Failure returned by shared-memory layout and persistence operations.
#[derive(Debug, thiserror::Error)]
pub enum DataplaneError {
    /// Declared capacity, live count, or serialized layout is inconsistent.
    #[error("invalid shared-memory layout: {0}")]
    InvalidLayout(String),
    /// A filesystem path cannot identify the required SHM or snapshot object.
    #[error("invalid shared-memory path: {0:?}")]
    InvalidPath(PathBuf),
    /// The operating system rejected an mmap or filesystem operation.
    #[error("{context}: {source}")]
    Io {
        /// Operation and resource context suitable for logs.
        context: String,
        /// Underlying operating-system error.
        #[source]
        source: io::Error,
    },
}

impl DataplaneError {
    pub(crate) fn io(context: impl Into<String>, source: io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }
}

/// Result returned by the physical data-plane API.
pub type DataplaneResult<T> = Result<T, DataplaneError>;
