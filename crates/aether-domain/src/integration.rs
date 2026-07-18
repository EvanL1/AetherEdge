//! Vendor-neutral topology and state reported by delegated device integrations.

use alloc::collections::BTreeSet;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::{DomainError, TimestampMs};

const MAX_IDENTIFIER_BYTES: usize = 512;
const MAX_DISPLAY_NAME_BYTES: usize = 512;
const MAX_ALIAS_VALUE_BYTES: usize = 1_024;
const MAX_STATE_STRING_BYTES: usize = 8_192;
const MAX_CONTEXT_BYTES: usize = 512;

fn validated_identifier(value: impl Into<String>) -> Result<String, DomainError> {
    let value = value.into();
    if value.is_empty()
        || value.trim() != value
        || value.len() > MAX_IDENTIFIER_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(DomainError::InvalidIntegrationIdentifier);
    }
    Ok(value)
}

fn validated_display_name(value: impl Into<String>) -> Result<String, DomainError> {
    let value = value.into();
    if value.trim().is_empty()
        || value.len() > MAX_DISPLAY_NAME_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(DomainError::InvalidIntegrationDisplayName);
    }
    Ok(value)
}

macro_rules! integration_id {
    ($name:ident) => {
        #[doc = concat!("Validated `", stringify!($name), "` scoped to an integration topology.")]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            #[doc = concat!("Creates a validated `", stringify!($name), "`.")]
            pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
                Ok(Self(validated_identifier(value)?))
            }

            /// Returns the opaque identifier.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

integration_id!(GatewayIdentity);
integration_id!(IntegrationId);
integration_id!(AreaId);
integration_id!(DeviceId);
integration_id!(EntityId);
integration_id!(IntegrationPointKey);

/// Monotonic generation of one integration's complete topology snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TopologyGeneration(u64);

impl TopologyGeneration {
    /// Creates a positive topology generation.
    pub const fn new(value: u64) -> Result<Self, DomainError> {
        if value == 0 {
            return Err(DomainError::ZeroTopologyGeneration);
        }
        Ok(Self(value))
    }

    /// Returns the generation as an unsigned integer.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Digest of the canonical complete topology snapshot.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SnapshotDigest(String);

impl SnapshotDigest {
    /// Creates a lowercase `sha256:` digest.
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        let digest = value
            .strip_prefix("sha256:")
            .ok_or(DomainError::InvalidSnapshotDigest)?;
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(DomainError::InvalidSnapshotDigest);
        }
        Ok(Self(value))
    }

    /// Returns the canonical digest spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Provider-native alias retained without becoming Aether identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExternalAlias {
    namespace: String,
    kind: String,
    value: String,
}

impl ExternalAlias {
    /// Creates a validated provider alias.
    pub fn new(
        namespace: impl Into<String>,
        kind: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<Self, DomainError> {
        let value = value.into();
        if value.trim().is_empty()
            || value.len() > MAX_ALIAS_VALUE_BYTES
            || value.chars().any(char::is_control)
        {
            return Err(DomainError::InvalidIntegrationIdentifier);
        }
        Ok(Self {
            namespace: validated_identifier(namespace)?,
            kind: validated_identifier(kind)?,
            value,
        })
    }

    /// Returns the alias namespace.
    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// Returns the alias kind inside the namespace.
    #[must_use]
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// Returns the provider-native alias value.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }
}

fn validate_aliases(aliases: &[ExternalAlias]) -> Result<(), DomainError> {
    let mut seen = BTreeSet::new();
    for alias in aliases {
        if !seen.insert((alias.namespace(), alias.kind(), alias.value())) {
            return Err(DomainError::DuplicateExternalAlias);
        }
    }
    Ok(())
}

/// One area exposed by a delegated integration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AreaRecord {
    id: AreaId,
    display_name: String,
    aliases: Vec<ExternalAlias>,
}

impl AreaRecord {
    /// Creates an area record.
    pub fn new(
        id: AreaId,
        display_name: impl Into<String>,
        aliases: Vec<ExternalAlias>,
    ) -> Result<Self, DomainError> {
        validate_aliases(&aliases)?;
        Ok(Self {
            id,
            display_name: validated_display_name(display_name)?,
            aliases,
        })
    }

    /// Returns the provider-stable area identifier.
    #[must_use]
    pub const fn id(&self) -> &AreaId {
        &self.id
    }

    /// Returns the current operator-visible name.
    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Returns provider-native aliases.
    #[must_use]
    pub fn aliases(&self) -> &[ExternalAlias] {
        &self.aliases
    }
}

/// One physical or logical device exposed by a delegated integration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRecord {
    id: DeviceId,
    display_name: String,
    area_id: Option<AreaId>,
    aliases: Vec<ExternalAlias>,
}

impl DeviceRecord {
    /// Creates a device record.
    pub fn new(
        id: DeviceId,
        display_name: impl Into<String>,
        area_id: Option<AreaId>,
        aliases: Vec<ExternalAlias>,
    ) -> Result<Self, DomainError> {
        validate_aliases(&aliases)?;
        Ok(Self {
            id,
            display_name: validated_display_name(display_name)?,
            area_id,
            aliases,
        })
    }

    /// Returns the provider-stable device identifier.
    #[must_use]
    pub const fn id(&self) -> &DeviceId {
        &self.id
    }

    /// Returns the current operator-visible name.
    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Returns the directly assigned area, if any.
    #[must_use]
    pub const fn area_id(&self) -> Option<&AreaId> {
        self.area_id.as_ref()
    }

    /// Returns provider-native aliases.
    #[must_use]
    pub fn aliases(&self) -> &[ExternalAlias] {
        &self.aliases
    }
}

/// Stable scalar kinds carried across integration boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservedValueType {
    /// Boolean state.
    Boolean,
    /// Signed 64-bit integer.
    Int64,
    /// Unsigned 64-bit integer.
    UInt64,
    /// Finite IEEE 754 binary64 value.
    Float64,
    /// Arbitrary-precision decimal represented canonically as text.
    Decimal,
    /// UTF-8 text.
    String,
    /// Opaque bytes.
    Bytes,
    /// One symbolic choice.
    Enum,
}

/// A provider-observed value that preserves its original scalar type.
#[derive(Debug, Clone, PartialEq)]
pub enum ObservedValue {
    /// Boolean state.
    Boolean(bool),
    /// Signed 64-bit integer.
    Int64(i64),
    /// Unsigned 64-bit integer.
    UInt64(u64),
    /// Finite IEEE 754 binary64 value.
    Float64(f64),
    /// Canonical decimal text.
    Decimal(String),
    /// UTF-8 text.
    String(String),
    /// Opaque bytes.
    Bytes(Vec<u8>),
    /// Symbolic choice.
    Enum(String),
}

impl ObservedValue {
    /// Creates a boolean value.
    #[must_use]
    pub const fn boolean(value: bool) -> Self {
        Self::Boolean(value)
    }

    /// Creates a signed integer value.
    #[must_use]
    pub const fn int64(value: i64) -> Self {
        Self::Int64(value)
    }

    /// Creates an unsigned integer value.
    #[must_use]
    pub const fn uint64(value: u64) -> Self {
        Self::UInt64(value)
    }

    /// Creates a finite floating-point value.
    pub fn float64(value: f64) -> Result<Self, DomainError> {
        if !value.is_finite() {
            return Err(DomainError::NonFiniteObservedValue);
        }
        Ok(Self::Float64(value))
    }

    /// Creates a canonical decimal value without losing precision.
    pub fn decimal(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        if !is_canonical_decimal(&value) {
            return Err(DomainError::InvalidObservedDecimal);
        }
        Ok(Self::Decimal(value))
    }

    /// Creates a bounded UTF-8 string value.
    pub fn string(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        if value.len() > MAX_STATE_STRING_BYTES {
            return Err(DomainError::ObservedValueTooLarge);
        }
        Ok(Self::String(value))
    }

    /// Creates an opaque byte value.
    #[must_use]
    pub fn bytes(value: Vec<u8>) -> Self {
        Self::Bytes(value)
    }

    /// Creates a non-empty symbolic choice.
    pub fn enumeration(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        if value.trim().is_empty() || value.len() > MAX_STATE_STRING_BYTES {
            return Err(DomainError::InvalidObservedEnum);
        }
        Ok(Self::Enum(value))
    }

    /// Returns the scalar type carried by this value.
    #[must_use]
    pub const fn value_type(&self) -> ObservedValueType {
        match self {
            Self::Boolean(_) => ObservedValueType::Boolean,
            Self::Int64(_) => ObservedValueType::Int64,
            Self::UInt64(_) => ObservedValueType::UInt64,
            Self::Float64(_) => ObservedValueType::Float64,
            Self::Decimal(_) => ObservedValueType::Decimal,
            Self::String(_) => ObservedValueType::String,
            Self::Bytes(_) => ObservedValueType::Bytes,
            Self::Enum(_) => ObservedValueType::Enum,
        }
    }
}

fn is_canonical_decimal(value: &str) -> bool {
    let unsigned = value.strip_prefix('-').unwrap_or(value);
    if unsigned.is_empty() {
        return false;
    }
    let mut parts = unsigned.split('.');
    let integer = match parts.next() {
        Some(integer) => integer,
        None => return false,
    };
    let fractional = parts.next();
    if parts.next().is_some()
        || integer.is_empty()
        || !integer.bytes().all(|byte| byte.is_ascii_digit())
        || (integer.len() > 1 && integer.starts_with('0'))
        || (value.starts_with('-') && integer == "0" && fractional.is_none())
    {
        return false;
    }
    fractional
        .is_none_or(|digits| !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()))
}

/// One entity exposed by a delegated integration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrationPointKind {
    /// Primary entity state.
    State,
    /// A bounded, explicitly mapped entity attribute.
    Attribute,
    /// A transient integration event.
    Event,
}

/// One stable semantic point projected from an integration entity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityPointDescriptor {
    key: IntegrationPointKey,
    display_name: String,
    kind: IntegrationPointKind,
    value_type: ObservedValueType,
    unit: Option<String>,
}

impl EntityPointDescriptor {
    /// Creates an explicitly mapped entity point.
    pub fn new(
        key: IntegrationPointKey,
        display_name: impl Into<String>,
        kind: IntegrationPointKind,
        value_type: ObservedValueType,
        unit: Option<&str>,
    ) -> Result<Self, DomainError> {
        let unit = unit
            .map(|unit| {
                if unit.trim().is_empty() || unit.len() > 64 || unit.chars().any(char::is_control) {
                    return Err(DomainError::InvalidIntegrationUnit);
                }
                Ok(unit.to_string())
            })
            .transpose()?;
        Ok(Self {
            key,
            display_name: validated_display_name(display_name)?,
            kind,
            value_type,
            unit,
        })
    }

    /// Returns the stable semantic point key.
    #[must_use]
    pub const fn key(&self) -> &IntegrationPointKey {
        &self.key
    }

    /// Returns the operator-visible point name.
    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Returns whether the point is state, attribute, or transient event.
    #[must_use]
    pub const fn kind(&self) -> IntegrationPointKind {
        self.kind
    }

    /// Returns the declared scalar type.
    #[must_use]
    pub const fn value_type(&self) -> ObservedValueType {
        self.value_type
    }

    /// Returns the normalized unit, if one is declared.
    #[must_use]
    pub fn unit(&self) -> Option<&str> {
        self.unit.as_deref()
    }
}

/// One entity exposed by a delegated integration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityRecord {
    id: EntityId,
    display_name: String,
    kind: String,
    points: Vec<EntityPointDescriptor>,
    device_id: Option<DeviceId>,
    area_id: Option<AreaId>,
    aliases: Vec<ExternalAlias>,
}

impl EntityRecord {
    /// Creates an entity record.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: EntityId,
        display_name: impl Into<String>,
        kind: impl Into<String>,
        points: Vec<EntityPointDescriptor>,
        device_id: Option<DeviceId>,
        area_id: Option<AreaId>,
        aliases: Vec<ExternalAlias>,
    ) -> Result<Self, DomainError> {
        validate_aliases(&aliases)?;
        if points.is_empty() {
            return Err(DomainError::EmptyIntegrationPoints);
        }
        let mut point_keys = BTreeSet::new();
        if points
            .iter()
            .any(|point| !point_keys.insert(point.key().as_str()))
        {
            return Err(DomainError::DuplicateIntegrationPoint);
        }
        Ok(Self {
            id,
            display_name: validated_display_name(display_name)?,
            kind: validated_identifier(kind)?,
            points,
            device_id,
            area_id,
            aliases,
        })
    }

    /// Returns the provider-stable entity identifier.
    #[must_use]
    pub const fn id(&self) -> &EntityId {
        &self.id
    }

    /// Returns the current operator-visible name.
    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Returns the provider-neutral entity kind.
    #[must_use]
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// Returns explicitly mapped state, attribute, and event points.
    #[must_use]
    pub fn points(&self) -> &[EntityPointDescriptor] {
        &self.points
    }

    /// Returns the associated device, if any.
    #[must_use]
    pub const fn device_id(&self) -> Option<&DeviceId> {
        self.device_id.as_ref()
    }

    /// Returns the entity's explicit area, if any.
    #[must_use]
    pub const fn area_id(&self) -> Option<&AreaId> {
        self.area_id.as_ref()
    }

    /// Resolves the explicit entity area before the device-inherited area.
    #[must_use]
    pub fn effective_area_id<'a>(&'a self, devices: &'a [DeviceRecord]) -> Option<&'a AreaId> {
        self.area_id.as_ref().or_else(|| {
            let device_id = self.device_id.as_ref()?;
            devices
                .iter()
                .find(|device| device.id() == device_id)
                .and_then(DeviceRecord::area_id)
        })
    }

    /// Returns provider-native aliases.
    #[must_use]
    pub fn aliases(&self) -> &[ExternalAlias] {
        &self.aliases
    }
}

/// Complete, generation-fenced topology reported by one delegated integration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationTopologySnapshot {
    gateway_id: GatewayIdentity,
    integration_id: IntegrationId,
    generation: TopologyGeneration,
    observed_at: TimestampMs,
    digest: SnapshotDigest,
    areas: Vec<AreaRecord>,
    devices: Vec<DeviceRecord>,
    entities: Vec<EntityRecord>,
}

impl IntegrationTopologySnapshot {
    /// Creates a complete snapshot after validating uniqueness and references.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        gateway_id: GatewayIdentity,
        integration_id: IntegrationId,
        generation: TopologyGeneration,
        observed_at: TimestampMs,
        digest: SnapshotDigest,
        areas: Vec<AreaRecord>,
        devices: Vec<DeviceRecord>,
        entities: Vec<EntityRecord>,
    ) -> Result<Self, DomainError> {
        ensure_unique(areas.iter().map(|area| area.id().as_str()))?;
        ensure_unique(devices.iter().map(|device| device.id().as_str()))?;
        ensure_unique(entities.iter().map(|entity| entity.id().as_str()))?;

        let area_ids: BTreeSet<_> = areas.iter().map(AreaRecord::id).collect();
        let device_ids: BTreeSet<_> = devices.iter().map(DeviceRecord::id).collect();
        if devices
            .iter()
            .filter_map(DeviceRecord::area_id)
            .any(|area_id| !area_ids.contains(area_id))
            || entities.iter().any(|entity| {
                entity
                    .area_id()
                    .is_some_and(|area_id| !area_ids.contains(area_id))
                    || entity
                        .device_id()
                        .is_some_and(|device_id| !device_ids.contains(device_id))
            })
        {
            return Err(DomainError::DanglingIntegrationReference);
        }

        Ok(Self {
            gateway_id,
            integration_id,
            generation,
            observed_at,
            digest,
            areas,
            devices,
            entities,
        })
    }

    /// Returns the authoritative edge gateway identity.
    #[must_use]
    pub const fn gateway_id(&self) -> &GatewayIdentity {
        &self.gateway_id
    }

    /// Returns the integration identity local to the gateway.
    #[must_use]
    pub const fn integration_id(&self) -> &IntegrationId {
        &self.integration_id
    }

    /// Returns the monotonic topology generation.
    #[must_use]
    pub const fn generation(&self) -> TopologyGeneration {
        self.generation
    }

    /// Returns when the integration observed the complete snapshot.
    #[must_use]
    pub const fn observed_at(&self) -> TimestampMs {
        self.observed_at
    }

    /// Returns the canonical snapshot digest.
    #[must_use]
    pub const fn digest(&self) -> &SnapshotDigest {
        &self.digest
    }

    /// Complete snapshots always replace the previous generation atomically.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        true
    }

    /// Returns all areas in this snapshot.
    #[must_use]
    pub fn areas(&self) -> &[AreaRecord] {
        &self.areas
    }

    /// Returns all devices in this snapshot.
    #[must_use]
    pub fn devices(&self) -> &[DeviceRecord] {
        &self.devices
    }

    /// Returns all entities in this snapshot.
    #[must_use]
    pub fn entities(&self) -> &[EntityRecord] {
        &self.entities
    }
}

fn ensure_unique<'a>(ids: impl Iterator<Item = &'a str>) -> Result<(), DomainError> {
    let mut seen = BTreeSet::new();
    for id in ids {
        if !seen.insert(id) {
            return Err(DomainError::DuplicateIntegrationResource);
        }
    }
    Ok(())
}

/// Availability semantics for an integration entity observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrationStateQuality {
    /// A typed value was observed.
    Good,
    /// The provider reports that the value is currently unknown.
    Unknown,
    /// The provider reports that the entity is unavailable.
    Unavailable,
}

/// One typed point observation scoped to a gateway integration entity.
#[derive(Debug, Clone, PartialEq)]
pub struct IntegrationObservation {
    gateway_id: GatewayIdentity,
    integration_id: IntegrationId,
    entity_id: EntityId,
    point_key: IntegrationPointKey,
    value: Option<ObservedValue>,
    quality: IntegrationStateQuality,
    observed_at: TimestampMs,
    sequence: u64,
    source_context: Option<String>,
}

impl IntegrationObservation {
    /// Creates an available state with its typed value.
    #[allow(clippy::too_many_arguments)]
    pub fn available(
        gateway_id: GatewayIdentity,
        integration_id: IntegrationId,
        entity_id: EntityId,
        point_key: IntegrationPointKey,
        value: ObservedValue,
        observed_at: TimestampMs,
        sequence: u64,
        source_context: Option<&str>,
    ) -> Result<Self, DomainError> {
        Self::new(
            gateway_id,
            integration_id,
            entity_id,
            point_key,
            Some(value),
            IntegrationStateQuality::Good,
            observed_at,
            sequence,
            source_context,
        )
    }

    /// Creates an observation whose provider value is unknown.
    pub fn unknown(
        gateway_id: GatewayIdentity,
        integration_id: IntegrationId,
        entity_id: EntityId,
        point_key: IntegrationPointKey,
        observed_at: TimestampMs,
        sequence: u64,
        source_context: Option<&str>,
    ) -> Result<Self, DomainError> {
        Self::new(
            gateway_id,
            integration_id,
            entity_id,
            point_key,
            None,
            IntegrationStateQuality::Unknown,
            observed_at,
            sequence,
            source_context,
        )
    }

    /// Creates an observation whose provider entity is unavailable.
    pub fn unavailable(
        gateway_id: GatewayIdentity,
        integration_id: IntegrationId,
        entity_id: EntityId,
        point_key: IntegrationPointKey,
        observed_at: TimestampMs,
        sequence: u64,
        source_context: Option<&str>,
    ) -> Result<Self, DomainError> {
        Self::new(
            gateway_id,
            integration_id,
            entity_id,
            point_key,
            None,
            IntegrationStateQuality::Unavailable,
            observed_at,
            sequence,
            source_context,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new(
        gateway_id: GatewayIdentity,
        integration_id: IntegrationId,
        entity_id: EntityId,
        point_key: IntegrationPointKey,
        value: Option<ObservedValue>,
        quality: IntegrationStateQuality,
        observed_at: TimestampMs,
        sequence: u64,
        source_context: Option<&str>,
    ) -> Result<Self, DomainError> {
        if sequence == 0 {
            return Err(DomainError::ZeroIntegrationStateSequence);
        }
        let source_context = source_context
            .map(|context| {
                if context.trim().is_empty()
                    || context.len() > MAX_CONTEXT_BYTES
                    || context.chars().any(char::is_control)
                {
                    return Err(DomainError::InvalidIntegrationContext);
                }
                Ok(context.to_string())
            })
            .transpose()?;
        Ok(Self {
            gateway_id,
            integration_id,
            entity_id,
            point_key,
            value,
            quality,
            observed_at,
            sequence,
            source_context,
        })
    }

    /// Returns the gateway identity.
    #[must_use]
    pub const fn gateway_id(&self) -> &GatewayIdentity {
        &self.gateway_id
    }

    /// Returns the integration identity.
    #[must_use]
    pub const fn integration_id(&self) -> &IntegrationId {
        &self.integration_id
    }

    /// Returns the provider-stable entity identity.
    #[must_use]
    pub const fn entity_id(&self) -> &EntityId {
        &self.entity_id
    }

    /// Returns the stable semantic point key within the entity.
    #[must_use]
    pub const fn point_key(&self) -> &IntegrationPointKey {
        &self.point_key
    }

    /// Returns the typed value, absent only for unknown or unavailable states.
    #[must_use]
    pub const fn value(&self) -> Option<&ObservedValue> {
        self.value.as_ref()
    }

    /// Returns the provider availability evidence.
    #[must_use]
    pub const fn quality(&self) -> IntegrationStateQuality {
        self.quality
    }

    /// Returns the provider observation time.
    #[must_use]
    pub const fn observed_at(&self) -> TimestampMs {
        self.observed_at
    }

    /// Returns the connection-local monotonic observation sequence.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Returns the optional upstream correlation context.
    #[must_use]
    pub fn source_context(&self) -> Option<&str> {
        self.source_context.as_deref()
    }
}

/// Atomic provider snapshot containing topology and its current typed observations.
#[derive(Debug, Clone, PartialEq)]
pub struct IntegrationSnapshot {
    topology: IntegrationTopologySnapshot,
    observations: Vec<IntegrationObservation>,
}

impl IntegrationSnapshot {
    /// Creates a provider snapshot after validating scope, references, and value types.
    pub fn new(
        topology: IntegrationTopologySnapshot,
        observations: Vec<IntegrationObservation>,
    ) -> Result<Self, DomainError> {
        let mut seen = BTreeSet::new();
        for observation in &observations {
            if observation.gateway_id() != topology.gateway_id()
                || observation.integration_id() != topology.integration_id()
                || !seen.insert((
                    observation.entity_id().as_str(),
                    observation.point_key().as_str(),
                ))
            {
                return Err(DomainError::InvalidIntegrationObservation);
            }
            let entity = topology
                .entities()
                .iter()
                .find(|entity| entity.id() == observation.entity_id())
                .ok_or(DomainError::InvalidIntegrationObservation)?;
            let point = entity
                .points()
                .iter()
                .find(|point| point.key() == observation.point_key())
                .ok_or(DomainError::InvalidIntegrationObservation)?;
            match (observation.quality(), observation.value()) {
                (IntegrationStateQuality::Good, Some(value))
                    if value.value_type() == point.value_type() => {},
                (IntegrationStateQuality::Unknown | IntegrationStateQuality::Unavailable, None) => {
                },
                _ => return Err(DomainError::InvalidIntegrationObservation),
            }
        }
        Ok(Self {
            topology,
            observations,
        })
    }

    /// Returns the complete topology.
    #[must_use]
    pub const fn topology(&self) -> &IntegrationTopologySnapshot {
        &self.topology
    }

    /// Returns the initial current observations.
    #[must_use]
    pub fn observations(&self) -> &[IntegrationObservation] {
        &self.observations
    }

    /// Splits the snapshot into owned topology and observation parts.
    #[must_use]
    pub fn into_parts(self) -> (IntegrationTopologySnapshot, Vec<IntegrationObservation>) {
        (self.topology, self.observations)
    }
}
