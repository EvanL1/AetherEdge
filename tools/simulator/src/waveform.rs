//! Waveform generation for the protocol simulator.
//!
//! Produces the time-varying values a simulated device reports, so a scenario
//! can look like a real sensor instead of a constant. Simulation never enters
//! the kernel runtime, so this stays inside the simulator binary.
//!
//! # Example
//!
//! ```ignore
//! use crate::waveform::{WaveformGenerator, generators::SineWave};
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
