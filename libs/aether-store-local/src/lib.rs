//! Local adapters that require no external service.

mod audit;
mod clock;
mod cloudlink_challenge_ledger;
mod cloudlink_spool;
mod data_processing;
mod file_cloudlink_spool;
mod file_outbox;
mod gateway_identity;
mod gateway_identity_fs;
mod history;
mod integration_generation;
mod live_state;
mod outbox;
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
pub use gateway_identity::{
    FileClaimedGatewayIdentitySource, FileGatewayIdentityStore,
    OsEd25519GatewayIdentityKeyGenerator,
};
pub use history::MemoryHistorySink;
pub use integration_generation::FileIntegrationTopologyGenerationStore;
pub use live_state::MemoryLiveState;
pub use outbox::MemoryOutbox;
pub use snapshot_covariates::{SnapshotCovariateLimits, SnapshotCovariateSource};

fn lock_error(resource: &str) -> PortError {
    PortError::new(
        PortErrorKind::Permanent,
        format!("{resource} lock was poisoned"),
    )
}
