//! Modbus client wrapper for TCP/RTU transport dispatch.
//!
//! Provides a unified interface over `voltage_modbus` TCP and RTU clients,
//! so callers don't need to match on transport type at every call site.

use voltage_modbus::{DeviceLimits, ModbusClient, ModbusTcpClient};

#[cfg(feature = "modbus")]
use voltage_modbus::ModbusRtuClient;

/// Unified Modbus client wrapper for TCP and RTU transports.
pub enum ModbusClientWrapper {
    /// TCP client
    Tcp(ModbusTcpClient),
    /// RTU client (requires `modbus-rtu` feature)
    #[cfg(feature = "modbus")]
    Rtu(ModbusRtuClient),
}

impl ModbusClientWrapper {
    /// Read coils (FC01)
    pub async fn read_01(
        &mut self,
        slave_id: u8,
        address: u16,
        quantity: u16,
    ) -> voltage_modbus::ModbusResult<Vec<bool>> {
        match self {
            Self::Tcp(client) => client.read_01(slave_id, address, quantity).await,
            #[cfg(feature = "modbus")]
            Self::Rtu(client) => client.read_01(slave_id, address, quantity).await,
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
            Self::Tcp(client) => client.read_02(slave_id, address, quantity).await,
            #[cfg(feature = "modbus")]
            Self::Rtu(client) => client.read_02(slave_id, address, quantity).await,
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
            Self::Tcp(client) => client.read_03(slave_id, address, quantity).await,
            #[cfg(feature = "modbus")]
            Self::Rtu(client) => client.read_03(slave_id, address, quantity).await,
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
            Self::Tcp(client) => client.read_04(slave_id, address, quantity).await,
            #[cfg(feature = "modbus")]
            Self::Rtu(client) => client.read_04(slave_id, address, quantity).await,
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
            Self::Tcp(client) => {
                client
                    .read_03_batch(slave_id, address, quantity, limits)
                    .await
            },
            #[cfg(feature = "modbus")]
            Self::Rtu(client) => {
                client
                    .read_03_batch(slave_id, address, quantity, limits)
                    .await
            },
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
            Self::Tcp(client) => {
                client
                    .read_04_batch(slave_id, address, quantity, limits)
                    .await
            },
            #[cfg(feature = "modbus")]
            Self::Rtu(client) => {
                client
                    .read_04_batch(slave_id, address, quantity, limits)
                    .await
            },
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
            Self::Tcp(client) => client.write_05(slave_id, address, value).await,
            #[cfg(feature = "modbus")]
            Self::Rtu(client) => client.write_05(slave_id, address, value).await,
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
            Self::Tcp(client) => client.write_06(slave_id, address, value).await,
            #[cfg(feature = "modbus")]
            Self::Rtu(client) => client.write_06(slave_id, address, value).await,
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
            Self::Tcp(client) => client.write_0f(slave_id, address, values).await,
            #[cfg(feature = "modbus")]
            Self::Rtu(client) => client.write_0f(slave_id, address, values).await,
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
            Self::Tcp(client) => client.write_10(slave_id, address, values).await,
            #[cfg(feature = "modbus")]
            Self::Rtu(client) => client.write_10(slave_id, address, values).await,
        }
    }

    /// Close the connection
    pub async fn close(&mut self) -> voltage_modbus::ModbusResult<()> {
        match self {
            Self::Tcp(client) => client.close().await,
            #[cfg(feature = "modbus")]
            Self::Rtu(client) => client.close().await,
        }
    }
}
