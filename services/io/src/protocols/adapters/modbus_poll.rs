//! Modbus polling and register reading logic.
//!
//! Contains the read path: batch register reading, coil reading,
//! segment building, and value decoding/transformation.

use tracing::debug;
use voltage_modbus::DeviceLimits;

use crate::protocols::core::data::{DataPoint, Value};
use crate::protocols::core::point::PointConfig;

use super::modbus_client::ModbusClientWrapper;
use super::modbus_config::ModbusAddress;

/// A segment of consecutive registers to be read in one batch.
pub(crate) struct RegisterSegment<'a> {
    pub start_address: u16,
    pub end_address: u16,
    pub points: Vec<(u16, u16, &'a PointConfig<ModbusAddress>)>, // (address, count, point)
}

/// Read a group of points with the same slave_id and function_code.
///
/// Uses batch reading optimization: consecutive registers are read in single requests.
pub(crate) async fn read_point_group(
    client: &mut ModbusClientWrapper,
    point_indices: &[usize],
    points: &[PointConfig<ModbusAddress>],
    max_batch_size: u16,
    max_gap: u16,
) -> Vec<(u32, DataPoint)> {
    let Some(first_index) = point_indices.first() else {
        return Vec::new();
    };

    let first = &points[*first_index];
    let slave_id = first.address.slave_id;
    let function_code = first.address.function_code;

    // For coils/discrete inputs (FC01/FC02), read individually
    if function_code == 1 || function_code == 2 {
        return read_coils_individually(client, point_indices, points, slave_id, function_code)
            .await;
    }

    // For registers (FC03/FC04), use batch optimization
    read_registers_batched(
        client,
        point_indices,
        points,
        slave_id,
        function_code,
        max_batch_size,
        max_gap,
    )
    .await
}

/// Read coils or discrete inputs individually (FC01/FC02).
async fn read_coils_individually(
    client: &mut ModbusClientWrapper,
    point_indices: &[usize],
    points: &[PointConfig<ModbusAddress>],
    slave_id: u8,
    function_code: u8,
) -> Vec<(u32, DataPoint)> {
    let mut results = Vec::with_capacity(point_indices.len());
    for index in point_indices {
        let point = &points[*index];
        let modbus_addr = &point.address;
        let value_result = match function_code {
            1 => client
                .read_01(slave_id, modbus_addr.register, 1)
                .await
                .map(|coils| Value::Bool(coils.first().copied().unwrap_or(false))),
            2 => client
                .read_02(slave_id, modbus_addr.register, 1)
                .await
                .map(|inputs| Value::Bool(inputs.first().copied().unwrap_or(false))),
            _ => continue,
        };

        if let Ok(value) = value_result {
            let transformed = apply_transform(value, &point.transform);
            results.push((
                point.id,
                DataPoint::new(point.id, point.point_type, transformed),
            ));
        }
    }

    results
}

/// Read registers in batches (FC03/FC04).
///
/// Groups consecutive registers (within max_gap) and reads them in single requests.
async fn read_registers_batched(
    client: &mut ModbusClientWrapper,
    point_indices: &[usize],
    points: &[PointConfig<ModbusAddress>],
    slave_id: u8,
    function_code: u8,
    max_batch_size: u16,
    max_gap: u16,
) -> Vec<(u32, DataPoint)> {
    let sorted_points: Vec<_> = point_indices
        .iter()
        .map(|index| {
            let point = &points[*index];
            (
                point.address.register,
                point.address.format.register_count(),
                point,
            )
        })
        .collect();

    if sorted_points.is_empty() {
        return Vec::new();
    }

    let segments = build_register_segments(&sorted_points, max_gap, max_batch_size);
    let mut results = Vec::with_capacity(point_indices.len());

    for segment in segments {
        match read_register_segment(client, slave_id, function_code, &segment, max_batch_size).await
        {
            Ok(batch_results) => results.extend(batch_results),
            Err(e) => {
                debug!(
                    "FC{:02} slave {} batch read @{}-{} failed: {}",
                    function_code,
                    slave_id,
                    segment.start_address,
                    segment.end_address.saturating_sub(1),
                    e
                );
            },
        }
    }

    results
}

/// Build segments of consecutive registers for batch reading.
#[allow(clippy::disallowed_methods)]
fn build_register_segments<'a>(
    sorted_points: &[(u16, u16, &'a PointConfig<ModbusAddress>)],
    max_gap: u16,
    max_batch_size: u16,
) -> Vec<RegisterSegment<'a>> {
    let mut segments = Vec::new();
    let mut current_segment: Option<RegisterSegment> = None;

    for &(addr, count, point) in sorted_points {
        match &mut current_segment {
            None => {
                current_segment = Some(RegisterSegment {
                    start_address: addr,
                    end_address: addr + count,
                    points: vec![(addr, count, point)],
                });
            },
            Some(seg) => {
                let gap = addr.saturating_sub(seg.end_address);
                let new_total = (addr + count).saturating_sub(seg.start_address);

                if gap <= max_gap && new_total <= max_batch_size {
                    seg.end_address = addr + count;
                    seg.points.push((addr, count, point));
                } else if let Some(segment) = current_segment.take() {
                    segments.push(segment);
                    current_segment = Some(RegisterSegment {
                        start_address: addr,
                        end_address: addr + count,
                        points: vec![(addr, count, point)],
                    });
                }
            },
        }
    }

    if let Some(seg) = current_segment {
        segments.push(seg);
    }

    segments
}

/// Read a segment of consecutive registers and decode individual points.
#[allow(clippy::needless_lifetimes)]
async fn read_register_segment<'a>(
    client: &mut ModbusClientWrapper,
    slave_id: u8,
    function_code: u8,
    segment: &RegisterSegment<'a>,
    max_batch_size: u16,
) -> std::result::Result<Vec<(u32, DataPoint)>, voltage_modbus::ModbusError> {
    let total_registers = segment.end_address - segment.start_address;
    let limits = DeviceLimits::new().with_max_read_registers(max_batch_size);

    let registers = match function_code {
        3 => {
            client
                .read_03_batch(slave_id, segment.start_address, total_registers, &limits)
                .await?
        },
        4 => {
            client
                .read_04_batch(slave_id, segment.start_address, total_registers, &limits)
                .await?
        },
        _ => return Ok(Vec::new()),
    };

    let mut results = Vec::with_capacity(segment.points.len());

    for &(addr, count, point) in &segment.points {
        let offset = (addr - segment.start_address) as usize;
        let end = offset + count as usize;

        if end <= registers.len() {
            let point_regs = &registers[offset..end];

            let modbus_addr = &point.address;
            if let Ok(value) = decode_registers(
                point_regs,
                modbus_addr.format,
                modbus_addr.byte_order,
                modbus_addr.bit_position,
            ) {
                let transformed = apply_transform(value, &point.transform);
                results.push((
                    point.id,
                    DataPoint::new(point.id, point.point_type, transformed),
                ));
            }
        }
    }

    Ok(results)
}

// ============================================================================
// Value helpers
// ============================================================================

/// Decode Modbus registers to a Value.
fn decode_registers(
    regs: &[u16],
    format: crate::protocols::core::point::DataFormat,
    byte_order: crate::protocols::core::point::ByteOrder,
    bit_position: Option<u8>,
) -> crate::protocols::core::error::Result<Value> {
    use crate::protocols::codec::byte_order::decode_registers as codec_decode;
    codec_decode(regs, format, byte_order, bit_position)
}

/// Apply transform to a value.
fn apply_transform(
    value: Value,
    transform: &crate::protocols::core::point::TransformConfig,
) -> Value {
    match value {
        Value::Float(v) => Value::Float(transform.apply(v)),
        Value::Integer(v) => Value::Float(transform.apply(v as f64)),
        Value::Bool(v) => Value::Bool(transform.apply_bool(v)),
    }
}
