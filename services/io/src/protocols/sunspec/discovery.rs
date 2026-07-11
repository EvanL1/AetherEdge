//! SunSpec model discovery over Modbus.

use std::time::Duration;

use aether_model::sunspec::DiscoveredModel;
use voltage_modbus::{ModbusRtuClient, ModbusTcpClient, RtuTransport, TcpTransport};

use crate::protocols::adapters::modbus_client::ModbusClientWrapper;
use crate::protocols::adapters::modbus_config::ModbusChannelParamsConfig;
use crate::utils::normalize_protocol_name;

/// SunS magic register values.
pub const SUNS_MAGIC_HI: u16 = 0x5375; // 'Su'
pub const SUNS_MAGIC_LO: u16 = 0x6E53; // 'nS'
pub const MODEL_END_ID: u16 = 0xFFFF;

/// Candidate SunSpec base addresses (0-based Modbus register).
pub const CANDIDATE_BASES: &[u16] = &[0, 40_000, 50_000];

/// Connect a short-lived Modbus client from channel parameters.
pub async fn connect_modbus(
    params: &ModbusChannelParamsConfig,
    protocol: &str,
) -> Result<ModbusClientWrapper, String> {
    let proto = normalize_protocol_name(protocol);

    match proto.as_ref() {
        "modbus_rtu" | "sunspec_rtu" => {
            let device = params
                .device
                .as_deref()
                .ok_or_else(|| "RTU device path required".to_string())?;
            let transport =
                RtuTransport::new(device, params.baud_rate).map_err(|e| e.to_string())?;
            Ok(ModbusClientWrapper::Rtu(ModbusRtuClient::from_transport(
                transport,
            )))
        },
        _ => {
            let addr = params
                .tcp_address()
                .ok_or_else(|| "TCP host required".to_string())?;
            let socket: std::net::SocketAddr = addr
                .parse()
                .map_err(|e: std::net::AddrParseError| e.to_string())?;
            let transport =
                TcpTransport::new(socket, Duration::from_millis(params.connect_timeout_ms))
                    .await
                    .map_err(|e| e.to_string())?;
            Ok(ModbusClientWrapper::Tcp(ModbusTcpClient::from_transport(
                transport,
            )))
        },
    }
}

/// Discover SunSpec models on a device.
///
/// Returns `(base_address, models)`.
pub async fn discover_models(
    client: &mut ModbusClientWrapper,
    slave_id: u8,
    function_code: u8,
    base_address: Option<u16>,
) -> Result<(u16, Vec<DiscoveredModel>), String> {
    let base = match base_address {
        Some(b) => {
            verify_suns(client, slave_id, function_code, b).await?;
            b
        },
        None => detect_base(client, slave_id, function_code).await?,
    };

    let mut models = Vec::new();
    let mut reg = base.saturating_add(2);

    loop {
        let model_id = read_register(client, slave_id, function_code, reg).await?;
        if model_id == MODEL_END_ID {
            break;
        }

        let length = read_register(client, slave_id, function_code, reg + 1).await?;
        models.push(DiscoveredModel {
            model_id,
            length,
            start_register: reg,
        });

        reg = reg.saturating_add(2).saturating_add(length);
        if reg == 0 {
            return Err("SunSpec model chain overflow".to_string());
        }
    }

    if models.is_empty() {
        return Err(format!(
            "No SunSpec models found at base {base} (only end marker?)"
        ));
    }

    Ok((base, models))
}

async fn detect_base(
    client: &mut ModbusClientWrapper,
    slave_id: u8,
    function_code: u8,
) -> Result<u16, String> {
    for &base in CANDIDATE_BASES {
        if verify_suns(client, slave_id, function_code, base)
            .await
            .is_ok()
        {
            return Ok(base);
        }
    }
    Err(format!(
        "SunS signature not found at bases {:?}",
        CANDIDATE_BASES
    ))
}

async fn verify_suns(
    client: &mut ModbusClientWrapper,
    slave_id: u8,
    function_code: u8,
    base: u16,
) -> Result<(), String> {
    let hi = read_register(client, slave_id, function_code, base).await?;
    let lo = read_register(client, slave_id, function_code, base + 1).await?;
    if hi == SUNS_MAGIC_HI && lo == SUNS_MAGIC_LO {
        Ok(())
    } else {
        Err(format!(
            "Invalid SunS at {base}: got {hi:#06x}/{lo:#06x}, expected {SUNS_MAGIC_HI:#06x}/{SUNS_MAGIC_LO:#06x}"
        ))
    }
}

async fn read_register(
    client: &mut ModbusClientWrapper,
    slave_id: u8,
    function_code: u8,
    address: u16,
) -> Result<u16, String> {
    let values = match function_code {
        3 => client.read_03(slave_id, address, 1).await,
        4 => client.read_04(slave_id, address, 1).await,
        fc => {
            return Err(format!(
                "Unsupported function code {fc} for discovery (use 3 or 4)"
            ));
        },
    }
    .map_err(|e| e.to_string())?;

    values
        .first()
        .copied()
        .ok_or_else(|| format!("Empty response at register {address}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_bases_include_common_values() {
        assert!(CANDIDATE_BASES.contains(&0));
        assert!(CANDIDATE_BASES.contains(&40_000));
        assert!(CANDIDATE_BASES.contains(&50_000));
    }
}
