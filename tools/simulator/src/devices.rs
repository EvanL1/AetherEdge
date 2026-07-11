//! Device register map management.

use crate::scenarios::{DeviceConfig, GeneratorConfig};
use aether_sim::{
    BoxedGenerator, ConstantValue, DailyPattern, LinearRamp, NoiseGenerator, RandomDrift, SineWave,
    SquareWave, TriangleWave, WaveformGenerator,
};
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;

/// Device register map: unit_id -> (address -> generator)
pub type DeviceMap = HashMap<u8, RegisterMap>;

/// Register map: address -> generator
pub type RegisterMap = HashMap<u16, Arc<dyn WaveformGenerator>>;

/// Build device map from configuration.
pub fn build_device_map(devices: &[DeviceConfig]) -> Result<DeviceMap> {
    let mut device_map = DeviceMap::new();

    for device in devices {
        let mut register_map = RegisterMap::new();

        for reg in &device.registers {
            let generator = create_generator(&reg.generator)?;
            register_map.insert(reg.address, Arc::from(generator));
        }

        device_map.insert(device.unit_id, register_map);
    }

    Ok(device_map)
}

/// Create a waveform generator from configuration.
fn create_generator(config: &GeneratorConfig) -> Result<BoxedGenerator> {
    let generator: BoxedGenerator = match config {
        GeneratorConfig::Constant { value } => Box::new(ConstantValue::new(*value)),

        GeneratorConfig::Sine {
            frequency,
            amplitude,
            offset,
            phase,
        } => Box::new(SineWave::new(*frequency, *amplitude, *offset, *phase)),

        GeneratorConfig::Square {
            frequency,
            high,
            low,
            duty_cycle,
        } => Box::new(SquareWave::new(*frequency, *high, *low).with_duty_cycle(*duty_cycle)),

        GeneratorConfig::Triangle {
            frequency,
            min,
            max,
        } => Box::new(TriangleWave::new(*frequency, *min, *max)),

        GeneratorConfig::RandomDrift {
            center,
            max_delta,
            smoothness,
        } => Box::new(RandomDrift::new(*center, *max_delta, *smoothness)),

        GeneratorConfig::DailyPattern {
            peak_hour,
            peak_value,
            base_value,
            spread_hours,
        } => Box::new(
            DailyPattern::new(*peak_hour, *peak_value, *base_value).with_spread(*spread_hours),
        ),

        GeneratorConfig::Noise { mean, std_dev } => Box::new(NoiseGenerator::new(*mean, *std_dev)),

        GeneratorConfig::LinearRamp {
            start,
            end,
            duration_sec,
            loop_mode,
        } => {
            // Use current time as start time
            let start_time_ms = chrono::Utc::now().timestamp_millis();
            let duration_ms = (*duration_sec as i64) * 1000;
            let mut ramp = LinearRamp::new(*start, *end, duration_ms, start_time_ms);
            if *loop_mode {
                ramp = ramp.with_loop();
            }
            Box::new(ramp)
        },
    };

    Ok(generator)
}

/// Generate register values for a device at the given timestamp.
pub fn generate_registers(
    register_map: &RegisterMap,
    start_addr: u16,
    count: u16,
    timestamp_ms: i64,
) -> Vec<u16> {
    let mut values = Vec::with_capacity(count as usize);

    for offset in 0..count {
        let addr = start_addr.wrapping_add(offset);
        let value = if let Some(generator) = register_map.get(&addr) {
            // Generate f64 value and convert to u16
            let f_value = generator.generate(timestamp_ms);
            // Clamp to u16 range and convert
            f_value.clamp(0.0, 65535.0) as u16
        } else {
            // Return 0 for unmapped registers
            0
        };
        values.push(value);
    }

    values
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    #[test]
    fn test_create_constant_generator() {
        let config = GeneratorConfig::Constant { value: 100.0 };
        let generator = create_generator(&config).unwrap();
        assert_eq!(generator.generate(0), 100.0);
    }

    #[test]
    fn test_generate_registers() {
        let mut register_map = RegisterMap::new();
        register_map.insert(0, Arc::new(ConstantValue::new(100.0)));
        register_map.insert(1, Arc::new(ConstantValue::new(200.0)));
        register_map.insert(2, Arc::new(ConstantValue::new(300.0)));

        let values = generate_registers(&register_map, 0, 3, 0);
        assert_eq!(values, vec![100, 200, 300]);
    }

    #[test]
    fn test_unmapped_registers_return_zero() {
        let register_map = RegisterMap::new();
        let values = generate_registers(&register_map, 0, 3, 0);
        assert_eq!(values, vec![0, 0, 0]);
    }
}
