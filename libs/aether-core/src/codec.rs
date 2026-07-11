//! Protocol codecs for AetherEMS.
//!
//! This module contains no_std compatible encoders and decoders for:
//! - DL/T 645-2007 protocol (Chinese smart meter standard)
//! - CAN bus frame decoding
//!
//! All functions are pure (no I/O), use fixed-size buffers, and avoid heap allocation.

pub mod can;
pub mod dl645;

// Re-exports
pub use dl645::{
    DataIdentifier, Dl645Error, Dl645Frame, FRAME_END, FRAME_START, MeterAddress,
    calculate_checksum, decode_data, decode_response, encode_data, encode_read_request,
};

pub use can::{CanDataType, CanDecodeError, ExtractedField, decode_value, extract_field};
