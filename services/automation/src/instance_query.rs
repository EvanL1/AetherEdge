//! Internal read models for commissioned instances.
//!
//! These types describe Automation query results without binding the
//! repository or runtime snapshot to HTTP, Serde, or OpenAPI.

use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceDataPlane {
    Measurement,
    Action,
}

#[derive(Debug)]
pub struct InstanceLiveSample {
    pub value: f64,
    pub timestamp_ms: u64,
}

#[derive(Debug)]
pub enum InstanceLiveDataView {
    Values(BTreeMap<u32, InstanceLiveSample>),
    Complete {
        measurements: BTreeMap<u32, InstanceLiveSample>,
        actions: BTreeMap<u32, InstanceLiveSample>,
    },
}

#[derive(Debug)]
pub struct PointRoutingView {
    pub channel_id: Option<i32>,
    pub channel_type: Option<String>,
    pub channel_point_id: Option<u32>,
    pub enabled: bool,
    pub channel_name: Option<String>,
    pub channel_point_name: Option<String>,
}

#[derive(Debug)]
pub struct InstanceMeasurementPointView {
    pub measurement_id: u32,
    pub name: String,
    pub unit: Option<String>,
    pub description: Option<String>,
    pub routing: Option<PointRoutingView>,
}

#[derive(Debug)]
pub struct InstanceActionPointView {
    pub action_id: u32,
    pub name: String,
    pub unit: Option<String>,
    pub description: Option<String>,
    pub routing: Option<PointRoutingView>,
}

#[derive(Debug)]
pub struct InstancePropertyView {
    pub property_id: i32,
    pub name: String,
    pub unit: Option<String>,
    pub description: Option<String>,
    pub value: Option<serde_json::Value>,
}

#[derive(Debug)]
pub struct InstancePointsView {
    pub instance_name: String,
    pub measurements: Vec<InstanceMeasurementPointView>,
    pub actions: Vec<InstanceActionPointView>,
    pub properties: Vec<InstancePropertyView>,
    pub logical_routing_revision: u64,
}
