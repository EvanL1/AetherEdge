//! Legacy SQLite configuration helpers used by the six service compositions.
//!
//! Live point state remains in SHM. This crate contains no external-service
//! client and is not a public integration boundary.

#[cfg(feature = "sqlite")]
pub mod sqlite;
