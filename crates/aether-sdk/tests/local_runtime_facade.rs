#![cfg(feature = "local-runtime")]

use aether_sdk::local::{MemoryAuditSink, MemoryLiveState};

#[test]
fn local_runtime_adapters_are_available_through_the_sdk_facade() {
    let _live_state = MemoryLiveState::new();
    let _audit = MemoryAuditSink::new();
}
