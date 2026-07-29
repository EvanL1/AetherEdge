//! Channel lifecycle, command dispatch, and protocol composition.

mod channel_creation;
pub mod channel_entry;
pub mod channel_manager;
mod channel_task;
mod command_guard;
pub mod shm_listener;
pub mod types;

pub(crate) mod converters;
pub(crate) mod factory;

pub use channel_manager::ChannelManager;
pub use shm_listener::ShmCommandListener;
pub use types::ChannelStatus;
