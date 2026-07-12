//! Addressing and samples for live IoT points.

use crate::{ChannelId, DomainError, InstanceId, PointId, TimestampMs};

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

    /// Returns whether samples of this kind originate in acquisition.
    #[must_use]
    pub const fn is_acquisition_owned(self) -> bool {
        matches!(self, Self::Telemetry | Self::Status)
    }
}

/// Physical address reported by one acquisition channel.
///
/// This type is intentionally distinct from [`PointAddress`]. A channel point
/// identifies data-plane input, while a point address identifies the logical
/// instance projection exposed to application queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChannelPointAddress {
    channel_id: ChannelId,
    kind: PointKind,
    point_id: PointId,
}

/// Physical address owned by the device-command path.
///
/// This type cannot represent telemetry or status slots, so a command sink
/// cannot accidentally acquire the acquisition writer's T/S authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChannelCommandAddress {
    channel_id: ChannelId,
    kind: PointKind,
    point_id: PointId,
}

impl ChannelCommandAddress {
    /// Creates a command-owned control or action address.
    pub const fn new(
        channel_id: ChannelId,
        kind: PointKind,
        point_id: PointId,
    ) -> Result<Self, DomainError> {
        if !kind.is_writable() {
            return Err(DomainError::PointNotCommandOwned(kind));
        }
        Ok(Self {
            channel_id,
            kind,
            point_id,
        })
    }

    /// Returns the physical channel identifier.
    #[must_use]
    pub const fn channel_id(self) -> ChannelId {
        self.channel_id
    }

    /// Returns the command-owned point kind.
    #[must_use]
    pub const fn kind(self) -> PointKind {
        self.kind
    }

    /// Returns the point identifier within the channel.
    #[must_use]
    pub const fn point_id(self) -> PointId {
        self.point_id
    }
}

impl ChannelPointAddress {
    /// Creates an acquisition-owned telemetry or status address.
    pub const fn new(
        channel_id: ChannelId,
        kind: PointKind,
        point_id: PointId,
    ) -> Result<Self, DomainError> {
        if !kind.is_acquisition_owned() {
            return Err(DomainError::PointNotAcquisitionOwned(kind));
        }
        Ok(Self {
            channel_id,
            kind,
            point_id,
        })
    }

    /// Returns the physical channel identifier.
    #[must_use]
    pub const fn channel_id(self) -> ChannelId {
        self.channel_id
    }

    /// Returns the acquisition-owned point kind.
    #[must_use]
    pub const fn kind(self) -> PointKind {
        self.kind
    }

    /// Returns the point identifier within the channel.
    #[must_use]
    pub const fn point_id(self) -> PointId {
        self.point_id
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

/// One validated sample emitted by a physical acquisition channel.
///
/// SHM may use an internal NaN sentinel for an unwritten slot, but an acquired
/// sample is always finite. Missing or unusable data is represented by absence
/// or [`PointQuality`], never by forging a numeric value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AcquiredPointSample {
    address: ChannelPointAddress,
    value: f64,
    raw: f64,
    timestamp: TimestampMs,
    quality: PointQuality,
}

impl AcquiredPointSample {
    /// Creates a finite physical-channel sample.
    pub fn new(
        address: ChannelPointAddress,
        value: f64,
        raw: f64,
        timestamp: TimestampMs,
        quality: PointQuality,
    ) -> Result<Self, DomainError> {
        if !value.is_finite() {
            return Err(DomainError::NonFiniteAcquiredValue);
        }
        if !raw.is_finite() {
            return Err(DomainError::NonFiniteAcquiredRawValue);
        }
        Ok(Self {
            address,
            value,
            raw,
            timestamp,
            quality,
        })
    }

    /// Returns the physical address.
    #[must_use]
    pub const fn address(self) -> ChannelPointAddress {
        self.address
    }

    /// Returns the engineering-unit value.
    #[must_use]
    pub const fn value(self) -> f64 {
        self.value
    }

    /// Returns the unscaled source value.
    #[must_use]
    pub const fn raw(self) -> f64 {
        self.raw
    }

    /// Returns the device/source timestamp.
    #[must_use]
    pub const fn timestamp(self) -> TimestampMs {
        self.timestamp
    }

    /// Returns the source quality.
    #[must_use]
    pub const fn quality(self) -> PointQuality {
        self.quality
    }
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
