//! J1939 Protocol Implementation
//!
//! SAE J1939 is a CAN-based protocol used in heavy-duty vehicles and industrial equipment.

mod client;

// Re-export client
pub use client::{J1939Client, J1939Config};

// Re-export voltage_j1939 types for convenience
pub use voltage_j1939::{
    DecodedSpn, J1939Id, SpnDataType, SpnDef, database_stats, decode_frame, decode_spn,
    get_spn_def, get_spns_for_pgn, list_supported_pgns, parse_can_id,
};
