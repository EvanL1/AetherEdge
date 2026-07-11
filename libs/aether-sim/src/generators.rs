//! Waveform generator implementations.
//!
//! This module contains various waveform generators:
//! - `SineWave` - Sinusoidal oscillation
//! - `SquareWave` - Binary high/low alternation
//! - `TriangleWave` - Linear ramp up/down
//! - `RandomDrift` - Smoothed random walk
//! - `DailyPattern` - 24-hour cycle pattern
//! - `ConstantValue` - Fixed value output
//! - `LinearRamp` - Linear increase/decrease
//! - `NoiseGenerator` - Gaussian noise

use crate::WaveformGenerator;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::f64::consts::PI;
use std::sync::Mutex;

// ============================================================================
// Sine Wave Generator
// ============================================================================

/// Sine wave generator: `value = offset + amplitude * sin(2π * frequency * t + phase)`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SineWave {
    /// Oscillation frequency in Hz
    pub frequency: f64,
    /// Peak amplitude (half of peak-to-peak)
    pub amplitude: f64,
    /// DC offset (center value)
    pub offset: f64,
    /// Phase shift in radians
    pub phase: f64,
}

impl SineWave {
    pub fn new(frequency: f64, amplitude: f64, offset: f64, phase: f64) -> Self {
        Self {
            frequency,
            amplitude,
            offset,
            phase,
        }
    }
}

impl WaveformGenerator for SineWave {
    fn generate(&self, timestamp_ms: i64) -> f64 {
        let t = timestamp_ms as f64 / 1000.0; // Convert to seconds
        let angle = 2.0 * PI * self.frequency * t + self.phase;
        self.offset + self.amplitude * angle.sin()
    }

    fn type_name(&self) -> &'static str {
        "SineWave"
    }
}

// ============================================================================
// Square Wave Generator
// ============================================================================

/// Square wave generator: alternates between high and low values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SquareWave {
    /// Oscillation frequency in Hz
    pub frequency: f64,
    /// High value
    pub high: f64,
    /// Low value
    pub low: f64,
    /// Duty cycle (0.0 to 1.0, default 0.5)
    pub duty_cycle: f64,
}

impl SquareWave {
    pub fn new(frequency: f64, high: f64, low: f64) -> Self {
        Self {
            frequency,
            high,
            low,
            duty_cycle: 0.5,
        }
    }

    pub fn with_duty_cycle(mut self, duty_cycle: f64) -> Self {
        self.duty_cycle = duty_cycle.clamp(0.0, 1.0);
        self
    }
}

impl WaveformGenerator for SquareWave {
    fn generate(&self, timestamp_ms: i64) -> f64 {
        let t = timestamp_ms as f64 / 1000.0;
        let period = 1.0 / self.frequency;
        let phase = (t % period) / period;
        if phase < self.duty_cycle {
            self.high
        } else {
            self.low
        }
    }

    fn type_name(&self) -> &'static str {
        "SquareWave"
    }
}

// ============================================================================
// Triangle Wave Generator
// ============================================================================

/// Triangle wave generator: linear ramp up and down.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriangleWave {
    /// Oscillation frequency in Hz
    pub frequency: f64,
    /// Minimum value
    pub min: f64,
    /// Maximum value
    pub max: f64,
}

impl TriangleWave {
    pub fn new(frequency: f64, min: f64, max: f64) -> Self {
        Self {
            frequency,
            min,
            max,
        }
    }
}

impl WaveformGenerator for TriangleWave {
    fn generate(&self, timestamp_ms: i64) -> f64 {
        let t = timestamp_ms as f64 / 1000.0;
        let period = 1.0 / self.frequency;
        let phase = (t % period) / period;

        let normalized = if phase < 0.5 {
            phase * 2.0 // Rising: 0 -> 1
        } else {
            2.0 - phase * 2.0 // Falling: 1 -> 0
        };

        self.min + normalized * (self.max - self.min)
    }

    fn type_name(&self) -> &'static str {
        "TriangleWave"
    }
}

// ============================================================================
// Random Drift Generator
// ============================================================================

/// Random drift generator: smoothed random walk around a center value.
///
/// Uses exponential smoothing to create realistic sensor drift patterns.
#[derive(Debug)]
pub struct RandomDrift {
    /// Center value to drift around
    pub center: f64,
    /// Maximum deviation from center
    pub max_delta: f64,
    /// Smoothing factor (0.0 to 1.0, higher = smoother)
    pub smoothness: f64,
    /// Internal state for smoothed value
    state: Mutex<f64>,
}

impl RandomDrift {
    pub fn new(center: f64, max_delta: f64, smoothness: f64) -> Self {
        Self {
            center,
            max_delta,
            smoothness: smoothness.clamp(0.0, 0.99),
            state: Mutex::new(center),
        }
    }
}

impl WaveformGenerator for RandomDrift {
    fn generate(&self, _timestamp_ms: i64) -> f64 {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let mut rng = rand::thread_rng();

        // Generate random target within bounds
        let target = self.center + rng.gen_range(-self.max_delta..self.max_delta);

        // Apply exponential smoothing
        *state = self.smoothness * *state + (1.0 - self.smoothness) * target;

        // Clamp to bounds
        (*state).clamp(self.center - self.max_delta, self.center + self.max_delta)
    }

    fn type_name(&self) -> &'static str {
        "RandomDrift"
    }
}

// Implement Clone manually due to Mutex
impl Clone for RandomDrift {
    fn clone(&self) -> Self {
        let state = *self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        Self {
            center: self.center,
            max_delta: self.max_delta,
            smoothness: self.smoothness,
            state: Mutex::new(state),
        }
    }
}

// ============================================================================
// Daily Pattern Generator
// ============================================================================

/// Daily pattern generator: simulates 24-hour cycle (e.g., solar power).
///
/// Uses Gaussian curve centered at peak hour to simulate daily patterns
/// like solar irradiance or temperature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyPattern {
    /// Hour of peak value (0-23)
    pub peak_hour: u8,
    /// Value at peak time
    pub peak_value: f64,
    /// Base value (minimum)
    pub base_value: f64,
    /// Spread of the peak (standard deviation in hours)
    pub spread_hours: f64,
}

impl DailyPattern {
    pub fn new(peak_hour: u8, peak_value: f64, base_value: f64) -> Self {
        Self {
            peak_hour: peak_hour.min(23),
            peak_value,
            base_value,
            spread_hours: 4.0, // Default spread
        }
    }

    pub fn with_spread(mut self, spread_hours: f64) -> Self {
        self.spread_hours = spread_hours.max(0.1);
        self
    }
}

impl WaveformGenerator for DailyPattern {
    fn generate(&self, timestamp_ms: i64) -> f64 {
        use chrono::{TimeZone, Timelike, Utc};

        // Use single() to handle LocalResult, fallback to base value if timestamp is invalid
        let Some(datetime) = Utc.timestamp_millis_opt(timestamp_ms).single() else {
            return self.base_value;
        };
        let hour = datetime.hour() as f64 + datetime.minute() as f64 / 60.0;

        // Calculate distance from peak (handle wrap-around)
        let distance = {
            let direct = (hour - self.peak_hour as f64).abs();
            let wrapped = 24.0 - direct;
            direct.min(wrapped)
        };

        // Gaussian curve
        let sigma = self.spread_hours;
        let factor = (-distance.powi(2) / (2.0 * sigma.powi(2))).exp();

        self.base_value + (self.peak_value - self.base_value) * factor
    }

    fn type_name(&self) -> &'static str {
        "DailyPattern"
    }
}

// ============================================================================
// Constant Value Generator
// ============================================================================

/// Constant value generator: always returns the same value.
///
/// Useful for status registers or fixed setpoints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConstantValue {
    pub value: f64,
}

impl ConstantValue {
    pub fn new(value: f64) -> Self {
        Self { value }
    }
}

impl WaveformGenerator for ConstantValue {
    fn generate(&self, _timestamp_ms: i64) -> f64 {
        self.value
    }

    fn type_name(&self) -> &'static str {
        "ConstantValue"
    }
}

// ============================================================================
// Linear Ramp Generator
// ============================================================================

/// Linear ramp generator: value increases/decreases linearly over time.
///
/// Useful for simulating charging/discharging processes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinearRamp {
    /// Starting value
    pub start_value: f64,
    /// Ending value
    pub end_value: f64,
    /// Duration of ramp in milliseconds
    pub duration_ms: i64,
    /// Start timestamp (set when generator is created)
    pub start_time_ms: i64,
    /// Whether to loop the ramp
    pub loop_mode: bool,
}

impl LinearRamp {
    pub fn new(start_value: f64, end_value: f64, duration_ms: i64, start_time_ms: i64) -> Self {
        Self {
            start_value,
            end_value,
            duration_ms,
            start_time_ms,
            loop_mode: false,
        }
    }

    pub fn with_loop(mut self) -> Self {
        self.loop_mode = true;
        self
    }
}

impl WaveformGenerator for LinearRamp {
    fn generate(&self, timestamp_ms: i64) -> f64 {
        let elapsed = timestamp_ms - self.start_time_ms;

        let progress = if self.loop_mode {
            let cycle_position = elapsed % (self.duration_ms * 2);
            if cycle_position < self.duration_ms {
                cycle_position as f64 / self.duration_ms as f64
            } else {
                1.0 - (cycle_position - self.duration_ms) as f64 / self.duration_ms as f64
            }
        } else {
            (elapsed as f64 / self.duration_ms as f64).clamp(0.0, 1.0)
        };

        self.start_value + progress * (self.end_value - self.start_value)
    }

    fn type_name(&self) -> &'static str {
        "LinearRamp"
    }
}

// ============================================================================
// Noise Generator
// ============================================================================

/// Gaussian noise generator: adds random noise to a base value.
#[derive(Debug)]
pub struct NoiseGenerator {
    /// Base (mean) value
    pub mean: f64,
    /// Standard deviation of noise
    pub std_dev: f64,
}

impl NoiseGenerator {
    pub fn new(mean: f64, std_dev: f64) -> Self {
        Self { mean, std_dev }
    }
}

impl WaveformGenerator for NoiseGenerator {
    fn generate(&self, _timestamp_ms: i64) -> f64 {
        let mut rng = rand::thread_rng();
        // Box-Muller transform for Gaussian noise
        let u1: f64 = rng.r#gen();
        let u2: f64 = rng.r#gen();
        let z = (-2.0 * u1.ln()).sqrt() * (2.0 * PI * u2).cos();
        self.mean + z * self.std_dev
    }

    fn type_name(&self) -> &'static str {
        "NoiseGenerator"
    }
}

impl Clone for NoiseGenerator {
    fn clone(&self) -> Self {
        Self {
            mean: self.mean,
            std_dev: self.std_dev,
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sine_wave() {
        let sine = SineWave::new(1.0, 100.0, 500.0, 0.0);

        // At t=0, sin(0) = 0, so value should be offset
        let v0 = sine.generate(0);
        assert!((v0 - 500.0).abs() < 0.01);

        // At t=0.25s (quarter period), sin(π/2) = 1, so value should be offset + amplitude
        let v1 = sine.generate(250);
        assert!((v1 - 600.0).abs() < 0.01);
    }

    #[test]
    fn test_square_wave() {
        let square = SquareWave::new(1.0, 100.0, 0.0);

        // At t=0.25s (25% of period), should be high
        let v1 = square.generate(250);
        assert!((v1 - 100.0).abs() < 0.01);

        // At t=0.75s (75% of period), should be low
        let v2 = square.generate(750);
        assert!((v2 - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_random_drift_bounds() {
        let drift = RandomDrift::new(100.0, 10.0, 0.9);

        for _ in 0..100 {
            let v = drift.generate(0);
            assert!((90.0..=110.0).contains(&v));
        }
    }

    #[test]
    fn test_daily_pattern() {
        let daily = DailyPattern::new(12, 1000.0, 100.0);

        // At noon (12:00), should be near peak
        let noon_ms = 12 * 3600 * 1000;
        let v_noon = daily.generate(noon_ms);
        assert!(v_noon > 900.0);

        // At midnight (00:00), should be near base
        let midnight_ms = 0;
        let v_midnight = daily.generate(midnight_ms);
        assert!(v_midnight < 200.0);
    }

    #[test]
    fn test_linear_ramp() {
        let ramp = LinearRamp::new(0.0, 100.0, 1000, 0);

        // At start
        assert!((ramp.generate(0) - 0.0).abs() < 0.01);

        // At midpoint
        assert!((ramp.generate(500) - 50.0).abs() < 0.01);

        // At end
        assert!((ramp.generate(1000) - 100.0).abs() < 0.01);

        // Past end (clamped)
        assert!((ramp.generate(2000) - 100.0).abs() < 0.01);
    }
}
