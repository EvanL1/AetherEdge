//! CAN protocol types (cross-platform)
//!
//! This module contains CAN-related types that don't depend on hardware.
//! Separated from the main CAN module to allow testing on non-Linux platforms.

use aether_model::PointType;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// CAN data type enumeration.
///
/// Using an enum instead of String eliminates heap allocation per point
/// and enables fast integer-based matching in the decoder hot path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CanDataType {
    /// Unsigned 8-bit integer
    UInt8,
    /// Unsigned 16-bit integer (default)
    #[default]
    UInt16,
    /// Signed 16-bit integer
    Int16,
    /// Unsigned 32-bit integer
    UInt32,
    /// Signed 32-bit integer
    Int32,
    /// 32-bit floating point
    Float32,
    /// ASCII string
    Ascii,
}

/// CAN point mapping structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanPoint {
    /// Unique point identifier (numeric)
    pub point_id: u32,
    /// Point type (T/S/C/A)
    pub point_type: PointType,
    /// CAN-ID (e.g., 0x351)
    pub can_id: u32,
    /// Byte offset in CAN data field (0-7)
    pub byte_offset: u8,
    /// Bit starting position within byte (0-7, LSB=0)
    pub bit_position: u8,
    /// Bit length (2/8/16/32/64)
    pub bit_length: u8,
    /// Data type for interpretation
    pub data_type: CanDataType,
    /// Scale factor for linear transformation (value = raw * scale + offset)
    #[serde(default = "default_scale")]
    pub scale: f64,
    /// Offset for linear transformation
    #[serde(default)]
    pub offset: f64,
}

fn default_scale() -> f64 {
    1.0
}

/// CAN frame data - fixed-size stack-allocated buffer
///
/// CAN frames have a maximum of 8 bytes of data, so we use a fixed
/// [u8; 8] buffer to avoid heap allocations.
#[derive(Debug, Clone)]
pub struct CanFrameData {
    data: [u8; 8],
    len: u8,
}

impl CanFrameData {
    /// Create from a byte slice (copies up to 8 bytes)
    pub fn from_slice(bytes: &[u8]) -> Self {
        let mut data = [0u8; 8];
        let len = bytes.len().min(8) as u8;
        data[..len as usize].copy_from_slice(&bytes[..len as usize]);
        Self { data, len }
    }

    /// Get the data as a slice
    pub fn as_slice(&self) -> &[u8] {
        &self.data[..self.len as usize]
    }

    /// Get the length
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// Check if empty
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// CAN frame cache - stores the latest received frame for each CAN-ID
///
/// No heap allocation for individual frame data - uses fixed-size CanFrameData.
#[derive(Debug, Default)]
pub struct CanFrameCache {
    /// Map from CAN-ID to frame data (fixed 8-byte buffer + length)
    frames: HashMap<u32, CanFrameData>,
}

impl CanFrameCache {
    /// Create a new empty frame cache
    pub fn new() -> Self {
        Self {
            frames: HashMap::new(),
        }
    }

    /// Update cache with a new frame (no heap allocation for the data)
    pub fn update(&mut self, can_id: u32, data: &[u8]) {
        self.frames.insert(can_id, CanFrameData::from_slice(data));
    }

    /// Get the latest frame data for a CAN-ID
    pub fn get(&self, can_id: u32) -> Option<&[u8]> {
        self.frames.get(&can_id).map(|f| f.as_slice())
    }

    /// Get number of cached CAN-IDs
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Check if empty
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Get all frames (for debugging)
    #[allow(dead_code)]
    pub fn iter(&self) -> impl Iterator<Item = (&u32, &CanFrameData)> {
        self.frames.iter()
    }
}
