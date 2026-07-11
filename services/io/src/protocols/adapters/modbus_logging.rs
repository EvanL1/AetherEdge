//! Modbus raw packet logging bridge.
//!
//! Bridges `voltage_modbus` real wire packets to io's `LogContext` system
//! via the `PacketCallback` mechanism.

use std::sync::Arc;

use voltage_modbus::{
    PacketCallback as VoltagePacketCallback, PacketDirection as VoltagePacketDirection,
};

use crate::protocols::core::logging::{
    LogContext, ModbusTransportType, PacketDirection, PacketMetadata,
};

/// Extract metadata from Modbus TCP ADU.
///
/// TCP ADU format: [Trans(2)][Proto(2)][Len(2)][Unit(1)][FC(1)][StartAddr(2)][Qty(2)]...
fn extract_tcp_metadata(data: &[u8]) -> (u8, u8, Option<u16>, Option<u16>, Option<u16>) {
    if data.len() >= 8 {
        let transaction_id = u16::from_be_bytes([data[0], data[1]]);
        let slave_id = data[6];
        let function_code = data[7];

        let (start_address, quantity) = if data.len() >= 12 {
            let start = u16::from_be_bytes([data[8], data[9]]);
            let qty = u16::from_be_bytes([data[10], data[11]]);
            (Some(start), Some(qty))
        } else {
            (None, None)
        };

        (
            slave_id,
            function_code,
            Some(transaction_id),
            start_address,
            quantity,
        )
    } else {
        (0, 0, None, None, None)
    }
}

/// Extract metadata from Modbus RTU ADU.
///
/// RTU ADU format: [Unit(1)][FC(1)][StartAddr(2)][Qty(2)]...[CRC(2)]
#[cfg(feature = "modbus")]
fn extract_rtu_metadata(data: &[u8]) -> (u8, u8, Option<u16>, Option<u16>) {
    if data.len() >= 2 {
        let slave_id = data[0];
        let function_code = data[1];

        let (start_address, quantity) = if data.len() >= 6 {
            let start = u16::from_be_bytes([data[2], data[3]]);
            let qty = u16::from_be_bytes([data[4], data[5]]);
            (Some(start), Some(qty))
        } else {
            (None, None)
        };

        (slave_id, function_code, start_address, quantity)
    } else {
        (0, 0, None, None)
    }
}

/// Create a PacketCallback that bridges voltage_modbus real packets to io's LogContext.
///
/// This callback receives the **actual bytes** sent/received on the wire,
/// not reconstructed packets. This ensures accurate logging including correct TID.
pub(crate) fn create_packet_callback(
    log_context: Arc<LogContext>,
    transport_type: ModbusTransportType,
    group_id: Arc<std::sync::atomic::AtomicU32>,
) -> VoltagePacketCallback {
    Arc::new(move |direction, data| {
        let dir = match direction {
            VoltagePacketDirection::Send => PacketDirection::Send,
            VoltagePacketDirection::Receive => PacketDirection::Receive,
        };

        let (slave_id, function_code, transaction_id, start_address, quantity) =
            match transport_type {
                ModbusTransportType::Tcp => extract_tcp_metadata(data),
                #[cfg(feature = "modbus")]
                ModbusTransportType::Rtu => {
                    let (slave, fc, start, qty) = extract_rtu_metadata(data);
                    (slave, fc, None, start, qty)
                },
                #[allow(unreachable_patterns)]
                _ => (0, 0, None, None, None),
            };

        let metadata = PacketMetadata::Modbus {
            transport: transport_type,
            slave_id,
            function_code,
            transaction_id,
            start_address,
            quantity,
        };

        let current_gid = group_id.load(std::sync::atomic::Ordering::Relaxed);
        let gid = if current_gid > 0 {
            Some(current_gid)
        } else {
            None
        };

        let ctx = log_context.clone();
        let data = data.to_vec();
        tokio::spawn(async move {
            ctx.log_raw_packet(dir, data, metadata, gid).await;
        });
    })
}
