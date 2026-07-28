//! Local adapters that require no external service.

mod audit;
mod clock;
mod cloudlink_challenge_ledger;
mod cloudlink_spool;
mod data_processing;
mod file_cloudlink_spool;
mod file_outbox;
mod history;
mod live_state;
mod outbox;
#[cfg(feature = "sqlite-routing")]
mod routing_sqlite;
mod snapshot_covariates;

use aether_ports::{PortError, PortErrorKind};

pub use audit::MemoryAuditSink;
#[cfg(feature = "sqlite-audit")]
pub use audit::SqliteAuditSink;
pub use clock::{ManualClock, SystemClock};
pub use cloudlink_challenge_ledger::{
    CloudLinkChallengeLedgerError, CloudLinkChallengeReservation, CloudLinkPendingChallengeRequest,
    FileCloudLinkChallengeLedger,
};
pub use cloudlink_spool::MemoryCloudLinkSpool;
pub use data_processing::{MemoryCovariateSource, MemoryHistoryQuery};
pub use file_cloudlink_spool::FileCloudLinkSpool;
pub use file_outbox::FileOutbox;
pub use history::MemoryHistorySink;
pub use live_state::MemoryLiveState;
pub use outbox::MemoryOutbox;
#[cfg(feature = "sqlite-routing")]
pub use routing_sqlite::{load_channel_routes, load_physical_topology, load_routing_snapshot};
pub use snapshot_covariates::{SnapshotCovariateLimits, SnapshotCovariateSource};

fn lock_error(resource: &str) -> PortError {
    PortError::new(
        PortErrorKind::Permanent,
        format!("{resource} lock was poisoned"),
    )
}
