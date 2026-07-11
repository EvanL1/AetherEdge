//! Modbus RTU slave server implementation.
//!
//! Implements a Modbus RTU server over serial port (or virtual serial port like pty).
//! Uses the same device map and writable storage as the TCP server.
//!
//! # Frame Format
//!
//! RTU frames: `[slave_id(1)][function_code(1)][data(N)][crc16(2)]`
//!
//! Unlike Modbus TCP, RTU frames have no MBAP header and use CRC16 for error checking.

use crate::coils::CoilStore;
use crate::devices::{DeviceMap, generate_registers};
use crate::scenarios::DeviceConfig;
use crate::writable::WritableRegisters;
use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_serial::{SerialPortBuilderExt, SerialStream};
use tracing::{debug, info, warn};

// Modbus function codes (same as TCP)
const FC_READ_COILS: u8 = 0x01;
const FC_READ_DISCRETE_INPUTS: u8 = 0x02;
const FC_READ_HOLDING_REGISTERS: u8 = 0x03;
const FC_READ_INPUT_REGISTERS: u8 = 0x04;
const FC_WRITE_SINGLE_COIL: u8 = 0x05;
const FC_WRITE_SINGLE_REGISTER: u8 = 0x06;
const FC_WRITE_MULTIPLE_COILS: u8 = 0x0F;
const FC_WRITE_MULTIPLE_REGISTERS: u8 = 0x10;

// Modbus exception codes
const EX_ILLEGAL_FUNCTION: u8 = 0x01;
const EX_ILLEGAL_DATA_ADDRESS: u8 = 0x02;
const EX_ILLEGAL_DATA_VALUE: u8 = 0x03;

/// Run the Modbus RTU server on a serial port.
///
/// # Arguments
///
/// * `port` - Serial port path (e.g., "/dev/ttyUSB0" or "/dev/pts/3")
/// * `baud_rate` - Baud rate (e.g., 9600, 19200, 38400, 115200)
/// * `device_map` - Device register configuration
/// * `devices` - Device configurations for initializing coils
pub async fn run_rtu_server(
    port: &str,
    baud_rate: u32,
    device_map: DeviceMap,
    devices: &[DeviceConfig],
) -> Result<()> {
    let device_map = Arc::new(device_map);
    let writable = Arc::new(WritableRegisters::new());
    let coil_store = Arc::new(CoilStore::new());

    // Initialize coils and discrete inputs from device configuration
    for device in devices {
        for coil in &device.coils {
            coil_store.write_coil(device.unit_id, coil.address, coil.value);
        }
        for input in &device.discrete_inputs {
            coil_store.set_discrete_input(device.unit_id, input.address, input.value);
        }
        if !device.coils.is_empty() || !device.discrete_inputs.is_empty() {
            info!(
                "Loaded {} coils and {} discrete inputs for unit {}",
                device.coils.len(),
                device.discrete_inputs.len(),
                device.unit_id
            );
        }
    }

    info!(
        "Starting Modbus RTU server on {} @ {} baud",
        port, baud_rate
    );

    // Open serial port
    let mut serial = tokio_serial::new(port, baud_rate)
        .timeout(Duration::from_millis(100))
        .open_native_async()?;

    info!("Modbus RTU server listening on {}", port);

    let mut buf = [0u8; 256];

    loop {
        // RTU frame detection: read with inter-character timeout
        // A frame ends when there's a gap of 3.5 character times
        match read_rtu_frame(&mut serial, &mut buf).await {
            Ok(frame_len) if frame_len >= 4 => {
                let frame = &buf[..frame_len];

                // Verify CRC
                let received_crc = u16::from_le_bytes([frame[frame_len - 2], frame[frame_len - 1]]);
                let calculated_crc = calculate_crc16(&frame[..frame_len - 2]);

                if received_crc != calculated_crc {
                    warn!(
                        "CRC error: received=0x{:04X}, calculated=0x{:04X}",
                        received_crc, calculated_crc
                    );
                    continue;
                }

                let slave_id = frame[0];
                let function_code = frame[1];
                let data = &frame[2..frame_len - 2];

                debug!(
                    "RTU request: slave={}, fc=0x{:02X}, data_len={}",
                    slave_id,
                    function_code,
                    data.len()
                );

                // Process request
                let response = process_rtu_request(
                    slave_id,
                    function_code,
                    data,
                    &device_map,
                    &writable,
                    &coil_store,
                );

                // Send response with CRC
                if let Some(resp_data) = response {
                    let mut resp_frame = resp_data;
                    let crc = calculate_crc16(&resp_frame);
                    resp_frame.extend_from_slice(&crc.to_le_bytes());

                    serial.write_all(&resp_frame).await?;
                    serial.flush().await?;

                    debug!("RTU response sent: {} bytes", resp_frame.len());
                }
            },
            Ok(_) => {
                // Frame too short, ignore
            },
            Err(e) => {
                if !e.to_string().contains("timed out") {
                    debug!("Read error (may be normal): {}", e);
                }
            },
        }
    }
}

/// Read a complete RTU frame from serial port.
///
/// RTU framing relies on timing: a frame ends after 3.5 character times of silence.
/// At 9600 baud, this is approximately 4ms.
#[allow(clippy::while_let_loop)] // Complex loop with conditional break, while_let doesn't fit
async fn read_rtu_frame(serial: &mut SerialStream, buf: &mut [u8]) -> Result<usize> {
    // Read first byte with longer timeout
    let mut total_read = match tokio::time::timeout(
        Duration::from_millis(1000),
        serial.read(&mut buf[0..1]),
    )
    .await
    {
        Ok(Ok(1)) => 1,
        Ok(Ok(_)) | Ok(Err(_)) | Err(_) => return Ok(0),
    };

    // Read remaining bytes with inter-character timeout
    loop {
        match tokio::time::timeout(
            Duration::from_millis(5), // ~3.5 char times at 9600
            serial.read(&mut buf[total_read..total_read + 1]),
        )
        .await
        {
            Ok(Ok(1)) => {
                total_read += 1;
                if total_read >= buf.len() - 1 {
                    break;
                }
            },
            _ => break, // Timeout means end of frame
        }
    }

    Ok(total_read)
}

/// Process a Modbus RTU request and return response data (without CRC).
fn process_rtu_request(
    slave_id: u8,
    function_code: u8,
    data: &[u8],
    device_map: &DeviceMap,
    writable: &WritableRegisters,
    coil_store: &CoilStore,
) -> Option<Vec<u8>> {
    let timestamp_ms = chrono::Utc::now().timestamp_millis();

    match function_code {
        // ====================================================================
        // FC01: Read Coils / FC02: Read Discrete Inputs
        // ====================================================================
        FC_READ_COILS | FC_READ_DISCRETE_INPUTS => {
            if data.len() < 4 {
                return Some(build_rtu_exception(
                    slave_id,
                    function_code,
                    EX_ILLEGAL_DATA_VALUE,
                ));
            }

            let start_addr = u16::from_be_bytes([data[0], data[1]]);
            let quantity = u16::from_be_bytes([data[2], data[3]]);

            if quantity == 0 || quantity > 2000 {
                return Some(build_rtu_exception(
                    slave_id,
                    function_code,
                    EX_ILLEGAL_DATA_VALUE,
                ));
            }

            let bits = if function_code == FC_READ_COILS {
                debug!(
                    "RTU Read coils: slave={}, addr={}, count={}",
                    slave_id, start_addr, quantity
                );
                coil_store.read_coils(slave_id, start_addr, quantity)
            } else {
                debug!(
                    "RTU Read discrete inputs: slave={}, addr={}, count={}",
                    slave_id, start_addr, quantity
                );
                coil_store.read_discrete_inputs(slave_id, start_addr, quantity)
            };

            Some(build_rtu_read_coils_response(
                slave_id,
                function_code,
                &bits,
            ))
        },

        // ====================================================================
        // FC03/FC04: Read Holding/Input Registers
        // ====================================================================
        FC_READ_HOLDING_REGISTERS | FC_READ_INPUT_REGISTERS => {
            if data.len() < 4 {
                return Some(build_rtu_exception(
                    slave_id,
                    function_code,
                    EX_ILLEGAL_DATA_VALUE,
                ));
            }

            let start_addr = u16::from_be_bytes([data[0], data[1]]);
            let quantity = u16::from_be_bytes([data[2], data[3]]);

            debug!(
                "RTU Read: slave={}, addr={}, count={}",
                slave_id, start_addr, quantity
            );

            if quantity == 0 || quantity > 125 {
                return Some(build_rtu_exception(
                    slave_id,
                    function_code,
                    EX_ILLEGAL_DATA_VALUE,
                ));
            }

            let register_map = device_map.get(&slave_id).or_else(|| device_map.get(&1));

            if let Some(register_map) = register_map {
                let generated =
                    generate_registers(register_map, start_addr, quantity, timestamp_ms);

                // Override with written values
                let values: Vec<u16> = (0..quantity)
                    .map(|offset| {
                        let addr = start_addr.wrapping_add(offset);
                        writable
                            .read(slave_id, addr)
                            .unwrap_or(generated[offset as usize])
                    })
                    .collect();

                Some(build_rtu_read_response(slave_id, function_code, &values))
            } else {
                Some(build_rtu_exception(
                    slave_id,
                    function_code,
                    EX_ILLEGAL_DATA_ADDRESS,
                ))
            }
        },

        // ====================================================================
        // FC05: Write Single Coil
        // ====================================================================
        FC_WRITE_SINGLE_COIL => {
            if data.len() < 4 {
                return Some(build_rtu_exception(
                    slave_id,
                    function_code,
                    EX_ILLEGAL_DATA_VALUE,
                ));
            }

            let addr = u16::from_be_bytes([data[0], data[1]]);
            let value_raw = u16::from_be_bytes([data[2], data[3]]);

            // Modbus coil values: 0xFF00 = ON, 0x0000 = OFF
            let value = match value_raw {
                0xFF00 => true,
                0x0000 => false,
                _ => {
                    return Some(build_rtu_exception(
                        slave_id,
                        function_code,
                        EX_ILLEGAL_DATA_VALUE,
                    ));
                },
            };

            debug!("RTU Write single coil: addr={}, value={}", addr, value);

            coil_store.write_coil(slave_id, addr, value);

            // Echo back
            let mut resp = vec![slave_id, function_code];
            resp.extend_from_slice(&addr.to_be_bytes());
            resp.extend_from_slice(&value_raw.to_be_bytes());
            Some(resp)
        },

        // ====================================================================
        // FC06: Write Single Register
        // ====================================================================
        FC_WRITE_SINGLE_REGISTER => {
            if data.len() < 4 {
                return Some(build_rtu_exception(
                    slave_id,
                    function_code,
                    EX_ILLEGAL_DATA_VALUE,
                ));
            }

            let addr = u16::from_be_bytes([data[0], data[1]]);
            let value = u16::from_be_bytes([data[2], data[3]]);

            debug!("RTU Write single: addr={}, value={}", addr, value);

            writable.write_single(slave_id, addr, value);

            // Echo back
            let mut resp = vec![slave_id, function_code];
            resp.extend_from_slice(&addr.to_be_bytes());
            resp.extend_from_slice(&value.to_be_bytes());
            Some(resp)
        },

        // ====================================================================
        // FC0F: Write Multiple Coils
        // ====================================================================
        FC_WRITE_MULTIPLE_COILS => {
            if data.len() < 5 {
                return Some(build_rtu_exception(
                    slave_id,
                    function_code,
                    EX_ILLEGAL_DATA_VALUE,
                ));
            }

            let addr = u16::from_be_bytes([data[0], data[1]]);
            let quantity = u16::from_be_bytes([data[2], data[3]]);
            let byte_count = data[4] as usize;

            // Validate byte count matches quantity
            let expected_bytes = (quantity as usize).div_ceil(8);
            if byte_count != expected_bytes || data.len() < 5 + byte_count {
                return Some(build_rtu_exception(
                    slave_id,
                    function_code,
                    EX_ILLEGAL_DATA_VALUE,
                ));
            }

            // Unpack coil values from bytes
            let coil_bytes = &data[5..5 + byte_count];
            let values = CoilStore::unpack_bytes_to_coils(coil_bytes, quantity);

            debug!(
                "RTU Write multiple coils: addr={}, count={}, values={:?}",
                addr, quantity, values
            );

            coil_store.write_coils(slave_id, addr, &values);

            // Response: slave_id + fc + addr + quantity
            let mut resp = vec![slave_id, function_code];
            resp.extend_from_slice(&addr.to_be_bytes());
            resp.extend_from_slice(&quantity.to_be_bytes());
            Some(resp)
        },

        // ====================================================================
        // FC10: Write Multiple Registers
        // ====================================================================
        FC_WRITE_MULTIPLE_REGISTERS => {
            if data.len() < 5 {
                return Some(build_rtu_exception(
                    slave_id,
                    function_code,
                    EX_ILLEGAL_DATA_VALUE,
                ));
            }

            let addr = u16::from_be_bytes([data[0], data[1]]);
            let quantity = u16::from_be_bytes([data[2], data[3]]);
            let byte_count = data[4] as usize;

            let expected_bytes = quantity as usize * 2;
            if byte_count != expected_bytes || data.len() < 5 + byte_count {
                return Some(build_rtu_exception(
                    slave_id,
                    function_code,
                    EX_ILLEGAL_DATA_VALUE,
                ));
            }

            let values: Vec<u16> = (0..quantity as usize)
                .map(|i| {
                    let offset = 5 + i * 2;
                    u16::from_be_bytes([data[offset], data[offset + 1]])
                })
                .collect();

            debug!(
                "RTU Write multiple: addr={}, count={}, values={:?}",
                addr, quantity, values
            );

            writable.write_multiple(slave_id, addr, &values);

            // Response: slave_id + fc + addr + quantity
            let mut resp = vec![slave_id, function_code];
            resp.extend_from_slice(&addr.to_be_bytes());
            resp.extend_from_slice(&quantity.to_be_bytes());
            Some(resp)
        },

        _ => {
            warn!("RTU: Unsupported function code: 0x{:02X}", function_code);
            Some(build_rtu_exception(
                slave_id,
                function_code,
                EX_ILLEGAL_FUNCTION,
            ))
        },
    }
}

/// Build RTU read coils/discrete inputs response (without CRC).
fn build_rtu_read_coils_response(slave_id: u8, function_code: u8, coils: &[bool]) -> Vec<u8> {
    // Pack coils into bytes (LSB-first)
    let packed_bytes = CoilStore::pack_coils_to_bytes(coils);
    let byte_count = packed_bytes.len() as u8;

    let mut resp = Vec::with_capacity(3 + byte_count as usize);
    resp.push(slave_id);
    resp.push(function_code);
    resp.push(byte_count);
    resp.extend_from_slice(&packed_bytes);

    resp
}

/// Build RTU read response (without CRC).
fn build_rtu_read_response(slave_id: u8, function_code: u8, values: &[u16]) -> Vec<u8> {
    let byte_count = (values.len() * 2) as u8;
    let mut resp = Vec::with_capacity(3 + byte_count as usize);

    resp.push(slave_id);
    resp.push(function_code);
    resp.push(byte_count);

    for value in values {
        resp.extend_from_slice(&value.to_be_bytes());
    }

    resp
}

/// Build RTU exception response (without CRC).
fn build_rtu_exception(slave_id: u8, function_code: u8, exception_code: u8) -> Vec<u8> {
    vec![slave_id, function_code | 0x80, exception_code]
}

/// Calculate Modbus CRC16.
///
/// Uses the standard Modbus CRC16 polynomial (0xA001, reflected).
fn calculate_crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;

    for byte in data {
        crc ^= *byte as u16;
        for _ in 0..8 {
            if crc & 0x0001 != 0 {
                crc = (crc >> 1) ^ 0xA001;
            } else {
                crc >>= 1;
            }
        }
    }

    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // CRC16 calculation tests
    // ========================================================================

    #[test]
    fn test_crc16() {
        // Test vector: standard Modbus read holding registers request
        // Request: [0x01, 0x03, 0x00, 0x00, 0x00, 0x0A]
        // CRC = 0xCDC5 (stored in frame as little-endian: 0xC5, 0xCD)
        let data = [0x01u8, 0x03, 0x00, 0x00, 0x00, 0x0A];
        let crc = calculate_crc16(&data);
        assert_eq!(crc, 0xCDC5);
    }

    #[test]
    fn test_crc16_response() {
        // Response example: [0x01, 0x03, 0x04, 0x00, 0x64, 0x00, 0xC8]
        // CRC = 0x7ABA (stored in frame as little-endian: 0xBA, 0x7A)
        let data = [0x01u8, 0x03, 0x04, 0x00, 0x64, 0x00, 0xC8];
        let crc = calculate_crc16(&data);
        assert_eq!(crc, 0x7ABA);
    }

    #[test]
    fn test_crc16_empty_data() {
        // Empty data should return initial CRC value 0xFFFF
        let crc = calculate_crc16(&[]);
        assert_eq!(crc, 0xFFFF);
    }

    #[test]
    fn test_crc16_single_byte() {
        let crc = calculate_crc16(&[0x00]);
        // Single byte 0x00: CRC calculation result
        assert_ne!(crc, 0xFFFF); // Should be different from initial
    }

    #[test]
    fn test_crc16_all_zeros() {
        let data = [0x00u8; 8];
        let crc = calculate_crc16(&data);
        // CRC of all zeros should be consistent
        assert_ne!(crc, 0x0000);
    }

    #[test]
    fn test_crc16_all_ones() {
        let data = [0xFFu8; 4];
        let crc = calculate_crc16(&data);
        // CRC of all 0xFF bytes
        assert_ne!(crc, 0xFFFF);
    }

    #[test]
    fn test_crc16_write_single_register() {
        // FC06 Write Single Register: slave=1, addr=0x0010, value=0x0003
        let data = [0x01u8, 0x06, 0x00, 0x10, 0x00, 0x03];
        let crc = calculate_crc16(&data);
        // This is a known Modbus request - verify CRC is computed
        assert_ne!(crc, 0x0000);
    }

    // ========================================================================
    // Build exception response tests
    // ========================================================================

    #[test]
    fn test_build_exception() {
        let resp = build_rtu_exception(1, 0x03, EX_ILLEGAL_FUNCTION);
        assert_eq!(resp, vec![1, 0x83, 0x01]);
    }

    #[test]
    fn test_build_exception_illegal_data_address() {
        let resp = build_rtu_exception(1, 0x03, EX_ILLEGAL_DATA_ADDRESS);
        assert_eq!(resp, vec![1, 0x83, 0x02]);
    }

    #[test]
    fn test_build_exception_illegal_data_value() {
        let resp = build_rtu_exception(1, 0x03, EX_ILLEGAL_DATA_VALUE);
        assert_eq!(resp, vec![1, 0x83, 0x03]);
    }

    #[test]
    fn test_build_exception_different_function_codes() {
        // FC01 Read Coils
        let resp = build_rtu_exception(1, FC_READ_COILS, EX_ILLEGAL_FUNCTION);
        assert_eq!(resp[1], 0x81); // 0x01 | 0x80

        // FC02 Read Discrete Inputs
        let resp = build_rtu_exception(1, FC_READ_DISCRETE_INPUTS, EX_ILLEGAL_FUNCTION);
        assert_eq!(resp[1], 0x82); // 0x02 | 0x80

        // FC04 Read Input Registers
        let resp = build_rtu_exception(1, FC_READ_INPUT_REGISTERS, EX_ILLEGAL_FUNCTION);
        assert_eq!(resp[1], 0x84); // 0x04 | 0x80

        // FC05 Write Single Coil
        let resp = build_rtu_exception(1, FC_WRITE_SINGLE_COIL, EX_ILLEGAL_FUNCTION);
        assert_eq!(resp[1], 0x85); // 0x05 | 0x80

        // FC06 Write Single Register
        let resp = build_rtu_exception(1, FC_WRITE_SINGLE_REGISTER, EX_ILLEGAL_FUNCTION);
        assert_eq!(resp[1], 0x86); // 0x06 | 0x80

        // FC0F Write Multiple Coils
        let resp = build_rtu_exception(1, FC_WRITE_MULTIPLE_COILS, EX_ILLEGAL_FUNCTION);
        assert_eq!(resp[1], 0x8F); // 0x0F | 0x80

        // FC10 Write Multiple Registers
        let resp = build_rtu_exception(1, FC_WRITE_MULTIPLE_REGISTERS, EX_ILLEGAL_FUNCTION);
        assert_eq!(resp[1], 0x90); // 0x10 | 0x80
    }

    #[test]
    fn test_build_exception_different_slave_ids() {
        let resp1 = build_rtu_exception(1, 0x03, EX_ILLEGAL_FUNCTION);
        let resp2 = build_rtu_exception(247, 0x03, EX_ILLEGAL_FUNCTION);

        assert_eq!(resp1[0], 1);
        assert_eq!(resp2[0], 247);
    }

    // ========================================================================
    // Build read response tests
    // ========================================================================

    #[test]
    fn test_build_read_response() {
        let values = vec![100, 200];
        let resp = build_rtu_read_response(1, 0x03, &values);
        assert_eq!(resp, vec![1, 0x03, 4, 0, 100, 0, 200]);
    }

    #[test]
    fn test_build_read_response_empty() {
        let values: Vec<u16> = vec![];
        let resp = build_rtu_read_response(1, 0x03, &values);
        assert_eq!(resp, vec![1, 0x03, 0]); // byte_count = 0
    }

    #[test]
    fn test_build_read_response_single_value() {
        let values = vec![0x1234];
        let resp = build_rtu_read_response(1, 0x03, &values);
        assert_eq!(resp, vec![1, 0x03, 2, 0x12, 0x34]);
    }

    #[test]
    fn test_build_read_response_max_value() {
        let values = vec![0xFFFF];
        let resp = build_rtu_read_response(1, 0x03, &values);
        assert_eq!(resp, vec![1, 0x03, 2, 0xFF, 0xFF]);
    }

    #[test]
    fn test_build_read_response_zero_value() {
        let values = vec![0x0000];
        let resp = build_rtu_read_response(1, 0x04, &values);
        assert_eq!(resp, vec![1, 0x04, 2, 0x00, 0x00]);
    }

    #[test]
    fn test_build_read_response_multiple_values() {
        let values = vec![0x0001, 0x0002, 0x0003, 0x0004, 0x0005];
        let resp = build_rtu_read_response(1, 0x03, &values);

        assert_eq!(resp[0], 1); // slave_id
        assert_eq!(resp[1], 0x03); // function_code
        assert_eq!(resp[2], 10); // byte_count = 5 * 2
        assert_eq!(resp.len(), 13); // 3 header + 10 data bytes
    }

    // ========================================================================
    // Build read coils response tests
    // ========================================================================

    #[test]
    fn test_build_read_coils_response_single() {
        let coils = vec![true];
        let resp = build_rtu_read_coils_response(1, FC_READ_COILS, &coils);
        // 1 coil = 1 byte, value = 0x01 (bit 0 set)
        assert_eq!(resp[0], 1); // slave_id
        assert_eq!(resp[1], FC_READ_COILS); // function_code
        assert_eq!(resp[2], 1); // byte_count
        assert_eq!(resp[3], 0x01); // coils packed
    }

    #[test]
    fn test_build_read_coils_response_8_coils() {
        // All 8 coils ON
        let coils = vec![true; 8];
        let resp = build_rtu_read_coils_response(1, FC_READ_COILS, &coils);
        assert_eq!(resp[2], 1); // byte_count = 1
        assert_eq!(resp[3], 0xFF); // all bits set
    }

    #[test]
    fn test_build_read_coils_response_9_coils() {
        // 9 coils = 2 bytes
        let mut coils = vec![true; 8];
        coils.push(true); // 9th coil
        let resp = build_rtu_read_coils_response(1, FC_READ_COILS, &coils);
        assert_eq!(resp[2], 2); // byte_count = 2
        assert_eq!(resp[3], 0xFF); // first 8 coils
        assert_eq!(resp[4], 0x01); // 9th coil (bit 0)
    }

    #[test]
    fn test_build_read_coils_response_alternating() {
        // Alternating pattern: true, false, true, false...
        let coils = vec![true, false, true, false, true, false, true, false];
        let resp = build_rtu_read_coils_response(1, FC_READ_COILS, &coils);
        // LSB first: bit0=1, bit1=0, bit2=1, bit3=0, ... = 0x55
        assert_eq!(resp[3], 0x55);
    }

    #[test]
    fn test_build_read_coils_response_empty() {
        let coils: Vec<bool> = vec![];
        let resp = build_rtu_read_coils_response(1, FC_READ_COILS, &coils);
        assert_eq!(resp[2], 0); // byte_count = 0
        assert_eq!(resp.len(), 3); // only header
    }

    #[test]
    fn test_build_read_discrete_inputs_response() {
        // FC02 uses same format as FC01
        let inputs = vec![false, true, false, true];
        let resp = build_rtu_read_coils_response(1, FC_READ_DISCRETE_INPUTS, &inputs);
        assert_eq!(resp[1], FC_READ_DISCRETE_INPUTS);
        // LSB first: bit0=0, bit1=1, bit2=0, bit3=1 = 0x0A
        assert_eq!(resp[3], 0x0A);
    }

    // ========================================================================
    // Function code constant tests
    // ========================================================================

    #[test]
    fn test_function_code_values() {
        assert_eq!(FC_READ_COILS, 0x01);
        assert_eq!(FC_READ_DISCRETE_INPUTS, 0x02);
        assert_eq!(FC_READ_HOLDING_REGISTERS, 0x03);
        assert_eq!(FC_READ_INPUT_REGISTERS, 0x04);
        assert_eq!(FC_WRITE_SINGLE_COIL, 0x05);
        assert_eq!(FC_WRITE_SINGLE_REGISTER, 0x06);
        assert_eq!(FC_WRITE_MULTIPLE_COILS, 0x0F);
        assert_eq!(FC_WRITE_MULTIPLE_REGISTERS, 0x10);
    }

    #[test]
    fn test_exception_code_values() {
        assert_eq!(EX_ILLEGAL_FUNCTION, 0x01);
        assert_eq!(EX_ILLEGAL_DATA_ADDRESS, 0x02);
        assert_eq!(EX_ILLEGAL_DATA_VALUE, 0x03);
    }

    // ========================================================================
    // CRC frame validation tests
    // ========================================================================

    #[test]
    fn test_crc_frame_roundtrip() {
        // Build a valid frame with CRC
        let data = [0x01u8, 0x03, 0x00, 0x00, 0x00, 0x01];
        let crc = calculate_crc16(&data);
        let crc_bytes = crc.to_le_bytes();

        // Verify CRC by including it in calculation
        let mut full_frame = data.to_vec();
        full_frame.extend_from_slice(&crc_bytes);

        // CRC of entire frame (including CRC) should be 0
        // Actually in Modbus, we verify by comparing calculated vs received CRC
        let received_crc = u16::from_le_bytes([full_frame[6], full_frame[7]]);
        let calculated_crc = calculate_crc16(&full_frame[..6]);
        assert_eq!(received_crc, calculated_crc);
    }

    #[test]
    fn test_response_structure_fc03() {
        // Simulate FC03 Read Holding Registers response
        let values = vec![0x0064, 0x00C8]; // 100, 200
        let resp = build_rtu_read_response(1, FC_READ_HOLDING_REGISTERS, &values);

        assert_eq!(resp[0], 1); // slave_id
        assert_eq!(resp[1], FC_READ_HOLDING_REGISTERS); // function_code
        assert_eq!(resp[2], 4); // byte_count = 2 registers * 2 bytes
        // Register values in big-endian
        assert_eq!(resp[3], 0x00);
        assert_eq!(resp[4], 0x64);
        assert_eq!(resp[5], 0x00);
        assert_eq!(resp[6], 0xC8);
    }
}
