//! Industry-neutral alarm policy values.

use alloc::string::String;

use crate::{ChannelId, DomainError, PointId};

/// Supported comparison applied to one live value and a configured threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlarmComparator {
    /// Current value is greater than the threshold.
    GreaterThan,
    /// Current value is less than the threshold.
    LessThan,
    /// Current value is greater than or equal to the threshold.
    GreaterThanOrEqual,
    /// Current value is less than or equal to the threshold.
    LessThanOrEqual,
    /// Current value is equal to the threshold.
    Equal,
    /// Current value is not equal to the threshold.
    NotEqual,
}

impl AlarmComparator {
    /// Returns the stable operator representation used by configuration and storage.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GreaterThan => ">",
            Self::LessThan => "<",
            Self::GreaterThanOrEqual => ">=",
            Self::LessThanOrEqual => "<=",
            Self::Equal => "==",
            Self::NotEqual => "!=",
        }
    }
}

impl TryFrom<&str> for AlarmComparator {
    type Error = DomainError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            ">" => Ok(Self::GreaterThan),
            "<" => Ok(Self::LessThan),
            ">=" => Ok(Self::GreaterThanOrEqual),
            "<=" => Ok(Self::LessThanOrEqual),
            "==" => Ok(Self::Equal),
            "!=" => Ok(Self::NotEqual),
            _ => Err(DomainError::InvalidAlarmComparator),
        }
    }
}

/// Alarm importance in the stable range 1 (low) through 3 (high).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct AlarmSeverity(u8);

impl AlarmSeverity {
    /// Validates and creates an alarm severity.
    pub const fn new(value: i64) -> Result<Self, DomainError> {
        if value < 1 || value > 3 {
            return Err(DomainError::InvalidAlarmSeverity);
        }
        Ok(Self(value as u8))
    }

    /// Returns the stable numeric representation.
    #[must_use]
    pub const fn get(self) -> i64 {
        self.0 as i64
    }
}

/// Live-state selector monitored by an alarm rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlarmRuleTarget {
    /// One point owned by an application service/channel namespace.
    Point {
        /// Owning service namespace.
        service_type: String,
        /// Physical channel identity.
        channel_id: ChannelId,
        /// Point-kind namespace used by the commissioned model.
        data_type: String,
        /// Point identity within the namespace.
        point_id: PointId,
    },
    /// Built-in channel connectivity state.
    ChannelOnline {
        /// Physical channel identity.
        channel_id: ChannelId,
    },
}

impl AlarmRuleTarget {
    /// Creates a point alarm selector.
    pub fn point(
        service_type: impl Into<String>,
        channel_id: ChannelId,
        data_type: impl Into<String>,
        point_id: PointId,
    ) -> Result<Self, DomainError> {
        let service_type = service_type.into();
        let data_type = data_type.into();
        if service_type.trim().is_empty() || data_type.trim().is_empty() {
            return Err(DomainError::InvalidAlarmTarget);
        }
        Ok(Self::Point {
            service_type,
            channel_id,
            data_type,
            point_id,
        })
    }

    /// Creates a channel-connectivity selector.
    #[must_use]
    pub const fn channel_online(channel_id: ChannelId) -> Self {
        Self::ChannelOnline { channel_id }
    }

    /// Returns the physical channel identity.
    #[must_use]
    pub const fn channel_id(&self) -> ChannelId {
        match self {
            Self::Point { channel_id, .. } | Self::ChannelOnline { channel_id } => *channel_id,
        }
    }
}

/// Validated alarm policy persisted behind the alarm mutation port.
#[derive(Debug, Clone, PartialEq)]
pub struct AlarmRuleDefinition {
    target: AlarmRuleTarget,
    name: String,
    severity: AlarmSeverity,
    comparator: AlarmComparator,
    threshold: f64,
    enabled: bool,
    description: Option<String>,
}

impl AlarmRuleDefinition {
    /// Creates a validated alarm policy.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        target: AlarmRuleTarget,
        name: impl Into<String>,
        severity: AlarmSeverity,
        comparator: AlarmComparator,
        threshold: f64,
        enabled: bool,
        description: Option<String>,
    ) -> Result<Self, DomainError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(DomainError::InvalidAlarmRuleName);
        }
        if !threshold.is_finite() {
            return Err(DomainError::NonFiniteAlarmThreshold);
        }
        Ok(Self {
            target,
            name,
            severity,
            comparator,
            threshold,
            enabled,
            description,
        })
    }

    /// Returns the monitored target.
    #[must_use]
    pub const fn target(&self) -> &AlarmRuleTarget {
        &self.target
    }

    /// Returns the human-readable rule name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the configured severity.
    #[must_use]
    pub const fn severity(&self) -> AlarmSeverity {
        self.severity
    }

    /// Returns the comparison operator.
    #[must_use]
    pub const fn comparator(&self) -> AlarmComparator {
        self.comparator
    }

    /// Returns the comparison threshold.
    #[must_use]
    pub const fn threshold(&self) -> f64 {
        self.threshold
    }

    /// Returns whether monitoring starts enabled.
    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    /// Returns the optional operator description.
    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// Returns a stable, non-sensitive target description for audit records.
    #[must_use]
    pub fn target_label(&self) -> String {
        match &self.target {
            AlarmRuleTarget::Point {
                service_type,
                channel_id,
                data_type,
                point_id,
            } => alloc::format!(
                "service={service_type}; channel_id={}; data_type={data_type}; point_id={}",
                channel_id.get(),
                point_id.get()
            ),
            AlarmRuleTarget::ChannelOnline { channel_id } => {
                alloc::format!("channel_id={}; channel_online=true", channel_id.get())
            },
        }
    }
}
