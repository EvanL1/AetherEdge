//! Waveform generation library for AetherEMS simulation.
//!
//! This library provides various waveform generators for simulating
//! industrial device data patterns. Used by the standalone simulator
//! to generate realistic Modbus register values.
//!
//! # Example
//!
//! ```rust
//! use aether_sim::{WaveformGenerator, generators::SineWave};
//!
//! let sine = SineWave::new(0.1, 100.0, 500.0, 0.0);
//! let value = sine.generate(1000);
//! ```

pub mod generators;

use std::sync::Arc;

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

    /// Get the generator type name for debugging.
    fn type_name(&self) -> &'static str;
}

/// Boxed generator for dynamic dispatch.
pub type BoxedGenerator = Box<dyn WaveformGenerator>;

/// Arc-wrapped generator for shared ownership.
pub type SharedGenerator = Arc<dyn WaveformGenerator>;

// Re-export commonly used generators
pub use generators::{
    ConstantValue, DailyPattern, LinearRamp, NoiseGenerator, RandomDrift, SineWave, SquareWave,
    TriangleWave,
};
