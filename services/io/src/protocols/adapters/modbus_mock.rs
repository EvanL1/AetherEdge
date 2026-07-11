// Mock server uses unwrap in handler code for simplicity
#![allow(clippy::disallowed_methods)]
#![allow(clippy::manual_div_ceil)]

//! Mock Modbus TCP Server for testing.
//!
//! This module provides a `MockModbusServer` that simulates a Modbus TCP device
//! for unit testing without requiring real hardware.
//!
//! # Example
//!
//! ```rust,ignore
//! use crate::protocols::adapters::modbus_mock::MockModbusServer;
//!
//! #[tokio::test]
//! async fn test_modbus_read() {
//!     // Start mock server on random port
//!     let server = MockModbusServer::start_on_random_port().await.unwrap();
//!
//!     // Pre-set register values
//!     server.set_register(100, 0x1234);
//!     server.set_register(101, 0x5678);
//!
//!     // Connect client and test
//!     let client = ModbusTcpClient::connect(&server.address()).await.unwrap();
//!     let regs = client.read_03(1, 100, 2).await.unwrap();
//!
//!     assert_eq!(regs, vec![0x1234, 0x5678]);
//! }
//! ```

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tracing::{debug, error};

/// Modbus exception codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ModbusException {
    /// Illegal function code
    IllegalFunction = 0x01,
    /// Illegal data address
    IllegalDataAddress = 0x02,
    /// Illegal data value
    IllegalDataValue = 0x03,
    /// Server device busy
    ServerDeviceBusy = 0x06,
}

/// Error injection configuration.
#[derive(Debug, Clone)]
pub struct ErrorInjection {
    /// Function code to inject error for (0 = all)
    pub function_code: u8,
    /// Exception to return
    pub exception: ModbusException,
    /// Number of times to inject (0 = unlimited)
    pub count: u32,
}

/// Mock Modbus TCP server for testing.
///
/// Simulates a Modbus TCP device that responds to standard Modbus requests.
/// Supports:
/// - FC01: Read Coils
/// - FC02: Read Discrete Inputs
/// - FC03: Read Holding Registers
/// - FC04: Read Input Registers
/// - FC05: Write Single Coil
/// - FC06: Write Single Register
/// - FC0F: Write Multiple Coils
/// - FC10: Write Multiple Registers
pub struct MockModbusServer {
    /// Server address
    address: SocketAddr,
    /// Shared state
    state: Arc<ServerState>,
    /// Shutdown signal sender
    shutdown_tx: mpsc::Sender<()>,
}

/// Shared server state.
struct ServerState {
    /// Holding registers (FC03/FC06/FC10)
    holding_registers: RwLock<HashMap<u16, u16>>,
    /// Input registers (FC04) - read-only in real devices
    input_registers: RwLock<HashMap<u16, u16>>,
    /// Coils (FC01/FC05/FC0F)
    coils: RwLock<HashMap<u16, bool>>,
    /// Discrete inputs (FC02) - read-only in real devices
    discrete_inputs: RwLock<HashMap<u16, bool>>,
    /// Error injection queue
    error_injections: RwLock<Vec<ErrorInjection>>,
    /// Request counter
    request_count: AtomicU64,
    /// Running flag
    running: AtomicBool,
}

impl MockModbusServer {
    /// Start the mock server on a specific port.
    ///
    /// # Arguments
    /// * `port` - TCP port to listen on
    ///
    /// # Returns
    /// A new `MockModbusServer` instance
    pub async fn start(port: u16) -> std::io::Result<Self> {
        Self::start_on_addr(format!("127.0.0.1:{}", port)).await
    }

    /// Start the mock server on a random available port.
    ///
    /// Useful for parallel testing to avoid port conflicts.
    pub async fn start_on_random_port() -> std::io::Result<Self> {
        Self::start_on_addr("127.0.0.1:0").await
    }

    /// Start the mock server on a specific address.
    async fn start_on_addr(addr: impl AsRef<str>) -> std::io::Result<Self> {
        let listener = TcpListener::bind(addr.as_ref()).await?;
        let address = listener.local_addr()?;

        let state = Arc::new(ServerState {
            holding_registers: RwLock::new(HashMap::new()),
            input_registers: RwLock::new(HashMap::new()),
            coils: RwLock::new(HashMap::new()),
            discrete_inputs: RwLock::new(HashMap::new()),
            error_injections: RwLock::new(Vec::new()),
            request_count: AtomicU64::new(0),
            running: AtomicBool::new(true),
        });

        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);

        // Spawn server task
        let server_state = state.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    accept_result = listener.accept() => {
                        match accept_result {
                            Ok((stream, peer)) => {
                                debug!("[MockModbus] connection from {}", peer);
                                let conn_state = server_state.clone();
                                tokio::spawn(async move {
                                    if let Err(e) = Self::handle_connection(stream, conn_state).await {
                                        debug!("[MockModbus] connection error: {}", e);
                                    }
                                });
                            }
                            Err(e) => {
                                error!("[MockModbus] accept error: {}", e);
                            }
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        debug!("[MockModbus] shutdown signal received");
                        break;
                    }
                }
            }
        });

        debug!("[MockModbus] started on {}", address);

        Ok(Self {
            address,
            state,
            shutdown_tx,
        })
    }

    /// Get the server address (for client connection).
    pub fn address(&self) -> String {
        self.address.to_string()
    }

    /// Get the server port.
    pub fn port(&self) -> u16 {
        self.address.port()
    }

    /// Set a holding register value (FC03 readable, FC06/FC10 writable).
    pub fn set_register(&self, address: u16, value: u16) {
        if let Ok(mut regs) = self.state.holding_registers.write() {
            regs.insert(address, value);
        }
    }

    /// Set multiple consecutive holding registers.
    pub fn set_registers(&self, start_address: u16, values: &[u16]) {
        if let Ok(mut regs) = self.state.holding_registers.write() {
            for (i, &value) in values.iter().enumerate() {
                regs.insert(start_address + i as u16, value);
            }
        }
    }

    /// Get a holding register value.
    pub fn get_register(&self, address: u16) -> Option<u16> {
        self.state
            .holding_registers
            .read()
            .ok()
            .and_then(|regs| regs.get(&address).copied())
    }

    /// Set an input register value (FC04 readable).
    pub fn set_input_register(&self, address: u16, value: u16) {
        if let Ok(mut regs) = self.state.input_registers.write() {
            regs.insert(address, value);
        }
    }

    /// Set a coil value (FC01 readable, FC05/FC0F writable).
    pub fn set_coil(&self, address: u16, value: bool) {
        if let Ok(mut coils) = self.state.coils.write() {
            coils.insert(address, value);
        }
    }

    /// Get a coil value.
    pub fn get_coil(&self, address: u16) -> Option<bool> {
        self.state
            .coils
            .read()
            .ok()
            .and_then(|coils| coils.get(&address).copied())
    }

    /// Set a discrete input value (FC02 readable).
    pub fn set_discrete_input(&self, address: u16, value: bool) {
        if let Ok(mut inputs) = self.state.discrete_inputs.write() {
            inputs.insert(address, value);
        }
    }

    /// Inject an error for the next request(s).
    ///
    /// # Arguments
    /// * `function_code` - FC to inject error for (0 = any)
    /// * `exception` - Exception to return
    /// * `count` - Number of times to inject (0 = unlimited)
    pub fn inject_error(&self, function_code: u8, exception: ModbusException, count: u32) {
        if let Ok(mut errors) = self.state.error_injections.write() {
            errors.push(ErrorInjection {
                function_code,
                exception,
                count,
            });
        }
    }

    /// Clear all error injections.
    pub fn clear_error_injections(&self) {
        if let Ok(mut errors) = self.state.error_injections.write() {
            errors.clear();
        }
    }

    /// Get the total number of requests processed.
    pub fn request_count(&self) -> u64 {
        self.state.request_count.load(Ordering::Relaxed)
    }

    /// Handle a single client connection.
    async fn handle_connection(
        mut stream: TcpStream,
        state: Arc<ServerState>,
    ) -> std::io::Result<()> {
        let mut buf = [0u8; 260]; // Max Modbus ADU size

        loop {
            if !state.running.load(Ordering::Relaxed) {
                break;
            }

            // Read MBAP header (7 bytes) - must read exact bytes
            match stream.read_exact(&mut buf[..7]).await {
                Ok(_) => {},
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e),
            }

            // Parse MBAP header
            let transaction_id = u16::from_be_bytes([buf[0], buf[1]]);
            let protocol_id = u16::from_be_bytes([buf[2], buf[3]]);
            let length = u16::from_be_bytes([buf[4], buf[5]]) as usize;
            let unit_id = buf[6];

            // Validate protocol ID (should be 0 for Modbus)
            if protocol_id != 0 {
                continue;
            }

            // Read PDU (length - 1 bytes, since unit_id is included in length)
            let pdu_len = length.saturating_sub(1);
            if pdu_len == 0 || pdu_len > 253 {
                continue;
            }

            // Read exact PDU bytes
            match stream.read_exact(&mut buf[7..7 + pdu_len]).await {
                Ok(_) => {},
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e),
            }

            let function_code = buf[7];
            let pdu_data = &buf[8..7 + pdu_len];

            state.request_count.fetch_add(1, Ordering::Relaxed);

            // Debug: print full request (use eprintln for test visibility)
            #[cfg(test)]
            eprintln!(
                "[MockModbus] Request: TX={} Unit={} FC={:02X} PDU={:02X?} (raw: {:02X?})",
                transaction_id,
                unit_id,
                function_code,
                pdu_data,
                &buf[..7 + pdu_len]
            );

            // Check for error injection
            let injected_error = Self::check_error_injection(&state, function_code);

            let response = if let Some(exception) = injected_error {
                Self::build_exception_response(transaction_id, unit_id, function_code, exception)
            } else {
                Self::process_request(&state, transaction_id, unit_id, function_code, pdu_data)
            };

            #[cfg(test)]
            eprintln!("[MockModbus] Response: {:02X?}", &response);

            stream.write_all(&response).await?;
        }

        Ok(())
    }

    /// Check if error should be injected for this function code.
    fn check_error_injection(state: &ServerState, function_code: u8) -> Option<ModbusException> {
        let mut errors = state.error_injections.write().ok()?;

        // Find matching injection
        let mut result = None;
        let mut to_remove = Vec::new();

        for (i, injection) in errors.iter_mut().enumerate() {
            if injection.function_code == 0 || injection.function_code == function_code {
                result = Some(injection.exception);

                if injection.count > 0 {
                    injection.count -= 1;
                    if injection.count == 0 {
                        to_remove.push(i);
                    }
                }
                break;
            }
        }

        // Remove exhausted injections
        for i in to_remove.into_iter().rev() {
            errors.remove(i);
        }

        result
    }

    /// Process a Modbus request and build response.
    fn process_request(
        state: &ServerState,
        transaction_id: u16,
        unit_id: u8,
        function_code: u8,
        data: &[u8],
    ) -> Vec<u8> {
        match function_code {
            0x01 => Self::handle_read_coils(state, transaction_id, unit_id, data),
            0x02 => Self::handle_read_discrete_inputs(state, transaction_id, unit_id, data),
            0x03 => Self::handle_read_holding_registers(state, transaction_id, unit_id, data),
            0x04 => Self::handle_read_input_registers(state, transaction_id, unit_id, data),
            0x05 => Self::handle_write_single_coil(state, transaction_id, unit_id, data),
            0x06 => Self::handle_write_single_register(state, transaction_id, unit_id, data),
            0x0F => Self::handle_write_multiple_coils(state, transaction_id, unit_id, data),
            0x10 => Self::handle_write_multiple_registers(state, transaction_id, unit_id, data),
            _ => Self::build_exception_response(
                transaction_id,
                unit_id,
                function_code,
                ModbusException::IllegalFunction,
            ),
        }
    }

    /// FC01: Read Coils
    fn handle_read_coils(
        state: &ServerState,
        transaction_id: u16,
        unit_id: u8,
        data: &[u8],
    ) -> Vec<u8> {
        if data.len() < 4 {
            return Self::build_exception_response(
                transaction_id,
                unit_id,
                0x01,
                ModbusException::IllegalDataValue,
            );
        }

        let start_addr = u16::from_be_bytes([data[0], data[1]]);
        let quantity = u16::from_be_bytes([data[2], data[3]]);

        if quantity == 0 || quantity > 2000 {
            return Self::build_exception_response(
                transaction_id,
                unit_id,
                0x01,
                ModbusException::IllegalDataValue,
            );
        }

        let coils = state.coils.read().unwrap_or_else(|e| e.into_inner());
        let byte_count = (quantity as usize + 7) / 8;
        let mut response_data = vec![0u8; byte_count];

        for i in 0..quantity {
            let addr = start_addr + i;
            if let Some(&value) = coils.get(&addr)
                && value
            {
                let byte_idx = i as usize / 8;
                let bit_idx = i as usize % 8;
                response_data[byte_idx] |= 1 << bit_idx;
            }
        }

        Self::build_response(
            transaction_id,
            unit_id,
            0x01,
            byte_count as u8,
            &response_data,
        )
    }

    /// FC02: Read Discrete Inputs
    fn handle_read_discrete_inputs(
        state: &ServerState,
        transaction_id: u16,
        unit_id: u8,
        data: &[u8],
    ) -> Vec<u8> {
        if data.len() < 4 {
            return Self::build_exception_response(
                transaction_id,
                unit_id,
                0x02,
                ModbusException::IllegalDataValue,
            );
        }

        let start_addr = u16::from_be_bytes([data[0], data[1]]);
        let quantity = u16::from_be_bytes([data[2], data[3]]);

        if quantity == 0 || quantity > 2000 {
            return Self::build_exception_response(
                transaction_id,
                unit_id,
                0x02,
                ModbusException::IllegalDataValue,
            );
        }

        let inputs = state
            .discrete_inputs
            .read()
            .unwrap_or_else(|e| e.into_inner());
        let byte_count = (quantity as usize + 7) / 8;
        let mut response_data = vec![0u8; byte_count];

        for i in 0..quantity {
            let addr = start_addr + i;
            if let Some(&value) = inputs.get(&addr)
                && value
            {
                let byte_idx = i as usize / 8;
                let bit_idx = i as usize % 8;
                response_data[byte_idx] |= 1 << bit_idx;
            }
        }

        Self::build_response(
            transaction_id,
            unit_id,
            0x02,
            byte_count as u8,
            &response_data,
        )
    }

    /// FC03: Read Holding Registers
    fn handle_read_holding_registers(
        state: &ServerState,
        transaction_id: u16,
        unit_id: u8,
        data: &[u8],
    ) -> Vec<u8> {
        if data.len() < 4 {
            return Self::build_exception_response(
                transaction_id,
                unit_id,
                0x03,
                ModbusException::IllegalDataValue,
            );
        }

        let start_addr = u16::from_be_bytes([data[0], data[1]]);
        let quantity = u16::from_be_bytes([data[2], data[3]]);

        if quantity == 0 || quantity > 125 {
            return Self::build_exception_response(
                transaction_id,
                unit_id,
                0x03,
                ModbusException::IllegalDataValue,
            );
        }

        let regs = state
            .holding_registers
            .read()
            .unwrap_or_else(|e| e.into_inner());
        let byte_count = (quantity * 2) as u8;
        let mut response_data = Vec::with_capacity(quantity as usize * 2);

        for i in 0..quantity {
            let addr = start_addr + i;
            let value = regs.get(&addr).copied().unwrap_or(0);
            response_data.extend_from_slice(&value.to_be_bytes());
        }

        Self::build_response(transaction_id, unit_id, 0x03, byte_count, &response_data)
    }

    /// FC04: Read Input Registers
    fn handle_read_input_registers(
        state: &ServerState,
        transaction_id: u16,
        unit_id: u8,
        data: &[u8],
    ) -> Vec<u8> {
        if data.len() < 4 {
            return Self::build_exception_response(
                transaction_id,
                unit_id,
                0x04,
                ModbusException::IllegalDataValue,
            );
        }

        let start_addr = u16::from_be_bytes([data[0], data[1]]);
        let quantity = u16::from_be_bytes([data[2], data[3]]);

        if quantity == 0 || quantity > 125 {
            return Self::build_exception_response(
                transaction_id,
                unit_id,
                0x04,
                ModbusException::IllegalDataValue,
            );
        }

        let regs = state
            .input_registers
            .read()
            .unwrap_or_else(|e| e.into_inner());
        let byte_count = (quantity * 2) as u8;
        let mut response_data = Vec::with_capacity(quantity as usize * 2);

        for i in 0..quantity {
            let addr = start_addr + i;
            let value = regs.get(&addr).copied().unwrap_or(0);
            response_data.extend_from_slice(&value.to_be_bytes());
        }

        Self::build_response(transaction_id, unit_id, 0x04, byte_count, &response_data)
    }

    /// FC05: Write Single Coil
    fn handle_write_single_coil(
        state: &ServerState,
        transaction_id: u16,
        unit_id: u8,
        data: &[u8],
    ) -> Vec<u8> {
        if data.len() < 4 {
            return Self::build_exception_response(
                transaction_id,
                unit_id,
                0x05,
                ModbusException::IllegalDataValue,
            );
        }

        let address = u16::from_be_bytes([data[0], data[1]]);
        let value = u16::from_be_bytes([data[2], data[3]]);

        // Valid values: 0x0000 (OFF) or 0xFF00 (ON)
        let coil_value = match value {
            0x0000 => false,
            0xFF00 => true,
            _ => {
                return Self::build_exception_response(
                    transaction_id,
                    unit_id,
                    0x05,
                    ModbusException::IllegalDataValue,
                );
            },
        };

        if let Ok(mut coils) = state.coils.write() {
            coils.insert(address, coil_value);
        }

        // Echo request as response
        Self::build_echo_response(transaction_id, unit_id, 0x05, data)
    }

    /// FC06: Write Single Register
    fn handle_write_single_register(
        state: &ServerState,
        transaction_id: u16,
        unit_id: u8,
        data: &[u8],
    ) -> Vec<u8> {
        if data.len() < 4 {
            return Self::build_exception_response(
                transaction_id,
                unit_id,
                0x06,
                ModbusException::IllegalDataValue,
            );
        }

        let address = u16::from_be_bytes([data[0], data[1]]);
        let value = u16::from_be_bytes([data[2], data[3]]);

        if let Ok(mut regs) = state.holding_registers.write() {
            regs.insert(address, value);
        }

        // Echo request as response
        Self::build_echo_response(transaction_id, unit_id, 0x06, data)
    }

    /// FC0F: Write Multiple Coils
    fn handle_write_multiple_coils(
        state: &ServerState,
        transaction_id: u16,
        unit_id: u8,
        data: &[u8],
    ) -> Vec<u8> {
        if data.len() < 5 {
            return Self::build_exception_response(
                transaction_id,
                unit_id,
                0x0F,
                ModbusException::IllegalDataValue,
            );
        }

        let start_addr = u16::from_be_bytes([data[0], data[1]]);
        let quantity = u16::from_be_bytes([data[2], data[3]]);
        let byte_count = data[4] as usize;

        if quantity == 0 || quantity > 1968 || data.len() < 5 + byte_count {
            return Self::build_exception_response(
                transaction_id,
                unit_id,
                0x0F,
                ModbusException::IllegalDataValue,
            );
        }

        let coil_data = &data[5..5 + byte_count];

        if let Ok(mut coils) = state.coils.write() {
            for i in 0..quantity {
                let byte_idx = i as usize / 8;
                let bit_idx = i as usize % 8;
                let value = (coil_data[byte_idx] >> bit_idx) & 1 == 1;
                coils.insert(start_addr + i, value);
            }
        }

        // Response: start address + quantity
        Self::build_response(transaction_id, unit_id, 0x0F, 0, &data[0..4])
    }

    /// FC10: Write Multiple Registers
    fn handle_write_multiple_registers(
        state: &ServerState,
        transaction_id: u16,
        unit_id: u8,
        data: &[u8],
    ) -> Vec<u8> {
        if data.len() < 5 {
            return Self::build_exception_response(
                transaction_id,
                unit_id,
                0x10,
                ModbusException::IllegalDataValue,
            );
        }

        let start_addr = u16::from_be_bytes([data[0], data[1]]);
        let quantity = u16::from_be_bytes([data[2], data[3]]);
        let byte_count = data[4] as usize;

        if quantity == 0 || quantity > 123 || byte_count != quantity as usize * 2 {
            return Self::build_exception_response(
                transaction_id,
                unit_id,
                0x10,
                ModbusException::IllegalDataValue,
            );
        }

        if data.len() < 5 + byte_count {
            return Self::build_exception_response(
                transaction_id,
                unit_id,
                0x10,
                ModbusException::IllegalDataValue,
            );
        }

        let reg_data = &data[5..5 + byte_count];

        if let Ok(mut regs) = state.holding_registers.write() {
            for i in 0..quantity as usize {
                let value = u16::from_be_bytes([reg_data[i * 2], reg_data[i * 2 + 1]]);
                regs.insert(start_addr + i as u16, value);
            }
        }

        // Response: start address + quantity
        Self::build_response(transaction_id, unit_id, 0x10, 0, &data[0..4])
    }

    /// Build a normal response PDU.
    fn build_response(
        transaction_id: u16,
        unit_id: u8,
        function_code: u8,
        byte_count: u8,
        data: &[u8],
    ) -> Vec<u8> {
        let pdu_len = if byte_count > 0 {
            2 + data.len() // FC + byte_count + data
        } else {
            1 + data.len() // FC + data (for write responses)
        };

        let length = 1 + pdu_len; // unit_id + PDU

        let mut response = Vec::with_capacity(7 + pdu_len);
        response.extend_from_slice(&transaction_id.to_be_bytes());
        response.extend_from_slice(&0u16.to_be_bytes()); // protocol ID
        response.extend_from_slice(&(length as u16).to_be_bytes());
        response.push(unit_id);
        response.push(function_code);
        if byte_count > 0 {
            response.push(byte_count);
        }
        response.extend_from_slice(data);

        response
    }

    /// Build an echo response (for write commands).
    fn build_echo_response(
        transaction_id: u16,
        unit_id: u8,
        function_code: u8,
        data: &[u8],
    ) -> Vec<u8> {
        Self::build_response(transaction_id, unit_id, function_code, 0, data)
    }

    /// Build an exception response.
    fn build_exception_response(
        transaction_id: u16,
        unit_id: u8,
        function_code: u8,
        exception: ModbusException,
    ) -> Vec<u8> {
        let mut response = Vec::with_capacity(9);
        response.extend_from_slice(&transaction_id.to_be_bytes());
        response.extend_from_slice(&0u16.to_be_bytes()); // protocol ID
        response.extend_from_slice(&3u16.to_be_bytes()); // length: unit_id + error_fc + exception
        response.push(unit_id);
        response.push(function_code | 0x80); // Error flag
        response.push(exception as u8);

        response
    }

    /// Stop the server.
    pub async fn stop(&self) {
        self.state.running.store(false, Ordering::Relaxed);
        let _ = self.shutdown_tx.send(()).await;
    }
}

impl Drop for MockModbusServer {
    fn drop(&mut self) {
        self.state.running.store(false, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_server_start_stop() {
        let server = MockModbusServer::start_on_random_port().await.unwrap();
        assert!(server.port() > 0);
        server.stop().await;
    }

    #[tokio::test]
    async fn test_set_get_register() {
        let server = MockModbusServer::start_on_random_port().await.unwrap();

        server.set_register(100, 0x1234);
        assert_eq!(server.get_register(100), Some(0x1234));
        assert_eq!(server.get_register(101), None);

        server.stop().await;
    }

    #[tokio::test]
    async fn test_set_get_coil() {
        let server = MockModbusServer::start_on_random_port().await.unwrap();

        server.set_coil(0, true);
        server.set_coil(1, false);

        assert_eq!(server.get_coil(0), Some(true));
        assert_eq!(server.get_coil(1), Some(false));
        assert_eq!(server.get_coil(2), None);

        server.stop().await;
    }

    #[tokio::test]
    async fn test_set_registers_batch() {
        let server = MockModbusServer::start_on_random_port().await.unwrap();

        server.set_registers(100, &[0x1111, 0x2222, 0x3333]);

        assert_eq!(server.get_register(100), Some(0x1111));
        assert_eq!(server.get_register(101), Some(0x2222));
        assert_eq!(server.get_register(102), Some(0x3333));

        server.stop().await;
    }

    #[tokio::test]
    async fn test_error_injection() {
        let server = MockModbusServer::start_on_random_port().await.unwrap();

        server.inject_error(0x03, ModbusException::IllegalDataAddress, 2);

        // Error should be injected twice
        // (actual verification would require connecting a client)

        server.clear_error_injections();
        server.stop().await;
    }
}

// =============================================================================
// Integration tests: MockModbusServer + ModbusTcpClient
// =============================================================================
// These tests verify end-to-end Modbus TCP protocol communication using
// the mock server and the production client implementation.

#[cfg(test)]
mod integration_tests {
    use super::*;
    use std::time::Duration;
    use voltage_modbus::{ModbusClient, ModbusTcpClient};

    /// Helper to create a connected client
    async fn create_client(port: u16) -> ModbusTcpClient {
        let addr = format!("127.0.0.1:{}", port);
        ModbusTcpClient::from_address(&addr, Duration::from_secs(5))
            .await
            .expect("Failed to connect to mock server")
    }

    // =========================================================================
    // FC01: Read Coils
    // =========================================================================

    #[tokio::test]
    async fn test_fc01_read_coils_single() {
        let server = MockModbusServer::start_on_random_port().await.unwrap();
        let port = server.port();

        // Set up coil values
        server.set_coil(0, true);

        let mut client = create_client(port).await;
        let result = client.read_01(1, 0, 1).await.expect("FC01 read failed");

        assert_eq!(result.len(), 1);
        assert!(result[0]);

        server.stop().await;
    }

    #[tokio::test]
    async fn test_fc01_read_coils_multiple() {
        let server = MockModbusServer::start_on_random_port().await.unwrap();
        let port = server.port();

        // Set up coil pattern: true, false, true, false, true
        server.set_coil(10, true);
        server.set_coil(11, false);
        server.set_coil(12, true);
        server.set_coil(13, false);
        server.set_coil(14, true);

        let mut client = create_client(port).await;
        let result = client.read_01(1, 10, 5).await.expect("FC01 read failed");

        assert_eq!(result, vec![true, false, true, false, true]);

        server.stop().await;
    }

    #[tokio::test]
    async fn test_fc01_read_coils_unset_returns_false() {
        let server = MockModbusServer::start_on_random_port().await.unwrap();
        let port = server.port();

        // Don't set any coils - should return false for unset addresses
        let mut client = create_client(port).await;
        let result = client.read_01(1, 100, 3).await.expect("FC01 read failed");

        assert_eq!(result, vec![false, false, false]);

        server.stop().await;
    }

    // =========================================================================
    // FC02: Read Discrete Inputs
    // =========================================================================

    #[tokio::test]
    async fn test_fc02_read_discrete_inputs() {
        let server = MockModbusServer::start_on_random_port().await.unwrap();
        let port = server.port();

        server.set_discrete_input(0, true);
        server.set_discrete_input(1, true);
        server.set_discrete_input(2, false);

        let mut client = create_client(port).await;
        let result = client.read_02(1, 0, 3).await.expect("FC02 read failed");

        assert_eq!(result, vec![true, true, false]);

        server.stop().await;
    }

    // =========================================================================
    // FC03: Read Holding Registers
    // =========================================================================

    #[tokio::test]
    async fn test_fc03_read_holding_registers_single() {
        let server = MockModbusServer::start_on_random_port().await.unwrap();
        let port = server.port();

        server.set_register(100, 0x1234);

        let mut client = create_client(port).await;
        let result = client.read_03(1, 100, 1).await.expect("FC03 read failed");

        assert_eq!(result, vec![0x1234]);

        server.stop().await;
    }

    #[tokio::test]
    async fn test_fc03_read_holding_registers_multiple() {
        let server = MockModbusServer::start_on_random_port().await.unwrap();
        let port = server.port();

        // Set up registers for a 32-bit float (42.5 in IEEE 754 = 0x422A0000)
        server.set_registers(100, &[0x422A, 0x0000]);

        let mut client = create_client(port).await;
        let result = client.read_03(1, 100, 2).await.expect("FC03 read failed");

        assert_eq!(result, vec![0x422A, 0x0000]);

        // Decode as float32 (big-endian)
        let bytes = [
            (result[0] >> 8) as u8,
            result[0] as u8,
            (result[1] >> 8) as u8,
            result[1] as u8,
        ];
        let value = f32::from_be_bytes(bytes);
        assert!((value - 42.5).abs() < 0.001);

        server.stop().await;
    }

    #[tokio::test]
    async fn test_fc03_read_unset_registers_returns_zero() {
        let server = MockModbusServer::start_on_random_port().await.unwrap();
        let port = server.port();

        let mut client = create_client(port).await;
        let result = client.read_03(1, 200, 3).await.expect("FC03 read failed");

        assert_eq!(result, vec![0, 0, 0]);

        server.stop().await;
    }

    // =========================================================================
    // FC04: Read Input Registers
    // =========================================================================

    #[tokio::test]
    async fn test_fc04_read_input_registers() {
        let server = MockModbusServer::start_on_random_port().await.unwrap();
        let port = server.port();

        server.set_input_register(0, 0xABCD);
        server.set_input_register(1, 0xEF01);

        let mut client = create_client(port).await;
        let result = client.read_04(1, 0, 2).await.expect("FC04 read failed");

        assert_eq!(result, vec![0xABCD, 0xEF01]);

        server.stop().await;
    }

    // =========================================================================
    // FC05: Write Single Coil
    // =========================================================================

    #[tokio::test]
    async fn test_fc05_write_single_coil_on() {
        let server = MockModbusServer::start_on_random_port().await.unwrap();
        let port = server.port();

        let mut client = create_client(port).await;
        client
            .write_05(1, 50, true)
            .await
            .expect("FC05 write failed");

        // Verify the coil was written
        assert_eq!(server.get_coil(50), Some(true));

        server.stop().await;
    }

    #[tokio::test]
    async fn test_fc05_write_single_coil_off() {
        let server = MockModbusServer::start_on_random_port().await.unwrap();
        let port = server.port();

        // Pre-set coil to true
        server.set_coil(60, true);

        let mut client = create_client(port).await;
        client
            .write_05(1, 60, false)
            .await
            .expect("FC05 write failed");

        // Verify the coil was turned off
        assert_eq!(server.get_coil(60), Some(false));

        server.stop().await;
    }

    // =========================================================================
    // FC06: Write Single Register
    // =========================================================================

    #[tokio::test]
    async fn test_fc06_write_single_register() {
        let server = MockModbusServer::start_on_random_port().await.unwrap();
        let port = server.port();

        let mut client = create_client(port).await;
        client
            .write_06(1, 300, 0x5678)
            .await
            .expect("FC06 write failed");

        // Verify the register was written
        assert_eq!(server.get_register(300), Some(0x5678));

        server.stop().await;
    }

    #[tokio::test]
    async fn test_fc06_write_then_read() {
        let server = MockModbusServer::start_on_random_port().await.unwrap();
        let port = server.port();

        let mut client = create_client(port).await;

        // Write a value
        client
            .write_06(1, 400, 0xBEEF)
            .await
            .expect("FC06 write failed");

        // Read it back
        let result = client.read_03(1, 400, 1).await.expect("FC03 read failed");
        assert_eq!(result, vec![0xBEEF]);

        server.stop().await;
    }

    // =========================================================================
    // FC0F: Write Multiple Coils
    // =========================================================================

    #[tokio::test]
    async fn test_fc0f_write_multiple_coils() {
        let server = MockModbusServer::start_on_random_port().await.unwrap();
        let port = server.port();

        let mut client = create_client(port).await;
        let coils = vec![true, false, true, true, false];
        client
            .write_0f(1, 100, &coils)
            .await
            .expect("FC0F write failed");

        // Verify each coil
        assert_eq!(server.get_coil(100), Some(true));
        assert_eq!(server.get_coil(101), Some(false));
        assert_eq!(server.get_coil(102), Some(true));
        assert_eq!(server.get_coil(103), Some(true));
        assert_eq!(server.get_coil(104), Some(false));

        server.stop().await;
    }

    // =========================================================================
    // FC10: Write Multiple Registers
    // =========================================================================

    #[tokio::test]
    async fn test_fc10_write_multiple_registers() {
        let server = MockModbusServer::start_on_random_port().await.unwrap();
        let port = server.port();

        let mut client = create_client(port).await;
        let values = vec![0x1111, 0x2222, 0x3333, 0x4444];
        client
            .write_10(1, 500, &values)
            .await
            .expect("FC10 write failed");

        // Verify each register
        assert_eq!(server.get_register(500), Some(0x1111));
        assert_eq!(server.get_register(501), Some(0x2222));
        assert_eq!(server.get_register(502), Some(0x3333));
        assert_eq!(server.get_register(503), Some(0x4444));

        server.stop().await;
    }

    #[tokio::test]
    async fn test_fc10_write_then_read() {
        let server = MockModbusServer::start_on_random_port().await.unwrap();
        let port = server.port();

        let mut client = create_client(port).await;

        // Write multiple registers
        let values = vec![0xAAAA, 0xBBBB];
        client
            .write_10(1, 600, &values)
            .await
            .expect("FC10 write failed");

        // Read them back
        let result = client.read_03(1, 600, 2).await.expect("FC03 read failed");
        assert_eq!(result, vec![0xAAAA, 0xBBBB]);

        server.stop().await;
    }

    // =========================================================================
    // Error Handling Tests
    // =========================================================================

    #[tokio::test]
    async fn test_error_injection_illegal_data_address() {
        let server = MockModbusServer::start_on_random_port().await.unwrap();
        let port = server.port();

        // Inject error for FC03
        server.inject_error(0x03, ModbusException::IllegalDataAddress, 1);

        let mut client = create_client(port).await;
        let result = client.read_03(1, 100, 1).await;

        // Should return an error
        assert!(result.is_err());
        let err = result.unwrap_err();
        // Verify it's an exception response (error code contains 0x83 = FC03 + 0x80)
        assert!(format!("{:?}", err).contains("Exception") || format!("{:?}", err).contains("02"));

        server.stop().await;
    }

    #[tokio::test]
    async fn test_error_injection_one_time() {
        let server = MockModbusServer::start_on_random_port().await.unwrap();
        let port = server.port();

        server.set_register(100, 0x1234);

        // Inject error for only 1 request
        server.inject_error(0x03, ModbusException::ServerDeviceBusy, 1);

        let mut client = create_client(port).await;

        // First request should fail
        let result1 = client.read_03(1, 100, 1).await;
        assert!(result1.is_err());

        // Second request should succeed
        let result2 = client.read_03(1, 100, 1).await;
        assert!(result2.is_ok());
        assert_eq!(result2.unwrap(), vec![0x1234]);

        server.stop().await;
    }

    // =========================================================================
    // Protocol Stress Tests
    // =========================================================================

    #[tokio::test]
    async fn test_multiple_rapid_requests() {
        let server = MockModbusServer::start_on_random_port().await.unwrap();
        let port = server.port();

        // Set up some data
        for i in 0..10u16 {
            server.set_register(i, i * 100);
        }

        let mut client = create_client(port).await;

        // Send many requests rapidly
        for i in 0..10u16 {
            let result = client.read_03(1, i, 1).await.expect("Rapid request failed");
            assert_eq!(result, vec![i * 100]);
        }

        // Verify request count
        assert_eq!(server.request_count(), 10);

        server.stop().await;
    }

    #[tokio::test]
    async fn test_mixed_read_write_operations() {
        let server = MockModbusServer::start_on_random_port().await.unwrap();
        let port = server.port();

        let mut client = create_client(port).await;

        // Interleaved reads and writes
        client.write_06(1, 0, 100).await.unwrap();
        let r1 = client.read_03(1, 0, 1).await.unwrap();
        assert_eq!(r1, vec![100]);

        client.write_06(1, 1, 200).await.unwrap();
        let r2 = client.read_03(1, 0, 2).await.unwrap();
        assert_eq!(r2, vec![100, 200]);

        client.write_10(1, 2, &[300, 400]).await.unwrap();
        let r3 = client.read_03(1, 0, 4).await.unwrap();
        assert_eq!(r3, vec![100, 200, 300, 400]);

        server.stop().await;
    }

    // =========================================================================
    // Data Type Conversion Tests
    // =========================================================================

    #[tokio::test]
    async fn test_float32_big_endian() {
        let server = MockModbusServer::start_on_random_port().await.unwrap();
        let port = server.port();

        // 123.456 in IEEE 754 big-endian: 0x42F6E979
        let value: f32 = 123.456;
        let bytes = value.to_be_bytes();
        let reg0 = u16::from_be_bytes([bytes[0], bytes[1]]);
        let reg1 = u16::from_be_bytes([bytes[2], bytes[3]]);

        server.set_registers(100, &[reg0, reg1]);

        let mut client = create_client(port).await;
        let result = client.read_03(1, 100, 2).await.unwrap();

        // Decode back to float
        let decoded_bytes = [
            (result[0] >> 8) as u8,
            result[0] as u8,
            (result[1] >> 8) as u8,
            result[1] as u8,
        ];
        let decoded = f32::from_be_bytes(decoded_bytes);

        assert!((decoded - 123.456).abs() < 0.001);

        server.stop().await;
    }

    #[tokio::test]
    async fn test_int32_signed() {
        let server = MockModbusServer::start_on_random_port().await.unwrap();
        let port = server.port();

        // -1000 as signed 32-bit: 0xFFFFFC18
        let value: i32 = -1000;
        let bytes = value.to_be_bytes();
        let reg0 = u16::from_be_bytes([bytes[0], bytes[1]]);
        let reg1 = u16::from_be_bytes([bytes[2], bytes[3]]);

        server.set_registers(200, &[reg0, reg1]);

        let mut client = create_client(port).await;
        let result = client.read_03(1, 200, 2).await.unwrap();

        // Decode back to i32
        let decoded_bytes = [
            (result[0] >> 8) as u8,
            result[0] as u8,
            (result[1] >> 8) as u8,
            result[1] as u8,
        ];
        let decoded = i32::from_be_bytes(decoded_bytes);

        assert_eq!(decoded, -1000);

        server.stop().await;
    }

    // =========================================================================
    // Low-level Protocol Debug Test
    // =========================================================================

    /// Test raw byte-level communication to debug response format
    #[tokio::test]
    async fn test_raw_fc03_response_format() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpStream;

        let server = MockModbusServer::start_on_random_port().await.unwrap();
        let port = server.port();

        // Set register 100 = 0x1234
        server.set_register(100, 0x1234);

        // Connect directly to server
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port))
            .await
            .expect("Failed to connect");

        // Build FC03 request: read 1 register at address 100
        // MBAP Header: TX=0x0001, Protocol=0x0000, Length=0x0006, Unit=0x01
        // PDU: FC=0x03, StartAddr=0x0064, Quantity=0x0001
        let request: [u8; 12] = [
            0x00, 0x01, // Transaction ID
            0x00, 0x00, // Protocol ID
            0x00, 0x06, // Length (Unit ID + PDU = 1 + 5 = 6)
            0x01, // Unit ID
            0x03, // Function Code
            0x00, 0x64, // Start Address (100)
            0x00, 0x01, // Quantity (1 register)
        ];

        stream
            .write_all(&request)
            .await
            .expect("Failed to send request");

        // Read response
        let mut response = [0u8; 32];
        let n = stream
            .read(&mut response)
            .await
            .expect("Failed to read response");

        // Expected response format:
        // MBAP: TX=0x0001, Protocol=0x0000, Length=0x0005, Unit=0x01
        // PDU: FC=0x03, ByteCount=0x02, Data=0x1234
        // Total: 11 bytes
        println!("Response ({} bytes): {:02X?}", n, &response[..n]);

        // Verify MBAP header
        assert_eq!(response[0..2], [0x00, 0x01], "Transaction ID mismatch");
        assert_eq!(response[2..4], [0x00, 0x00], "Protocol ID should be 0");

        let length = u16::from_be_bytes([response[4], response[5]]);
        println!("Length field: {}", length);
        assert_eq!(
            length, 5,
            "Length should be 5 (unit_id + FC + BC + 2 bytes data)"
        );

        assert_eq!(response[6], 0x01, "Unit ID mismatch");
        assert_eq!(response[7], 0x03, "Function code mismatch");
        assert_eq!(response[8], 0x02, "Byte count should be 2");

        // Verify data
        let reg_value = u16::from_be_bytes([response[9], response[10]]);
        assert_eq!(reg_value, 0x1234, "Register value mismatch");

        assert_eq!(n, 11, "Response should be exactly 11 bytes");

        server.stop().await;
    }
}
