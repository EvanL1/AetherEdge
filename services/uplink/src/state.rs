use std::sync::Arc;

use aether_store_local::FileCloudLinkSpool;
use tokio::sync::Notify;

use crate::cloudlink_runtime::CloudLinkRuntimeStatus;

/// Shared HTTP application state. Cloud delivery state is owned by CloudLink.
pub struct AppState {
    pub spool: Arc<FileCloudLinkSpool>,
    pub alarm_broadcast_token: Arc<str>,
    pub cloudlink: Arc<CloudLinkRuntimeStatus>,
    pub delivery_wake: Arc<Notify>,
}
