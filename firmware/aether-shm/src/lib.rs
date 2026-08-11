//! Firmware-owned raw shared-memory layout and accessors.
//!
//! `aether-shm` is a `no_std` crate for an embedded HAL-provided memory region.
//! It does not define or open a filesystem path, and its ABI is independent
//! from the AetherEdge Linux live-state data plane.
//!
//! ```rust,ignore
//! use aether_shm::{RawPtrShm, ShmOps};
//!
//! let base = 0x2000_0000 as *mut u8;
//! let Some(mut state) = (unsafe { RawPtrShm::from_raw(base, 256) }) else {
//!     return;
//! };
//! if !state.init() {
//!     return;
//! }
//! let _written = state.write_slot(0, 42.5, timestamp_ms, 0);
//! ```

#![no_std]

#[cfg(test)]
extern crate std;

mod layout;
mod raw_ptr;
mod traits;

pub use layout::{
    FIRMWARE_HEADER_SIZE, FIRMWARE_SHM_MAGIC, FIRMWARE_SHM_VERSION, FIRMWARE_SLOT_SIZE,
    MAX_FIRMWARE_SLOT_COUNT, firmware_shm_size,
};
pub use raw_ptr::RawPtrShm;
pub use traits::{ShmOps, ShmOpsExt};
