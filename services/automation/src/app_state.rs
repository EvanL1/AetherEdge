//! Application State Management
//!
//! Central application state that is shared across all API handlers

use std::sync::Arc;

use aether_application::{
    ActionRoutingApplication, ControlApplication, MeasurementRoutingApplication,
};

use crate::config::AutomationConfig;
use crate::infra::application_control::ControlAuthenticator;
use crate::instance_configuration::InstanceConfigurationApplication;
use crate::instance_manager::InstanceManager;
use aether_shm_bridge::ShmDeviceCommandSink;

/// Application state containing shared resources
pub struct AppState {
    /// Configuration loaded from database
    pub config: Arc<AutomationConfig>,

    /// Instance lifecycle manager backed by SQLite configuration and SHM live state.
    pub instance_manager: Arc<InstanceManager>,

    /// Shared authenticated and audited device-control use case.
    pub control_application: Arc<ControlApplication>,

    /// Shared authenticated and audited physical action-routing use case.
    pub action_routing_application: Arc<ActionRoutingApplication>,

    /// Shared authenticated, audited, revision-fenced measurement-routing use case.
    pub measurement_routing_application: Arc<MeasurementRoutingApplication>,

    /// Shared authenticated, audited, revision-fenced instance desired-state use case.
    pub instance_configuration_application: Arc<InstanceConfigurationApplication>,

    /// Verifies JWT and service credentials before constructing command actors.
    pub control_authenticator: Arc<ControlAuthenticator>,

    /// Typed C/A command sink (concrete type for delayed configuration in main.rs).
    pub shm_dispatch: Arc<ShmDeviceCommandSink>,
}

impl AppState {
    /// Create new application state
    pub fn new(
        config: Arc<AutomationConfig>,
        instance_manager: Arc<InstanceManager>,
        control_application: Arc<ControlApplication>,
        action_routing_application: Arc<ActionRoutingApplication>,
        measurement_routing_application: Arc<MeasurementRoutingApplication>,
        instance_configuration_application: Arc<InstanceConfigurationApplication>,
        control_authenticator: Arc<ControlAuthenticator>,
        shm_dispatch: Arc<ShmDeviceCommandSink>,
    ) -> Self {
        Self {
            config,
            instance_manager,
            control_application,
            action_routing_application,
            measurement_routing_application,
            instance_configuration_application,
            control_authenticator,
            shm_dispatch,
        }
    }
}
