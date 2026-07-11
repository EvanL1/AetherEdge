//! Channel-Instance Point Routing Data Types
//!
//! This module provides routing data types used by API handlers and instance routing:
//! - `MeasurementRoutingRow`, `ActionRoutingRow` - CSV/API request structures
//! - `MeasurementRouting`, `ActionRouting` - Database records (sqlx::FromRow)

use common::FourRemote;
use serde::{Deserialize, Serialize};

/// CSV row structure for measurement routing (T/S → M)
///
/// `channel_id`, `channel_type`, and `channel_point_id` form a unit - all None means unbound
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeasurementRoutingRow {
    pub channel_id: Option<i32>,
    pub channel_type: Option<FourRemote>, // T or S only, None if unbound
    pub channel_point_id: Option<u32>,
    pub measurement_id: u32,
}

/// CSV row structure for action routing (A → C/A)
///
/// `channel_id`, `channel_type`, and `channel_point_id` form a unit - all None means unbound
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionRoutingRow {
    pub action_id: u32,
    pub channel_id: Option<i32>,
    pub channel_type: Option<FourRemote>, // C or A only, None if unbound
    pub channel_point_id: Option<u32>,
}

/// Measurement routing record from database
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct MeasurementRouting {
    pub routing_id: i32,
    pub instance_id: u16,
    pub instance_name: String,
    pub channel_id: Option<i32>,
    pub channel_type: Option<String>,
    pub channel_point_id: Option<u32>,
    pub measurement_id: u32,
    pub description: Option<String>,
    pub enabled: bool,
}

/// Action routing record from database
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ActionRouting {
    pub routing_id: i32,
    pub instance_id: u16,
    pub instance_name: String,
    pub action_id: u32,
    pub channel_id: Option<i32>,
    pub channel_type: Option<String>,
    pub channel_point_id: Option<u32>,
    pub description: Option<String>,
    pub enabled: bool,
}
