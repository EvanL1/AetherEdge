//! Modbus TCP slave server implementation.
//!
//! Implements a simple Modbus TCP server using raw TCP sockets
//! and manual frame parsing for maximum control and simplicity.
//!
//! Supported function codes:
//! - FC01: Read Coils
//! - FC02: Read Discrete Inputs
//! - FC03: Read Holding Registers
//! - FC04: Read Input Registers
//! - FC05: Write Single Coil
//! - FC06: Write Single Register
//! - FC0F: Write Multiple Coils
//! - FC10: Write Multiple Registers

use crate::coils::CoilStore;
use crate::devices::{DeviceMap, generate_registers};
use crate::scenarios::{DeviceConfig, FaultConfig, FaultScenario};
use crate::state_machine::StateMachineStore;
use crate::writable::WritableRegisters;
use anyhow::Result;
use rand::Rng;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, error, info, warn};

// Modbus function codes
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
const EX_SERVER_DEVICE_FAILURE: u8 = 0x04;
const EX_SERVER_DEVICE_BUSY: u8 = 0x06;

/// Run the Modbus TCP server.
pub async fn run_server(
    addr: &str,
    device_map: DeviceMap,
    faults: FaultConfig,
    devices: &[DeviceConfig],
    sm_store: Arc<StateMachineStore>,
) -> Result<()> {
    let socket_addr: SocketAddr = addr.parse()?;
    let listener = TcpListener::bind(socket_addr).await?;

    let device_map = Arc::new(device_map);
    let faults = Arc::new(faults);
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

    info!("Modbus TCP server listening on {}", socket_addr);

    loop {
        match listener.accept().await {
            Ok((stream, peer_addr)) => {
                info!("New connection from {}", peer_addr);

                let device_map = Arc::clone(&device_map);
                let faults = Arc::clone(&faults);
                let writable = Arc::clone(&writable);
                let coil_store = Arc::clone(&coil_store);
                let sm_store = Arc::clone(&sm_store);

                tokio::spawn(async move {
                    if let Err(e) = handle_connection(
                        stream, device_map, faults, writable, coil_store, sm_store,
                    )
                    .await
                    {
                        error!("Connection error from {}: {}", peer_addr, e);
                    }
                    info!("Connection closed: {}", peer_addr);
                });
            },
            Err(e) => {
                error!("Accept error: {}", e);
            },
        }
    }
}

/// Handle a single TCP connection.
async fn handle_connection(
    mut stream: TcpStream,
    device_map: Arc<DeviceMap>,
    faults: Arc<FaultConfig>,
    writable: Arc<WritableRegisters>,
    coil_store: Arc<CoilStore>,
    sm_store: Arc<StateMachineStore>,
) -> Result<()> {
    let mut buf = [0u8; 260]; // Max Modbus frame size

    loop {
        // Read MBAP header (7 bytes)
        let n = stream.read(&mut buf[..7]).await?;
        if n == 0 {
            // Connection closed
            break;
        }
        if n < 7 {
            warn!("Incomplete MBAP header");
            continue;
        }

        // Parse MBAP header
        let transaction_id = u16::from_be_bytes([buf[0], buf[1]]);
        let protocol_id = u16::from_be_bytes([buf[2], buf[3]]);
        let length = u16::from_be_bytes([buf[4], buf[5]]) as usize;
        let unit_id = buf[6];

        if protocol_id != 0 {
            warn!("Invalid protocol ID: {}", protocol_id);
            continue;
        }

        // Read PDU (length - 1 bytes, since unit_id is included in length)
        let pdu_len = length - 1;
        if pdu_len > 253 {
            warn!("PDU too long: {}", pdu_len);
            continue;
        }

        let n = stream.read(&mut buf[7..7 + pdu_len]).await?;
        if n < pdu_len {
            warn!("Incomplete PDU");
            continue;
        }

        // Check for fault injection
        if faults.enabled
            && let Some(fault) = check_faults(&faults.scenarios)
            && let Some(response) = handle_fault(fault, &buf[..7 + pdu_len]).await
        {
            stream.write_all(&response).await?;
            continue;
        }

        // Process request
        let function_code = buf[7];
        let pdu = &buf[8..7 + pdu_len];

        let response = process_request(
            transaction_id,
            unit_id,
            function_code,
            pdu,
            &device_map,
            &writable,
            &coil_store,
            &sm_store,
        );

        stream.write_all(&response).await?;
    }

    Ok(())
}

/// Process a Modbus request and return a response.
fn process_request(
    transaction_id: u16,
    unit_id: u8,
    function_code: u8,
    pdu: &[u8],
    device_map: &DeviceMap,
    writable: &WritableRegisters,
    coil_store: &CoilStore,
    sm_store: &StateMachineStore,
) -> Vec<u8> {
    let timestamp_ms = chrono::Utc::now().timestamp_millis();

    match function_code {
        // ====================================================================
        // FC01: Read Coils
        // ====================================================================
        FC_READ_COILS => {
            if pdu.len() < 4 {
                return build_exception(
                    transaction_id,
                    unit_id,
                    function_code,
                    EX_ILLEGAL_DATA_VALUE,
                );
            }

            let start_addr = u16::from_be_bytes([pdu[0], pdu[1]]);
            let quantity = u16::from_be_bytes([pdu[2], pdu[3]]);

            debug!(
                "Read coils: unit={}, addr={}, count={}",
                unit_id, start_addr, quantity
            );

            if quantity == 0 || quantity > 2000 {
                return build_exception(
                    transaction_id,
                    unit_id,
                    function_code,
                    EX_ILLEGAL_DATA_VALUE,
                );
            }

            let coils = coil_store.read_coils(unit_id, start_addr, quantity);
            build_read_coils_response(transaction_id, unit_id, function_code, &coils)
        },

        // ====================================================================
        // FC02: Read Discrete Inputs
        // ====================================================================
        FC_READ_DISCRETE_INPUTS => {
            if pdu.len() < 4 {
                return build_exception(
                    transaction_id,
                    unit_id,
                    function_code,
                    EX_ILLEGAL_DATA_VALUE,
                );
            }

            let start_addr = u16::from_be_bytes([pdu[0], pdu[1]]);
            let quantity = u16::from_be_bytes([pdu[2], pdu[3]]);

            debug!(
                "Read discrete inputs: unit={}, addr={}, count={}",
                unit_id, start_addr, quantity
            );

            if quantity == 0 || quantity > 2000 {
                return build_exception(
                    transaction_id,
                    unit_id,
                    function_code,
                    EX_ILLEGAL_DATA_VALUE,
                );
            }

            let inputs = coil_store.read_discrete_inputs(unit_id, start_addr, quantity);
            build_read_coils_response(transaction_id, unit_id, function_code, &inputs)
        },

        // ====================================================================
        // FC03/FC04: Read Holding/Input Registers
        // ====================================================================
        FC_READ_HOLDING_REGISTERS | FC_READ_INPUT_REGISTERS => {
            if pdu.len() < 4 {
                return build_exception(
                    transaction_id,
                    unit_id,
                    function_code,
                    EX_ILLEGAL_DATA_VALUE,
                );
            }

            let start_addr = u16::from_be_bytes([pdu[0], pdu[1]]);
            let quantity = u16::from_be_bytes([pdu[2], pdu[3]]);

            debug!(
                "Read registers: unit={}, addr={}, count={}",
                unit_id, start_addr, quantity
            );

            if quantity == 0 || quantity > 125 {
                return build_exception(
                    transaction_id,
                    unit_id,
                    function_code,
                    EX_ILLEGAL_DATA_VALUE,
                );
            }

            // Get register values - prioritize writable storage over generated values
            let register_map = device_map.get(&unit_id).or_else(|| device_map.get(&1));

            if let Some(register_map) = register_map {
                // Generate base values from device waveforms
                let generated =
                    generate_registers(register_map, start_addr, quantity, timestamp_ms);

                // Override with any written values
                let values: Vec<u16> = (0..quantity)
                    .map(|offset| {
                        let addr = start_addr.wrapping_add(offset);
                        // If register was written, use that value; otherwise use generated
                        writable
                            .read(unit_id, addr)
                            .unwrap_or(generated[offset as usize])
                    })
                    .collect();

                build_read_response(transaction_id, unit_id, function_code, &values)
            } else {
                build_exception(
                    transaction_id,
                    unit_id,
                    function_code,
                    EX_ILLEGAL_DATA_ADDRESS,
                )
            }
        },

        // ====================================================================
        // FC05: Write Single Coil
        // ====================================================================
        FC_WRITE_SINGLE_COIL => {
            if pdu.len() < 4 {
                return build_exception(
                    transaction_id,
                    unit_id,
                    function_code,
                    EX_ILLEGAL_DATA_VALUE,
                );
            }

            let addr = u16::from_be_bytes([pdu[0], pdu[1]]);
            let value_raw = u16::from_be_bytes([pdu[2], pdu[3]]);

            // Modbus coil values: 0xFF00 = ON, 0x0000 = OFF
            let value = match value_raw {
                0xFF00 => true,
                0x0000 => false,
                _ => {
                    return build_exception(
                        transaction_id,
                        unit_id,
                        function_code,
                        EX_ILLEGAL_DATA_VALUE,
                    );
                },
            };

            debug!(
                "Write single coil: unit={}, addr={}, value={}",
                unit_id, addr, value
            );

            coil_store.write_coil(unit_id, addr, value);

            if let Some(sm) = sm_store.get(&unit_id)
                && let Some(new_state) = sm.on_coil_write(addr, value)
            {
                info!(
                    "State transition: unit={} -> {} (coil {}={})",
                    unit_id,
                    new_state.as_str(),
                    addr,
                    value
                );
            }

            build_write_single_coil_response(transaction_id, unit_id, addr, value)
        },

        // ====================================================================
        // FC06: Write Single Register
        // ====================================================================
        FC_WRITE_SINGLE_REGISTER => {
            if pdu.len() < 4 {
                return build_exception(
                    transaction_id,
                    unit_id,
                    function_code,
                    EX_ILLEGAL_DATA_VALUE,
                );
            }

            let addr = u16::from_be_bytes([pdu[0], pdu[1]]);
            let value = u16::from_be_bytes([pdu[2], pdu[3]]);

            debug!(
                "Write single register: unit={}, addr={}, value={}",
                unit_id, addr, value
            );

            // Store the written value for subsequent reads
            writable.write_single(unit_id, addr, value);

            if let Some(sm) = sm_store.get(&unit_id)
                && let Some(new_state) = sm.on_register_write(addr, value)
            {
                info!(
                    "State transition: unit={} -> {} (reg {}={})",
                    unit_id,
                    new_state.as_str(),
                    addr,
                    value
                );
            }

            build_write_single_response(transaction_id, unit_id, addr, value)
        },

        // ====================================================================
        // FC0F: Write Multiple Coils
        // ====================================================================
        FC_WRITE_MULTIPLE_COILS => {
            if pdu.len() < 5 {
                return build_exception(
                    transaction_id,
                    unit_id,
                    function_code,
                    EX_ILLEGAL_DATA_VALUE,
                );
            }

            let addr = u16::from_be_bytes([pdu[0], pdu[1]]);
            let quantity = u16::from_be_bytes([pdu[2], pdu[3]]);
            let byte_count = pdu[4] as usize;

            // Validate byte count matches quantity
            let expected_bytes = (quantity as usize).div_ceil(8);
            if byte_count != expected_bytes || pdu.len() < 5 + byte_count {
                return build_exception(
                    transaction_id,
                    unit_id,
                    function_code,
                    EX_ILLEGAL_DATA_VALUE,
                );
            }

            // Unpack coil values from bytes
            let coil_bytes = &pdu[5..5 + byte_count];
            let values = CoilStore::unpack_bytes_to_coils(coil_bytes, quantity);

            debug!(
                "Write multiple coils: unit={}, addr={}, count={}, values={:?}",
                unit_id, addr, quantity, values
            );

            coil_store.write_coils(unit_id, addr, &values);

            if let Some(sm) = sm_store.get(&unit_id) {
                for (i, &v) in values.iter().enumerate() {
                    if let Some(new_state) = sm.on_coil_write(addr + i as u16, v) {
                        info!(
                            "State transition: unit={} -> {} (coil {}={})",
                            unit_id,
                            new_state.as_str(),
                            addr + i as u16,
                            v
                        );
                    }
                }
            }

            build_write_multiple_coils_response(transaction_id, unit_id, addr, quantity)
        },

        // ====================================================================
        // FC10: Write Multiple Registers
        // ====================================================================
        FC_WRITE_MULTIPLE_REGISTERS => {
            if pdu.len() < 5 {
                return build_exception(
                    transaction_id,
                    unit_id,
                    function_code,
                    EX_ILLEGAL_DATA_VALUE,
                );
            }

            let addr = u16::from_be_bytes([pdu[0], pdu[1]]);
            let quantity = u16::from_be_bytes([pdu[2], pdu[3]]);
            let byte_count = pdu[4] as usize;

            // Validate byte count matches quantity
            let expected_bytes = quantity as usize * 2;
            if byte_count != expected_bytes || pdu.len() < 5 + byte_count {
                return build_exception(
                    transaction_id,
                    unit_id,
                    function_code,
                    EX_ILLEGAL_DATA_VALUE,
                );
            }

            // Parse register values from PDU
            let values: Vec<u16> = (0..quantity as usize)
                .map(|i| {
                    let offset = 5 + i * 2;
                    u16::from_be_bytes([pdu[offset], pdu[offset + 1]])
                })
                .collect();

            debug!(
                "Write multiple registers: unit={}, addr={}, count={}, values={:?}",
                unit_id, addr, quantity, values
            );

            // Store all written values
            writable.write_multiple(unit_id, addr, &values);

            if let Some(sm) = sm_store.get(&unit_id) {
                for (i, &v) in values.iter().enumerate() {
                    if let Some(new_state) = sm.on_register_write(addr + i as u16, v) {
                        info!(
                            "State transition: unit={} -> {} (reg {}={})",
                            unit_id,
                            new_state.as_str(),
                            addr + i as u16,
                            v
                        );
                    }
                }
            }

            build_write_multiple_response(transaction_id, unit_id, addr, quantity)
        },

        _ => {
            warn!("Unsupported function code: 0x{:02X}", function_code);
            build_exception(transaction_id, unit_id, function_code, EX_ILLEGAL_FUNCTION)
        },
    }
}

/// Build a Modbus read response.
fn build_read_response(
    transaction_id: u16,
    unit_id: u8,
    function_code: u8,
    values: &[u16],
) -> Vec<u8> {
    let byte_count = (values.len() * 2) as u8;
    let pdu_len = 2 + byte_count as usize; // function_code + byte_count + data

    let mut response = Vec::with_capacity(7 + pdu_len);

    // MBAP header
    response.extend_from_slice(&transaction_id.to_be_bytes());
    response.extend_from_slice(&0u16.to_be_bytes()); // protocol_id
    response.extend_from_slice(&((pdu_len + 1) as u16).to_be_bytes()); // length (includes unit_id)
    response.push(unit_id);

    // PDU
    response.push(function_code);
    response.push(byte_count);
    for value in values {
        response.extend_from_slice(&value.to_be_bytes());
    }

    response
}

/// Build a Modbus write single register response.
fn build_write_single_response(transaction_id: u16, unit_id: u8, addr: u16, value: u16) -> Vec<u8> {
    let mut response = Vec::with_capacity(12);

    // MBAP header
    response.extend_from_slice(&transaction_id.to_be_bytes());
    response.extend_from_slice(&0u16.to_be_bytes());
    response.extend_from_slice(&6u16.to_be_bytes()); // length
    response.push(unit_id);

    // PDU
    response.push(FC_WRITE_SINGLE_REGISTER);
    response.extend_from_slice(&addr.to_be_bytes());
    response.extend_from_slice(&value.to_be_bytes());

    response
}

/// Build a Modbus write multiple registers response.
fn build_write_multiple_response(
    transaction_id: u16,
    unit_id: u8,
    addr: u16,
    quantity: u16,
) -> Vec<u8> {
    let mut response = Vec::with_capacity(12);

    // MBAP header
    response.extend_from_slice(&transaction_id.to_be_bytes());
    response.extend_from_slice(&0u16.to_be_bytes());
    response.extend_from_slice(&6u16.to_be_bytes());
    response.push(unit_id);

    // PDU
    response.push(FC_WRITE_MULTIPLE_REGISTERS);
    response.extend_from_slice(&addr.to_be_bytes());
    response.extend_from_slice(&quantity.to_be_bytes());

    response
}

/// Build a Modbus read coils/discrete inputs response (FC01/FC02).
///
/// Coils are packed into bytes with LSB-first ordering within each byte.
/// For example, coils 0-7 go into byte 0, with coil 0 as bit 0 (LSB).
fn build_read_coils_response(
    transaction_id: u16,
    unit_id: u8,
    function_code: u8,
    coils: &[bool],
) -> Vec<u8> {
    // Pack coils into bytes (LSB-first)
    let packed_bytes = CoilStore::pack_coils_to_bytes(coils);
    let byte_count = packed_bytes.len() as u8;
    let pdu_len = 2 + byte_count as usize; // function_code + byte_count + data

    let mut response = Vec::with_capacity(7 + pdu_len);

    // MBAP header
    response.extend_from_slice(&transaction_id.to_be_bytes());
    response.extend_from_slice(&0u16.to_be_bytes()); // protocol_id
    response.extend_from_slice(&((pdu_len + 1) as u16).to_be_bytes()); // length (includes unit_id)
    response.push(unit_id);

    // PDU
    response.push(function_code);
    response.push(byte_count);
    response.extend_from_slice(&packed_bytes);

    response
}

/// Build a Modbus write single coil response (FC05).
///
/// Echo back the request: address + value (0xFF00 for ON, 0x0000 for OFF).
fn build_write_single_coil_response(
    transaction_id: u16,
    unit_id: u8,
    addr: u16,
    value: bool,
) -> Vec<u8> {
    let mut response = Vec::with_capacity(12);

    // MBAP header
    response.extend_from_slice(&transaction_id.to_be_bytes());
    response.extend_from_slice(&0u16.to_be_bytes());
    response.extend_from_slice(&6u16.to_be_bytes()); // length
    response.push(unit_id);

    // PDU
    response.push(FC_WRITE_SINGLE_COIL);
    response.extend_from_slice(&addr.to_be_bytes());
    // Modbus coil value encoding
    response.extend_from_slice(&(if value { 0xFF00u16 } else { 0x0000u16 }).to_be_bytes());

    response
}

/// Build a Modbus write multiple coils response (FC0F).
///
/// Response contains: start address + quantity of coils written.
fn build_write_multiple_coils_response(
    transaction_id: u16,
    unit_id: u8,
    addr: u16,
    quantity: u16,
) -> Vec<u8> {
    let mut response = Vec::with_capacity(12);

    // MBAP header
    response.extend_from_slice(&transaction_id.to_be_bytes());
    response.extend_from_slice(&0u16.to_be_bytes());
    response.extend_from_slice(&6u16.to_be_bytes());
    response.push(unit_id);

    // PDU
    response.push(FC_WRITE_MULTIPLE_COILS);
    response.extend_from_slice(&addr.to_be_bytes());
    response.extend_from_slice(&quantity.to_be_bytes());

    response
}

/// Build a Modbus exception response.
fn build_exception(
    transaction_id: u16,
    unit_id: u8,
    function_code: u8,
    exception_code: u8,
) -> Vec<u8> {
    let mut response = Vec::with_capacity(9);

    // MBAP header
    response.extend_from_slice(&transaction_id.to_be_bytes());
    response.extend_from_slice(&0u16.to_be_bytes());
    response.extend_from_slice(&3u16.to_be_bytes());
    response.push(unit_id);

    // PDU (error response)
    response.push(function_code | 0x80); // Error flag
    response.push(exception_code);

    response
}

/// Check if any fault should be triggered.
fn check_faults(scenarios: &[FaultScenario]) -> Option<&FaultScenario> {
    let mut rng = rand::thread_rng();

    for scenario in scenarios {
        let probability = match scenario {
            FaultScenario::ConnectionDrop { probability, .. } => *probability,
            FaultScenario::SlowResponse { probability, .. } => *probability,
            FaultScenario::InvalidResponse { probability } => *probability,
            FaultScenario::NoResponse { probability } => *probability,
        };

        if rng.r#gen::<f64>() < probability {
            return Some(scenario);
        }
    }

    None
}

/// Handle a triggered fault, returning an optional response.
async fn handle_fault(fault: &FaultScenario, request: &[u8]) -> Option<Vec<u8>> {
    let transaction_id = u16::from_be_bytes([request[0], request[1]]);
    let unit_id = request[6];
    let function_code = request[7];

    match fault {
        FaultScenario::ConnectionDrop { duration_sec, .. } => {
            warn!("Fault: Connection drop for {} seconds", duration_sec);
            tokio::time::sleep(Duration::from_secs(*duration_sec)).await;
            Some(build_exception(
                transaction_id,
                unit_id,
                function_code,
                EX_SERVER_DEVICE_FAILURE,
            ))
        },

        FaultScenario::SlowResponse { delay_ms, .. } => {
            warn!("Fault: Slow response ({}ms delay)", delay_ms);
            tokio::time::sleep(Duration::from_millis(*delay_ms)).await;
            Some(build_exception(
                transaction_id,
                unit_id,
                function_code,
                EX_SERVER_DEVICE_BUSY,
            ))
        },

        FaultScenario::InvalidResponse { .. } => {
            warn!("Fault: Invalid response");
            Some(build_exception(
                transaction_id,
                unit_id,
                function_code,
                EX_ILLEGAL_DATA_VALUE,
            ))
        },

        FaultScenario::NoResponse { .. } => {
            warn!("Fault: No response (timeout simulation)");
            tokio::time::sleep(Duration::from_secs(30)).await;
            None // No response
        },
    }
}
