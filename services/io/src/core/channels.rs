//! Channel lifecycle, command dispatch, and protocol composition.

mod channel_creation;
pub mod channel_entry;
pub mod channel_manager;
mod channel_task;
mod command_guard;
mod protocol_registry;
pub mod shm_listener;
pub mod types;

mod converters;
mod factory;

pub use channel_manager::ChannelManager;
pub use protocol_registry::{BuiltProtocolRuntime, ProtocolAdapterFactory, ProtocolRegistry};
pub use shm_listener::ShmCommandListener;

/// Return the protocol factories statically linked by this IO build.
pub fn compiled_protocol_registry() -> crate::error::Result<std::sync::Arc<ProtocolRegistry>> {
    factory::compiled_protocol_registry()
}
pub use types::ChannelStatus;
