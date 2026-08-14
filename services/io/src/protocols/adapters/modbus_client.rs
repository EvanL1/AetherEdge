//! Modbus client wrapper for TCP/RTU transport dispatch.
//!
//! Provides a unified interface over `voltage_modbus` TCP and RTU clients,
//! so callers don't need to match on transport type at every call site.

use std::future::Future;
use std::time::Duration;

use voltage_modbus::{DeviceLimits, ModbusClient, ModbusError, ModbusResult, ModbusTcpClient};

#[cfg(feature = "modbus")]
use voltage_modbus::ModbusRtuClient;

/// Unified Modbus client wrapper for TCP and RTU transports.
pub enum ModbusClientWrapper {
    /// TCP client
    Tcp {
        client: ModbusTcpClient,
        read_timeout: Duration,
        write_timeout: Duration,
        unusable: bool,
    },
    /// RTU client (requires `modbus-rtu` feature)
    #[cfg(feature = "modbus")]
    Rtu {
        client: ModbusRtuClient,
        read_timeout: Duration,
        write_timeout: Duration,
        unusable: bool,
    },
}

enum RequestDeadline<T> {
    Completed(ModbusResult<T>),
    Elapsed(ModbusError),
}

async fn wait_for_request<T>(
    io_timeout: Duration,
    operation: &'static str,
    request: impl Future<Output = ModbusResult<T>>,
) -> RequestDeadline<T> {
    match tokio::time::timeout(io_timeout, request).await {
        Ok(result) => RequestDeadline::Completed(result),
        Err(_) => RequestDeadline::Elapsed(ModbusError::timeout(
            format!("complete {operation} request"),
            io_timeout.as_millis().min(u128::from(u64::MAX)) as u64,
        )),
    }
}

macro_rules! request_with_deadline {
    ($client:expr, $io_timeout:expr, $unusable:expr, $operation:literal, $request:expr) => {{
        if *$unusable {
            Err(ModbusError::connection(
                "Modbus transport requires an explicit reconnect",
            ))
        } else {
            match wait_for_request($io_timeout, $operation, $request).await {
                RequestDeadline::Completed(result) => {
                    if matches!(&result, Err(error) if error.is_transport_error())
                        || !ModbusClient::is_connected($client)
                    {
                        // The dependency may otherwise reconnect implicitly on
                        // the next request without the channel's connect
                        // deadline. Poison this wrapper and return lifecycle
                        // ownership to ModbusChannel::connect.
                        *$unusable = true;
                        let _ = $client.close().await;
                    }
                    result
                },
                RequestDeadline::Elapsed(error) => {
                    // Cancelling a request can leave a partial frame in the
                    // transport. Close it before returning so the next
                    // operation cannot consume stale bytes as a fresh response.
                    *$unusable = true;
                    let _ = $client.close().await;
                    Err(error)
                },
            }
        }
    }};
}

impl ModbusClientWrapper {
    pub fn tcp(client: ModbusTcpClient, read_timeout: Duration, write_timeout: Duration) -> Self {
        Self::Tcp {
            client,
            read_timeout,
            write_timeout,
            unusable: false,
        }
    }

    #[cfg(feature = "modbus")]
    pub fn rtu(client: ModbusRtuClient, read_timeout: Duration, write_timeout: Duration) -> Self {
        Self::Rtu {
            client,
            read_timeout,
            write_timeout,
            unusable: false,
        }
    }

    /// Whether a transport error has made this connection unsafe to reuse.
    pub fn is_usable(&self) -> bool {
        match self {
            Self::Tcp { unusable, .. } => !unusable,
            #[cfg(feature = "modbus")]
            Self::Rtu { unusable, .. } => !unusable,
        }
    }

    /// Read coils (FC01)
    pub async fn read_01(
        &mut self,
        slave_id: u8,
        address: u16,
        quantity: u16,
    ) -> voltage_modbus::ModbusResult<Vec<bool>> {
        match self {
            Self::Tcp {
                client,
                read_timeout,
                unusable,
                ..
            } => request_with_deadline!(
                client,
                *read_timeout,
                unusable,
                "FC01 read",
                client.read_01(slave_id, address, quantity)
            ),
            #[cfg(feature = "modbus")]
            Self::Rtu {
                client,
                read_timeout,
                unusable,
                ..
            } => request_with_deadline!(
                client,
                *read_timeout,
                unusable,
                "FC01 read",
                client.read_01(slave_id, address, quantity)
            ),
        }
    }

    /// Read discrete inputs (FC02)
    pub async fn read_02(
        &mut self,
        slave_id: u8,
        address: u16,
        quantity: u16,
    ) -> voltage_modbus::ModbusResult<Vec<bool>> {
        match self {
            Self::Tcp {
                client,
                read_timeout,
                unusable,
                ..
            } => request_with_deadline!(
                client,
                *read_timeout,
                unusable,
                "FC02 read",
                client.read_02(slave_id, address, quantity)
            ),
            #[cfg(feature = "modbus")]
            Self::Rtu {
                client,
                read_timeout,
                unusable,
                ..
            } => request_with_deadline!(
                client,
                *read_timeout,
                unusable,
                "FC02 read",
                client.read_02(slave_id, address, quantity)
            ),
        }
    }

    /// Read holding registers (FC03)
    pub async fn read_03(
        &mut self,
        slave_id: u8,
        address: u16,
        quantity: u16,
    ) -> voltage_modbus::ModbusResult<Vec<u16>> {
        match self {
            Self::Tcp {
                client,
                read_timeout,
                unusable,
                ..
            } => request_with_deadline!(
                client,
                *read_timeout,
                unusable,
                "FC03 read",
                client.read_03(slave_id, address, quantity)
            ),
            #[cfg(feature = "modbus")]
            Self::Rtu {
                client,
                read_timeout,
                unusable,
                ..
            } => request_with_deadline!(
                client,
                *read_timeout,
                unusable,
                "FC03 read",
                client.read_03(slave_id, address, quantity)
            ),
        }
    }

    /// Read input registers (FC04)
    pub async fn read_04(
        &mut self,
        slave_id: u8,
        address: u16,
        quantity: u16,
    ) -> voltage_modbus::ModbusResult<Vec<u16>> {
        match self {
            Self::Tcp {
                client,
                read_timeout,
                unusable,
                ..
            } => request_with_deadline!(
                client,
                *read_timeout,
                unusable,
                "FC04 read",
                client.read_04(slave_id, address, quantity)
            ),
            #[cfg(feature = "modbus")]
            Self::Rtu {
                client,
                read_timeout,
                unusable,
                ..
            } => request_with_deadline!(
                client,
                *read_timeout,
                unusable,
                "FC04 read",
                client.read_04(slave_id, address, quantity)
            ),
        }
    }

    /// Batch read holding registers (FC03) with automatic chunking.
    pub async fn read_03_batch(
        &mut self,
        slave_id: u8,
        address: u16,
        quantity: u16,
        limits: &DeviceLimits,
    ) -> voltage_modbus::ModbusResult<Vec<u16>> {
        match self {
            Self::Tcp {
                client,
                read_timeout,
                unusable,
                ..
            } => request_with_deadline!(
                client,
                *read_timeout,
                unusable,
                "FC03 batch read",
                client.read_03_batch(slave_id, address, quantity, limits)
            ),
            #[cfg(feature = "modbus")]
            Self::Rtu {
                client,
                read_timeout,
                unusable,
                ..
            } => request_with_deadline!(
                client,
                *read_timeout,
                unusable,
                "FC03 batch read",
                client.read_03_batch(slave_id, address, quantity, limits)
            ),
        }
    }

    /// Batch read input registers (FC04) with automatic chunking.
    pub async fn read_04_batch(
        &mut self,
        slave_id: u8,
        address: u16,
        quantity: u16,
        limits: &DeviceLimits,
    ) -> voltage_modbus::ModbusResult<Vec<u16>> {
        match self {
            Self::Tcp {
                client,
                read_timeout,
                unusable,
                ..
            } => request_with_deadline!(
                client,
                *read_timeout,
                unusable,
                "FC04 batch read",
                client.read_04_batch(slave_id, address, quantity, limits)
            ),
            #[cfg(feature = "modbus")]
            Self::Rtu {
                client,
                read_timeout,
                unusable,
                ..
            } => request_with_deadline!(
                client,
                *read_timeout,
                unusable,
                "FC04 batch read",
                client.read_04_batch(slave_id, address, quantity, limits)
            ),
        }
    }

    /// Write single coil (FC05)
    pub async fn write_05(
        &mut self,
        slave_id: u8,
        address: u16,
        value: bool,
    ) -> voltage_modbus::ModbusResult<()> {
        match self {
            Self::Tcp {
                client,
                write_timeout,
                unusable,
                ..
            } => request_with_deadline!(
                client,
                *write_timeout,
                unusable,
                "FC05 write",
                client.write_05(slave_id, address, value)
            ),
            #[cfg(feature = "modbus")]
            Self::Rtu {
                client,
                write_timeout,
                unusable,
                ..
            } => request_with_deadline!(
                client,
                *write_timeout,
                unusable,
                "FC05 write",
                client.write_05(slave_id, address, value)
            ),
        }
    }

    /// Write single register (FC06)
    pub async fn write_06(
        &mut self,
        slave_id: u8,
        address: u16,
        value: u16,
    ) -> voltage_modbus::ModbusResult<()> {
        match self {
            Self::Tcp {
                client,
                write_timeout,
                unusable,
                ..
            } => request_with_deadline!(
                client,
                *write_timeout,
                unusable,
                "FC06 write",
                client.write_06(slave_id, address, value)
            ),
            #[cfg(feature = "modbus")]
            Self::Rtu {
                client,
                write_timeout,
                unusable,
                ..
            } => request_with_deadline!(
                client,
                *write_timeout,
                unusable,
                "FC06 write",
                client.write_06(slave_id, address, value)
            ),
        }
    }

    /// Write multiple coils (FC0F)
    pub async fn write_0f(
        &mut self,
        slave_id: u8,
        address: u16,
        values: &[bool],
    ) -> voltage_modbus::ModbusResult<()> {
        match self {
            Self::Tcp {
                client,
                write_timeout,
                unusable,
                ..
            } => request_with_deadline!(
                client,
                *write_timeout,
                unusable,
                "FC0F write",
                client.write_0f(slave_id, address, values)
            ),
            #[cfg(feature = "modbus")]
            Self::Rtu {
                client,
                write_timeout,
                unusable,
                ..
            } => request_with_deadline!(
                client,
                *write_timeout,
                unusable,
                "FC0F write",
                client.write_0f(slave_id, address, values)
            ),
        }
    }

    /// Write multiple registers (FC10)
    pub async fn write_10(
        &mut self,
        slave_id: u8,
        address: u16,
        values: &[u16],
    ) -> voltage_modbus::ModbusResult<()> {
        match self {
            Self::Tcp {
                client,
                write_timeout,
                unusable,
                ..
            } => request_with_deadline!(
                client,
                *write_timeout,
                unusable,
                "FC10 write",
                client.write_10(slave_id, address, values)
            ),
            #[cfg(feature = "modbus")]
            Self::Rtu {
                client,
                write_timeout,
                unusable,
                ..
            } => request_with_deadline!(
                client,
                *write_timeout,
                unusable,
                "FC10 write",
                client.write_10(slave_id, address, values)
            ),
        }
    }

    /// Close the connection
    pub async fn close(&mut self) -> voltage_modbus::ModbusResult<()> {
        match self {
            Self::Tcp { client, .. } => client.close().await,
            #[cfg(feature = "modbus")]
            Self::Rtu { client, .. } => client.close().await,
        }
    }
}
