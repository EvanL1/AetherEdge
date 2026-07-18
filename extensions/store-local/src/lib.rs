//! Local adapters that require no external service.

mod audit;
mod clock;
mod cloudlink_challenge_ledger;
mod cloudlink_spool;
mod data_processing;
mod file_cloudlink_spool;
mod file_outbox;
mod history;
#[cfg(feature = "integration-control")]
mod integration_control_audit;
#[cfg(feature = "integration-control")]
mod integration_control_ledger;
mod integration_generation;
mod live_state;
mod outbox;
mod snapshot_covariates;
#[cfg(feature = "sqlite-topology")]
mod sqlite_topology;

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
#[cfg(feature = "integration-control")]
pub use integration_control_audit::FileIntegrationControlAudit;
#[cfg(feature = "integration-control")]
pub use integration_control_ledger::FileIntegrationControlLedger;
pub use integration_generation::FileIntegrationTopologyGenerationStore;
pub use live_state::MemoryLiveState;
pub use outbox::MemoryOutbox;
pub use snapshot_covariates::{SnapshotCovariateLimits, SnapshotCovariateSource};
#[cfg(feature = "sqlite-topology")]
pub use sqlite_topology::{
    LogicalPointRoutes, SqliteLiveTopologySnapshot, SqliteShmTopologySnapshot,
    load_sqlite_live_topology, load_sqlite_shm_topology,
};

fn lock_error(resource: &str) -> PortError {
    PortError::new(
        PortErrorKind::Permanent,
        format!("{resource} lock was poisoned"),
    )
}
