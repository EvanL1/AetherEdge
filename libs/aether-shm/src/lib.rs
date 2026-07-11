//! # aether-shm
//!
//! Platform-agnostic shared memory abstraction for AetherEMS.
//!
//! This crate provides reader and writer interfaces for shared memory
//! that work on both Linux (via mmap) and embedded platforms (via raw pointers).
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                    aether-shm                              │
//! ├─────────────────────────────────────────────────────────────┤
//! │                       ShmOps trait                          │
//! │  - read_slot(index) -> Option<PointSlot>                    │
//! │  - write_slot(index, value, ts, quality)                    │
//! │  - slot_count() -> u32                                      │
//! ├─────────────────────────────────────────────────────────────┤
//! │           ┌─────────────────┐  ┌─────────────────┐          │
//! │           │   std impl      │  │  no_std impl    │          │
//! │           │  (memmap2)      │  │ (raw pointer)   │          │
//! │           └─────────────────┘  └─────────────────┘          │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Usage
//!
//! ### Linux (std)
//!
//! ```rust,ignore
//! use aether_shm::{ShmConfig, MmapReader, MmapWriter};
//!
//! // Create a writer (typically in io)
//! let config = ShmConfig::default();
//! let mut writer = MmapWriter::create(&config)?;
//! writer.write_slot(0, 42.5, timestamp_ms, Quality::Good as u8);
//!
//! // Create a reader (typically in automation)
//! let reader = MmapReader::open(&config)?;
//! if let Some((value, ts, quality)) = reader.read_slot(0) {
//!     println!("Value: {}", value);
//! }
//! ```
//!
//! ### Embedded (no_std)
//!
//! ```rust,ignore
//! use aether_shm::RawPtrShm;
//!
//! // Get shared memory base address from HAL
//! let base_addr: *mut u8 = 0x2000_0000 as *mut u8;
//! let mut shm = unsafe { RawPtrShm::from_raw(base_addr, 1024) };
//!
//! shm.init();
//! shm.write_slot(0, 42.5, timestamp_ms, 0);
//! ```

#![cfg_attr(not(feature = "std"), no_std)]

// Re-export core types
pub use aether_core::shm::{
    DEFAULT_MAX_SLOTS, HEADER_SIZE, PointSlot, SHM_MAGIC, SHM_VERSION, SLOT_SIZE, ShmHeader,
    shm_size, slot_offset,
};
pub use aether_core::{PointType, Quality};

mod traits;
pub use traits::{ShmOps, ShmOpsExt};

#[cfg(feature = "std")]
mod mmap;

#[cfg(feature = "std")]
pub use mmap::{MmapReader, MmapWriter, ShmConfig, ShmError};

mod raw_ptr;
pub use raw_ptr::RawPtrShm;

/// Default shared memory path on Linux.
#[cfg(feature = "std")]
pub const DEFAULT_SHM_PATH: &str = "/shm/rtdb/aether.shm";
