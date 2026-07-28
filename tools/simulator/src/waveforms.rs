//! Waveform generation for the standalone AetherEdge simulator.
//!
//! These generators simulate
//! industrial device data patterns. Used by the standalone simulator
//! to generate realistic Modbus register values.
//!
//! # Example
//!
//! ```rust
//! use crate::waveforms::{WaveformGenerator, SineWave};
//!
//! let sine = SineWave::new(0.1, 100.0, 500.0, 0.0);
//! let value = sine.generate(1000);
//! ```

pub mod generators;

/// Core trait for waveform generation.
///
/// All generators implement this trait to produce time-varying values.
/// The timestamp is in milliseconds since Unix epoch.
pub trait WaveformGenerator: Send + Sync {
    /// Generate a value for the given timestamp.
    ///
    /// # Arguments
    /// * `timestamp_ms` - Unix timestamp in milliseconds
    ///
    /// # Returns
    /// The generated value as f64
    fn generate(&self, timestamp_ms: i64) -> f64;
}

/// Boxed generator for dynamic dispatch.
pub type BoxedGenerator = Box<dyn WaveformGenerator>;

// Re-export commonly used generators
pub use generators::{
    ConstantValue, DailyPattern, LinearRamp, NoiseGenerator, RandomDrift, SineWave, SquareWave,
    TriangleWave,
};
