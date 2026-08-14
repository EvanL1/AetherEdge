//! Modbus polling and register reading logic.
//!
//! Contains the read path: batch register reading, coil reading,
//! segment building, and value decoding/transformation.

use tracing::debug;
use voltage_modbus::DeviceLimits;

use crate::protocols::core::data::{DataPoint, Value};
use crate::protocols::core::point::PointConfig;
use crate::protocols::core::traits::PointFailure;

use super::modbus_client::ModbusClientWrapper;
use super::modbus_config::ModbusAddress;

/// A segment of consecutive registers to be read in one batch.
pub(crate) struct RegisterSegment<'a> {
    pub start_address: u16,
    pub end_address: u16,
    pub points: Vec<(u16, u16, &'a PointConfig<ModbusAddress>)>, // (address, count, point)
}

/// Exact result for one Modbus read group. Transport/decode failures remain
/// attached to their point identities instead of being inferred from an empty
/// aggregate after other segments may already have succeeded.
pub(crate) struct PointGroupRead {
    pub points: Vec<(u32, DataPoint)>,
    pub failures: Vec<PointFailure>,
}

impl PointGroupRead {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            points: Vec::with_capacity(capacity),
            failures: Vec::new(),
        }
    }
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
) -> PointGroupRead {
    let Some(first_index) = point_indices.first() else {
        return PointGroupRead::with_capacity(0);
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
) -> PointGroupRead {
    let mut result = PointGroupRead::with_capacity(point_indices.len());
    for index in point_indices {
        let point = &points[*index];
        let modbus_addr = &point.address;
        let value_result = match function_code {
            1 => client
                .read_01(slave_id, modbus_addr.register, 1)
                .await
                .map(|coils| coils.first().copied().map(Value::Bool)),
            2 => client
                .read_02(slave_id, modbus_addr.register, 1)
                .await
                .map(|inputs| inputs.first().copied().map(Value::Bool)),
            _ => continue,
        };

        match value_result {
            Ok(Some(value)) => {
                let transformed = apply_transform(value, &point.transform);
                result.points.push((
                    point.id,
                    DataPoint::new(point.id, point.point_type, transformed),
                ));
            },
            Ok(None) => result.failures.push(PointFailure::typed(
                point.id,
                point.point_type,
                "Modbus returned an empty bit response",
            )),
            Err(error) => result.failures.push(PointFailure::typed_with_error(
                point.id,
                point.point_type,
                format!("Modbus FC{function_code:02} read failed: {error}"),
            )),
        }
    }

    result
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
) -> PointGroupRead {
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
        return PointGroupRead::with_capacity(0);
    }

    let segments = build_register_segments(&sorted_points, max_gap, max_batch_size);
    let mut result = PointGroupRead::with_capacity(point_indices.len());

    for segment in segments {
        match read_register_segment(client, slave_id, function_code, &segment, max_batch_size).await
        {
            Ok(segment_result) => {
                result.points.extend(segment_result.points);
                result.failures.extend(segment_result.failures);
            },
            Err(e) => {
                debug!(
                    "FC{:02} slave {} batch read @{}-{} failed: {}",
                    function_code,
                    slave_id,
                    segment.start_address,
                    segment.end_address.saturating_sub(1),
                    e
                );
                append_segment_failures(
                    &segment,
                    format!("Modbus FC{function_code:02} segment read failed: {e}"),
                    &mut result.failures,
                );
            },
        }
    }

    result
}

fn append_segment_failures(
    segment: &RegisterSegment<'_>,
    error: String,
    failures: &mut Vec<PointFailure>,
) {
    failures.extend(segment.points.iter().map(|(_, _, point)| {
        PointFailure::typed_with_error(point.id, point.point_type, error.clone())
    }));
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
) -> std::result::Result<PointGroupRead, voltage_modbus::ModbusError> {
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
        _ => return Ok(PointGroupRead::with_capacity(0)),
    };

    let mut result = PointGroupRead::with_capacity(segment.points.len());

    for &(addr, count, point) in &segment.points {
        let offset = (addr - segment.start_address) as usize;
        let end = offset + count as usize;

        if end > registers.len() {
            result.failures.push(PointFailure::typed_with_error(
                point.id,
                point.point_type,
                format!(
                    "Modbus response too short: need register offset {end}, received {}",
                    registers.len()
                ),
            ));
            continue;
        }

        let point_regs = &registers[offset..end];
        let modbus_addr = &point.address;
        match decode_registers(
            point_regs,
            modbus_addr.format,
            modbus_addr.byte_order,
            modbus_addr.bit_position,
        ) {
            Ok(value) => {
                let transformed = apply_transform(value, &point.transform);
                result.points.push((
                    point.id,
                    DataPoint::new(point.id, point.point_type, transformed),
                ));
            },
            Err(error) => result.failures.push(PointFailure::typed_with_error(
                point.id,
                point.point_type,
                format!("Modbus register decode failed: {error}"),
            )),
        }
    }

    Ok(result)
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use aether_core::PointType;
    use voltage_modbus::{ModbusTcpClient, TcpTransport};

    use super::*;
    use crate::protocols::adapters::modbus_mock::{MockModbusServer, ModbusException};
    use crate::protocols::core::point::DataFormat;

    #[test]
    fn segmented_failure_keeps_successful_points_and_identifies_failed_segment() {
        let points = vec![
            PointConfig::telemetry(
                10,
                ModbusAddress::holding_register(1, 0, DataFormat::UInt16),
            ),
            PointConfig::telemetry(
                20,
                ModbusAddress::holding_register(1, 100, DataFormat::UInt16),
            ),
            PointConfig::signal(
                21,
                ModbusAddress::holding_register(1, 101, DataFormat::UInt16),
            ),
        ];
        let sorted = points
            .iter()
            .map(|point| {
                (
                    point.address.register,
                    point.address.register_count(),
                    point,
                )
            })
            .collect::<Vec<_>>();
        let segments = build_register_segments(&sorted, 1, 64);
        assert_eq!(segments.len(), 2);

        let mut result = PointGroupRead::with_capacity(points.len());
        result.points.push((
            points[0].id,
            DataPoint::new(points[0].id, points[0].point_type, Value::Integer(7)),
        ));
        append_segment_failures(
            &segments[1],
            "injected second-segment exception".to_string(),
            &mut result.failures,
        );

        assert_eq!(result.points.len(), 1);
        assert_eq!(result.points[0].0, 10);
        assert_eq!(result.failures.len(), 2);
        assert_eq!(result.failures[0].point_id, 20);
        assert_eq!(result.failures[0].point_type, Some(PointType::Telemetry));
        assert_eq!(result.failures[1].point_id, 21);
        assert_eq!(result.failures[1].point_type, Some(PointType::Signal));
        assert!(
            result
                .failures
                .iter()
                .all(|failure| failure.error.contains("second-segment"))
        );
    }

    #[tokio::test]
    async fn live_segment_exception_preserves_other_segment_results() {
        let server = MockModbusServer::start_on_random_port()
            .await
            .expect("start Modbus fixture");
        server.set_register(0, 7);
        server.set_register(100, 9);
        server.inject_error_at_address(3, 100, ModbusException::IllegalDataAddress, 1);
        let transport = TcpTransport::new(
            server.address().parse().expect("fixture socket address"),
            Duration::from_secs(1),
        )
        .await
        .expect("connect Modbus fixture");
        let mut client = ModbusClientWrapper::tcp(
            ModbusTcpClient::from_transport(transport),
            Duration::from_secs(1),
            Duration::from_secs(1),
        );
        let points = vec![
            PointConfig::telemetry(
                10,
                ModbusAddress::holding_register(1, 0, DataFormat::UInt16),
            ),
            PointConfig::telemetry(
                20,
                ModbusAddress::holding_register(1, 100, DataFormat::UInt16),
            ),
        ];

        let result = read_point_group(&mut client, &[0, 1], &points, 64, 0).await;

        assert_eq!(result.points.len(), 1);
        assert_eq!(result.points[0].0, 10);
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].point_id, 20);
        assert_eq!(result.failures[0].point_type, Some(PointType::Telemetry));
        assert!(result.failures[0].error.contains("segment read failed"));
        assert_eq!(server.request_count(), 2);
    }
}
