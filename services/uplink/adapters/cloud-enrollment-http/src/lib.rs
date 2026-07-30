//! Strict, bounded HTTP Claim adapter for AetherCloud Gateway enrollment.

mod client;
mod config;
mod wire;

pub use client::HttpCloudEnrollmentClient;
pub use config::{HttpCloudEnrollmentConfig, MAX_CLAIM_RESPONSE_BYTES};
