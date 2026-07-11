//! Local adapters that require no external service.

mod audit;
mod file_outbox;
mod history;
mod live_state;
mod outbox;

use aether_ports::{PortError, PortErrorKind};

pub use audit::MemoryAuditSink;
#[cfg(feature = "sqlite-audit")]
pub use audit::SqliteAuditSink;
pub use file_outbox::FileOutbox;
pub use history::MemoryHistorySink;
pub use live_state::MemoryLiveState;
pub use outbox::MemoryOutbox;

fn lock_error(resource: &str) -> PortError {
    PortError::new(
        PortErrorKind::Permanent,
        format!("{resource} lock was poisoned"),
    )
}
