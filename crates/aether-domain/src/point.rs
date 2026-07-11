//! Addressing and samples for live IoT points.

use crate::{InstanceId, PointId, TimestampMs};

/// Semantic role of a point in the device model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PointKind {
    /// Continuously sampled telemetry.
    Telemetry,
    /// Discrete or enumerated device status.
    Status,
    /// Requested command value before device dispatch.
    Command,
    /// Action value routed to a device actuator.
    Action,
}

impl PointKind {
    /// Returns whether callers may target this point with a control command.
    #[must_use]
    pub const fn is_writable(self) -> bool {
        matches!(self, Self::Command | Self::Action)
    }
}

/// Stable address of a point in an instance model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PointAddress {
    instance_id: InstanceId,
    kind: PointKind,
    point_id: PointId,
}

impl PointAddress {
    /// Creates a point address.
    #[must_use]
    pub const fn new(instance_id: InstanceId, kind: PointKind, point_id: PointId) -> Self {
        Self {
            instance_id,
            kind,
            point_id,
        }
    }

    /// Returns the owning instance identifier.
    #[must_use]
    pub const fn instance_id(self) -> InstanceId {
        self.instance_id
    }

    /// Returns the point role.
    #[must_use]
    pub const fn kind(self) -> PointKind {
        self.kind
    }

    /// Returns the point identifier within its instance.
    #[must_use]
    pub const fn point_id(self) -> PointId {
        self.point_id
    }
}

/// Quality attached to a sampled point value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointQuality {
    /// Value is valid for normal use.
    Good,
    /// Value is usable but may be stale or degraded.
    Uncertain,
    /// Value is known to be invalid.
    Bad,
    /// No current value is available.
    Unavailable,
}

/// One timestamped value read from or written to the live data plane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PointSample {
    address: PointAddress,
    value: f64,
    timestamp: TimestampMs,
    quality: PointQuality,
}

impl PointSample {
    /// Creates a point sample.
    #[must_use]
    pub const fn new(
        address: PointAddress,
        value: f64,
        timestamp: TimestampMs,
        quality: PointQuality,
    ) -> Self {
        Self {
            address,
            value,
            timestamp,
            quality,
        }
    }

    /// Returns the sampled point address.
    #[must_use]
    pub const fn address(self) -> PointAddress {
        self.address
    }

    /// Returns the numeric point value.
    #[must_use]
    pub const fn value(self) -> f64 {
        self.value
    }

    /// Returns the sample timestamp.
    #[must_use]
    pub const fn timestamp(self) -> TimestampMs {
        self.timestamp
    }

    /// Returns the sample quality.
    #[must_use]
    pub const fn quality(self) -> PointQuality {
        self.quality
    }
}
