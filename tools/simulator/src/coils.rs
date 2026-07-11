//! Coil and discrete input storage for Modbus operations.
//!
//! Coils (FC=0x01 read, FC=0x05/0x0F write) are single-bit outputs.
//! Discrete inputs (FC=0x02 read) are single-bit inputs (read-only from master).
//!
//! Unlike registers (16-bit), coils are boolean values. When read in bulk,
//! they are packed into bytes with LSB-first ordering within each byte.

use std::collections::HashMap;
use std::sync::RwLock;

/// Storage for coil and discrete input values.
///
/// Provides separate address spaces for:
/// - Coils: Read/write via FC01/FC05/FC0F
/// - Discrete inputs: Read-only via FC02
///
/// Thread-safe using RwLock for concurrent read/write access.
pub struct CoilStore {
    /// Coils storage: (unit_id, address) -> bool
    coils: RwLock<HashMap<(u8, u16), bool>>,
    /// Discrete inputs storage: (unit_id, address) -> bool
    discrete_inputs: RwLock<HashMap<(u8, u16), bool>>,
}

impl CoilStore {
    /// Create a new empty coil store.
    pub fn new() -> Self {
        Self {
            coils: RwLock::new(HashMap::new()),
            discrete_inputs: RwLock::new(HashMap::new()),
        }
    }

    // ========================================================================
    // Coils (FC01 read, FC05/FC0F write)
    // ========================================================================

    /// Write a single coil (FC05).
    pub fn write_coil(&self, unit_id: u8, address: u16, value: bool) {
        let mut coils = self.coils.write().unwrap_or_else(|e| e.into_inner());
        coils.insert((unit_id, address), value);
    }

    /// Write multiple coils (FC0F).
    pub fn write_coils(&self, unit_id: u8, start_address: u16, values: &[bool]) {
        let mut coils = self.coils.write().unwrap_or_else(|e| e.into_inner());
        for (offset, &value) in values.iter().enumerate() {
            let addr = start_address.wrapping_add(offset as u16);
            coils.insert((unit_id, addr), value);
        }
    }

    /// Read multiple coils (FC01).
    ///
    /// Returns a Vec of boolean values for the requested range.
    /// Unset coils default to `false`.
    pub fn read_coils(&self, unit_id: u8, start_address: u16, count: u16) -> Vec<bool> {
        let coils = self.coils.read().unwrap_or_else(|e| e.into_inner());
        (0..count)
            .map(|offset| {
                let addr = start_address.wrapping_add(offset);
                coils.get(&(unit_id, addr)).copied().unwrap_or(false)
            })
            .collect()
    }

    // ========================================================================
    // Discrete Inputs (FC02 read-only)
    // ========================================================================

    /// Set a discrete input value (for simulation setup).
    pub fn set_discrete_input(&self, unit_id: u8, address: u16, value: bool) {
        let mut inputs = self
            .discrete_inputs
            .write()
            .unwrap_or_else(|e| e.into_inner());
        inputs.insert((unit_id, address), value);
    }

    /// Read multiple discrete inputs (FC02).
    ///
    /// Returns a Vec of boolean values for the requested range.
    /// Unset inputs default to `false`.
    pub fn read_discrete_inputs(&self, unit_id: u8, start_address: u16, count: u16) -> Vec<bool> {
        let inputs = self
            .discrete_inputs
            .read()
            .unwrap_or_else(|e| e.into_inner());
        (0..count)
            .map(|offset| {
                let addr = start_address.wrapping_add(offset);
                inputs.get(&(unit_id, addr)).copied().unwrap_or(false)
            })
            .collect()
    }

    // ========================================================================
    // Bit Packing Utilities
    // ========================================================================

    /// Pack boolean values into bytes (LSB-first within each byte).
    ///
    /// This is the format required for Modbus FC01/FC02 responses.
    ///
    /// Example: [true, false, true, false, true, false, true, false]
    ///          becomes \[0x55\] (binary: 0101_0101, LSB=bit0=true)
    pub fn pack_coils_to_bytes(values: &[bool]) -> Vec<u8> {
        let byte_count = values.len().div_ceil(8);
        let mut bytes = vec![0u8; byte_count];

        for (i, &value) in values.iter().enumerate() {
            if value {
                let byte_index = i / 8;
                let bit_index = i % 8;
                bytes[byte_index] |= 1 << bit_index;
            }
        }

        bytes
    }

    /// Unpack bytes to boolean values (LSB-first within each byte).
    ///
    /// This is the format used in Modbus FC0F requests.
    pub fn unpack_bytes_to_coils(bytes: &[u8], count: u16) -> Vec<bool> {
        let mut values = Vec::with_capacity(count as usize);

        for i in 0..count as usize {
            let byte_index = i / 8;
            let bit_index = i % 8;

            if byte_index < bytes.len() {
                let value = (bytes[byte_index] >> bit_index) & 1 == 1;
                values.push(value);
            } else {
                values.push(false);
            }
        }

        values
    }
}

impl Default for CoilStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_write_read_coil() {
        let store = CoilStore::new();

        // Initially false
        assert_eq!(store.read_coils(1, 100, 1), vec![false]);

        // Write and read back
        store.write_coil(1, 100, true);
        assert_eq!(store.read_coils(1, 100, 1), vec![true]);

        // Different unit_id should still be false
        assert_eq!(store.read_coils(2, 100, 1), vec![false]);
    }

    #[test]
    fn test_write_multiple_coils() {
        let store = CoilStore::new();
        store.write_coils(1, 0, &[true, false, true, false, true]);

        assert_eq!(
            store.read_coils(1, 0, 6),
            vec![true, false, true, false, true, false]
        );
    }

    #[test]
    fn test_read_coils_bulk() {
        let store = CoilStore::new();
        store.write_coil(1, 0, true);
        store.write_coil(1, 2, true);
        store.write_coil(1, 4, true);

        let values = store.read_coils(1, 0, 6);
        assert_eq!(values, vec![true, false, true, false, true, false]);
    }

    #[test]
    fn test_discrete_inputs() {
        let store = CoilStore::new();
        store.set_discrete_input(1, 0, true);
        store.set_discrete_input(1, 1, true);
        store.set_discrete_input(1, 2, false);
        store.set_discrete_input(1, 3, false);

        let values = store.read_discrete_inputs(1, 0, 4);
        assert_eq!(values, vec![true, true, false, false]);
    }

    #[test]
    fn test_pack_coils_to_bytes() {
        // 8 coils: alternating true/false starting with true
        let values = vec![true, false, true, false, true, false, true, false];
        let bytes = CoilStore::pack_coils_to_bytes(&values);
        assert_eq!(bytes, vec![0x55]); // 0101_0101 = 0x55

        // 16 coils: first 8 = 0x55, second 8 = 0xAA
        let values = vec![
            true, false, true, false, true, false, true, false, // 0x55
            false, true, false, true, false, true, false, true, // 0xAA
        ];
        let bytes = CoilStore::pack_coils_to_bytes(&values);
        assert_eq!(bytes, vec![0x55, 0xAA]);

        // 10 coils: needs 2 bytes
        let values = vec![
            true, true, true, true, true, true, true, true, // 0xFF
            true, true, // 0x03
        ];
        let bytes = CoilStore::pack_coils_to_bytes(&values);
        assert_eq!(bytes, vec![0xFF, 0x03]);
    }

    #[test]
    fn test_unpack_bytes_to_coils() {
        // Unpack 0x55 to 8 coils
        let coils = CoilStore::unpack_bytes_to_coils(&[0x55], 8);
        assert_eq!(
            coils,
            vec![true, false, true, false, true, false, true, false]
        );

        // Unpack 2 bytes to 10 coils
        let coils = CoilStore::unpack_bytes_to_coils(&[0xFF, 0x03], 10);
        assert_eq!(
            coils,
            vec![true, true, true, true, true, true, true, true, true, true]
        );
    }

    #[test]
    fn test_pack_unpack_roundtrip() {
        let original = vec![
            true, false, true, true, false, false, true, false, // 8 bits
            true, false, true, // 3 more bits
        ];

        let bytes = CoilStore::pack_coils_to_bytes(&original);
        let unpacked = CoilStore::unpack_bytes_to_coils(&bytes, original.len() as u16);

        assert_eq!(original, unpacked);
    }
}
