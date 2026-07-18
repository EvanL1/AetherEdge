//! Closed Integration v1alpha1 wire data transfer objects.

use std::collections::BTreeSet;
use std::fmt;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use crate::error::{
    ContractResult, IntegrationContractError, IntegrationContractErrorCode as Code,
};
use crate::validation::{
    MAX_AREAS, MAX_DEVICES, MAX_DIAGNOSTIC_CHARS, MAX_ENTITIES, MAX_OBSERVATIONS,
    MAX_POINTS_PER_ENTITY, MAX_STRING_VALUE_CHARS, MAX_TITLE_CHARS, MAX_UNIT_CHARS,
    MAX_VERSION_CHARS, canonical_base64url, canonical_decimal, canonical_i64, canonical_u64,
    display_or_evidence_text, foundation_float64, identifier, text_bound,
};
use crate::{OBSERVATION_BATCH_SCHEMA, TOPOLOGY_SNAPSHOT_SCHEMA};

/// Public point semantics frozen by Integration v1alpha1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IntegrationPointKindV1Alpha1 {
    /// Sampled measurement evidence.
    Telemetry,
    /// Current discrete or configured state.
    Status,
    /// Transient event evidence.
    Event,
}

/// Public observed scalar discriminant frozen by Integration v1alpha1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ObservedValueTypeV1Alpha1 {
    /// JSON boolean.
    Boolean,
    /// Canonical signed decimal string.
    Int64,
    /// Canonical unsigned decimal string.
    UInt64,
    /// Foundation-safe finite JSON binary64 number.
    Float64,
    /// Canonical arbitrary-precision decimal string.
    Decimal,
    /// Bounded Unicode string.
    String,
    /// Canonical unpadded Base64url bytes.
    Bytes,
}

/// Closed observed-value union.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum ObservedValueV1Alpha1 {
    /// Boolean value.
    Boolean {
        /// JSON boolean.
        value: bool,
    },
    /// Signed integer value.
    Int64 {
        /// Canonical signed decimal string.
        value: String,
    },
    /// Unsigned integer value.
    UInt64 {
        /// Canonical unsigned decimal string.
        value: String,
    },
    /// Floating-point value.
    Float64 {
        /// Foundation-safe finite binary64 number.
        #[serde(deserialize_with = "deserialize_foundation_float64")]
        value: f64,
    },
    /// Arbitrary-precision decimal value.
    Decimal {
        /// Canonical decimal string.
        value: String,
    },
    /// Unicode text.
    String {
        /// Bounded string value.
        value: String,
    },
    /// Opaque byte sequence.
    Bytes {
        /// Required canonical encoding discriminator.
        encoding: String,
        /// Unpadded canonical Base64url.
        value: String,
    },
}

impl ObservedValueV1Alpha1 {
    pub(crate) fn validate(&self) -> ContractResult<()> {
        match self {
            Self::Boolean { .. } => Ok(()),
            Self::Int64 { value } => canonical_i64(value).map(|_value| ()),
            Self::UInt64 { value } => canonical_u64(value).map(|_value| ()),
            Self::Float64 { value } => foundation_float64(*value),
            Self::Decimal { value } => canonical_decimal(value),
            Self::String { value } => {
                if value.chars().count() > MAX_STRING_VALUE_CHARS {
                    return Err(IntegrationContractError::new(
                        Code::FieldBound,
                        "observed string exceeds 4096 characters",
                    ));
                }
                Ok(())
            },
            Self::Bytes { encoding, value } => {
                if encoding != "base64url" {
                    return Err(IntegrationContractError::new(
                        Code::ValueEncodingInvalid,
                        "bytes encoding must be base64url",
                    ));
                }
                canonical_base64url(value)
            },
        }
    }

    /// Returns the closed public discriminant.
    #[must_use]
    pub const fn value_type(&self) -> ObservedValueTypeV1Alpha1 {
        match self {
            Self::Boolean { .. } => ObservedValueTypeV1Alpha1::Boolean,
            Self::Int64 { .. } => ObservedValueTypeV1Alpha1::Int64,
            Self::UInt64 { .. } => ObservedValueTypeV1Alpha1::UInt64,
            Self::Float64 { .. } => ObservedValueTypeV1Alpha1::Float64,
            Self::Decimal { .. } => ObservedValueTypeV1Alpha1::Decimal,
            Self::String { .. } => ObservedValueTypeV1Alpha1::String,
            Self::Bytes { .. } => ObservedValueTypeV1Alpha1::Bytes,
        }
    }
}

fn deserialize_foundation_float64<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: Deserializer<'de>,
{
    struct FoundationFloatVisitor;

    impl<'de> Visitor<'de> for FoundationFloatVisitor {
        type Value = f64;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a Foundation-safe finite IEEE 754 binary64 number")
        }

        fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;
            if value.unsigned_abs() > MAX_SAFE_INTEGER.unsigned_abs() {
                return Err(E::custom(Code::JsonUnsafeNumber.as_str()));
            }
            Ok(value as f64)
        }

        fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
            if value > MAX_SAFE_INTEGER {
                return Err(E::custom(Code::JsonUnsafeNumber.as_str()));
            }
            Ok(value as f64)
        }

        fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            foundation_float64(value).map_err(|error| E::custom(error.code().as_str()))?;
            Ok(value)
        }
    }

    deserializer.deserialize_f64(FoundationFloatVisitor)
}

/// One public area record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationAreaV1Alpha1 {
    pub(crate) area_id: String,
    pub(crate) name: String,
}

impl IntegrationAreaV1Alpha1 {
    /// Returns the stable area identity.
    #[must_use]
    pub fn area_id(&self) -> &str {
        &self.area_id
    }

    /// Returns the current display name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// One public device record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationDeviceV1Alpha1 {
    pub(crate) device_id: String,
    pub(crate) name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) area_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) manufacturer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) software_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) hardware_version: Option<String>,
}

impl IntegrationDeviceV1Alpha1 {
    /// Returns the stable device identity.
    #[must_use]
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// Returns the current display name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the directly assigned area.
    #[must_use]
    pub fn area_id(&self) -> Option<&str> {
        self.area_id.as_deref()
    }

    /// Returns the optional manufacturer.
    #[must_use]
    pub fn manufacturer(&self) -> Option<&str> {
        self.manufacturer.as_deref()
    }

    /// Returns the optional model.
    #[must_use]
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// Returns the optional software version.
    #[must_use]
    pub fn software_version(&self) -> Option<&str> {
        self.software_version.as_deref()
    }

    /// Returns the optional hardware version.
    #[must_use]
    pub fn hardware_version(&self) -> Option<&str> {
        self.hardware_version.as_deref()
    }
}

/// One public entity point descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntityPointDescriptorV1Alpha1 {
    pub(crate) point_key: String,
    pub(crate) title: String,
    pub(crate) kind: IntegrationPointKindV1Alpha1,
    pub(crate) value_type: ObservedValueTypeV1Alpha1,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unit: Option<String>,
}

impl EntityPointDescriptorV1Alpha1 {
    /// Returns the stable entity-local point key.
    #[must_use]
    pub fn point_key(&self) -> &str {
        &self.point_key
    }

    /// Returns the display title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Returns the public semantic point kind.
    #[must_use]
    pub const fn kind(&self) -> IntegrationPointKindV1Alpha1 {
        self.kind
    }

    /// Returns the closed public scalar type.
    #[must_use]
    pub const fn value_type(&self) -> ObservedValueTypeV1Alpha1 {
        self.value_type
    }

    /// Returns the optional unit.
    #[must_use]
    pub fn unit(&self) -> Option<&str> {
        self.unit.as_deref()
    }
}

/// One public delegated-provider entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationEntityV1Alpha1 {
    pub(crate) entity_id: String,
    pub(crate) source_address: String,
    pub(crate) name: String,
    pub(crate) entity_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) device_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) area_id: Option<String>,
    pub(crate) points: Vec<EntityPointDescriptorV1Alpha1>,
}

impl IntegrationEntityV1Alpha1 {
    /// Returns the stable provider registry identity.
    #[must_use]
    pub fn entity_id(&self) -> &str {
        &self.entity_id
    }

    /// Returns the current mutable provider address.
    #[must_use]
    pub fn source_address(&self) -> &str {
        &self.source_address
    }

    /// Returns the current display name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the provider-neutral constrained entity kind.
    #[must_use]
    pub fn entity_kind(&self) -> &str {
        &self.entity_kind
    }

    /// Returns the optional device reference.
    #[must_use]
    pub fn device_id(&self) -> Option<&str> {
        self.device_id.as_deref()
    }

    /// Returns the optional explicit area reference.
    #[must_use]
    pub fn area_id(&self) -> Option<&str> {
        self.area_id.as_deref()
    }

    /// Returns the closed point descriptors.
    #[must_use]
    pub fn points(&self) -> &[EntityPointDescriptorV1Alpha1] {
        &self.points
    }
}

/// Complete closed public topology snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationTopologySnapshotV1Alpha1 {
    pub(crate) schema: String,
    pub(crate) integration_id: String,
    pub(crate) integration_kind: String,
    pub(crate) snapshot_generation: String,
    pub(crate) observed_at_ms: String,
    pub(crate) areas: Vec<IntegrationAreaV1Alpha1>,
    pub(crate) devices: Vec<IntegrationDeviceV1Alpha1>,
    pub(crate) entities: Vec<IntegrationEntityV1Alpha1>,
}

impl IntegrationTopologySnapshotV1Alpha1 {
    pub(crate) fn validate(&self) -> ContractResult<()> {
        if self.schema != TOPOLOGY_SNAPSHOT_SCHEMA {
            return Err(IntegrationContractError::new(
                Code::UnsupportedSchema,
                "unsupported topology schema",
            ));
        }
        identifier(&self.integration_id)?;
        identifier(&self.integration_kind)?;
        canonical_u64(&self.snapshot_generation)?;
        canonical_u64(&self.observed_at_ms)?;
        bound_collection(self.areas.len(), MAX_AREAS, "too many integration areas")?;
        bound_collection(
            self.devices.len(),
            MAX_DEVICES,
            "too many integration devices",
        )?;
        bound_collection(
            self.entities.len(),
            MAX_ENTITIES,
            "too many integration entities",
        )?;

        let mut area_ids = BTreeSet::new();
        for area in &self.areas {
            identifier(&area.area_id)?;
            display_or_evidence_text(&area.name, MAX_TITLE_CHARS, "area name exceeds its bound")?;
            if !area_ids.insert(area.area_id.as_str()) {
                return identity_conflict();
            }
        }

        let mut device_ids = BTreeSet::new();
        for device in &self.devices {
            identifier(&device.device_id)?;
            display_or_evidence_text(
                &device.name,
                MAX_TITLE_CHARS,
                "device name exceeds its bound",
            )?;
            optional_identifier(device.area_id.as_deref())?;
            optional_display_or_evidence_text(
                device.manufacturer.as_deref(),
                MAX_TITLE_CHARS,
                "manufacturer exceeds its bound",
            )?;
            optional_display_or_evidence_text(
                device.model.as_deref(),
                MAX_TITLE_CHARS,
                "model exceeds its bound",
            )?;
            optional_display_or_evidence_text(
                device.software_version.as_deref(),
                MAX_VERSION_CHARS,
                "software version exceeds its bound",
            )?;
            optional_display_or_evidence_text(
                device.hardware_version.as_deref(),
                MAX_VERSION_CHARS,
                "hardware version exceeds its bound",
            )?;
            if !device_ids.insert(device.device_id.as_str()) {
                return identity_conflict();
            }
        }

        let mut entity_ids = BTreeSet::new();
        let mut source_addresses = BTreeSet::new();
        for entity in &self.entities {
            identifier(&entity.entity_id)?;
            identifier(&entity.source_address)?;
            identifier(&entity.entity_kind)?;
            display_or_evidence_text(
                &entity.name,
                MAX_TITLE_CHARS,
                "entity name exceeds its bound",
            )?;
            optional_identifier(entity.device_id.as_deref())?;
            optional_identifier(entity.area_id.as_deref())?;
            if entity.points.is_empty() || entity.points.len() > MAX_POINTS_PER_ENTITY {
                return Err(IntegrationContractError::new(
                    Code::FieldBound,
                    "entity points must contain between 1 and 64 entries",
                ));
            }
            if !entity_ids.insert(entity.entity_id.as_str())
                || !source_addresses.insert(entity.source_address.as_str())
            {
                return identity_conflict();
            }
            let mut point_keys = BTreeSet::new();
            for point in &entity.points {
                identifier(&point.point_key)?;
                display_or_evidence_text(
                    &point.title,
                    MAX_TITLE_CHARS,
                    "point title exceeds its bound",
                )?;
                optional_text(
                    point.unit.as_deref(),
                    MAX_UNIT_CHARS,
                    "point unit exceeds its bound",
                )?;
                if !point_keys.insert(point.point_key.as_str()) {
                    return identity_conflict();
                }
            }
        }

        if self
            .devices
            .iter()
            .filter_map(|device| device.area_id.as_deref())
            .any(|area_id| !area_ids.contains(area_id))
            || self.entities.iter().any(|entity| {
                entity
                    .area_id
                    .as_deref()
                    .is_some_and(|area_id| !area_ids.contains(area_id))
                    || entity
                        .device_id
                        .as_deref()
                        .is_some_and(|device_id| !device_ids.contains(device_id))
            })
        {
            return Err(IntegrationContractError::new(
                Code::ReferenceNotFound,
                "topology contains a dangling reference",
            ));
        }
        Ok(())
    }

    /// Returns the public integration identity.
    #[must_use]
    pub fn integration_id(&self) -> &str {
        &self.integration_id
    }

    /// Returns the constrained public integration kind.
    #[must_use]
    pub fn integration_kind(&self) -> &str {
        &self.integration_kind
    }

    /// Returns the canonical snapshot-generation string.
    #[must_use]
    pub fn snapshot_generation(&self) -> &str {
        &self.snapshot_generation
    }

    /// Returns the canonical observation-time string.
    #[must_use]
    pub fn observed_at_ms(&self) -> &str {
        &self.observed_at_ms
    }

    /// Returns all areas in the complete snapshot.
    #[must_use]
    pub fn areas(&self) -> &[IntegrationAreaV1Alpha1] {
        &self.areas
    }

    /// Returns all devices in the complete snapshot.
    #[must_use]
    pub fn devices(&self) -> &[IntegrationDeviceV1Alpha1] {
        &self.devices
    }

    /// Returns all entities in the complete snapshot.
    #[must_use]
    pub fn entities(&self) -> &[IntegrationEntityV1Alpha1] {
        &self.entities
    }
}

/// Public provider-evidence quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IntegrationObservationQualityV1Alpha1 {
    /// Fresh typed value.
    Good,
    /// Retained typed value whose freshness is uncertain.
    Uncertain,
    /// Provider error with no value.
    Bad,
    /// Provider value is absent or unavailable.
    Unavailable,
}

/// One closed public point observation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationObservationV1Alpha1 {
    pub(crate) entity_id: String,
    pub(crate) point_key: String,
    pub(crate) observed_at_ms: String,
    pub(crate) quality: IntegrationObservationQualityV1Alpha1,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) value: Option<ObservedValueV1Alpha1>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) diagnostic: Option<String>,
}

impl IntegrationObservationV1Alpha1 {
    fn validate_wire(&self) -> ContractResult<()> {
        identifier(&self.entity_id)?;
        identifier(&self.point_key)?;
        canonical_u64(&self.observed_at_ms)?;
        optional_display_or_evidence_text(
            self.diagnostic.as_deref(),
            MAX_DIAGNOSTIC_CHARS,
            "observation diagnostic exceeds its bound",
        )?;
        if let Some(value) = &self.value {
            value.validate()?;
        }
        let requires_value = matches!(
            self.quality,
            IntegrationObservationQualityV1Alpha1::Good
                | IntegrationObservationQualityV1Alpha1::Uncertain
        );
        if requires_value != self.value.is_some() {
            return Err(IntegrationContractError::new(
                Code::ObservationValueInvalid,
                "observation quality and value presence disagree",
            ));
        }
        Ok(())
    }

    /// Returns the stable entity reference.
    #[must_use]
    pub fn entity_id(&self) -> &str {
        &self.entity_id
    }

    /// Returns the stable entity-local point reference.
    #[must_use]
    pub fn point_key(&self) -> &str {
        &self.point_key
    }

    /// Returns the canonical provider observation time.
    #[must_use]
    pub fn observed_at_ms(&self) -> &str {
        &self.observed_at_ms
    }

    /// Returns the public provider-evidence quality.
    #[must_use]
    pub const fn quality(&self) -> IntegrationObservationQualityV1Alpha1 {
        self.quality
    }

    /// Returns the typed value when quality permits one.
    #[must_use]
    pub const fn value(&self) -> Option<&ObservedValueV1Alpha1> {
        self.value.as_ref()
    }

    /// Returns the bounded diagnostic when present.
    #[must_use]
    pub fn diagnostic(&self) -> Option<&str> {
        self.diagnostic.as_deref()
    }
}

/// Complete closed public observation batch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationObservationBatchV1Alpha1 {
    pub(crate) schema: String,
    pub(crate) integration_id: String,
    pub(crate) snapshot_generation: String,
    pub(crate) batch_id: String,
    pub(crate) observed_at_ms: String,
    pub(crate) observations: Vec<IntegrationObservationV1Alpha1>,
}

impl IntegrationObservationBatchV1Alpha1 {
    pub(crate) fn validate_wire(&self) -> ContractResult<()> {
        if self.schema != OBSERVATION_BATCH_SCHEMA {
            return Err(IntegrationContractError::new(
                Code::UnsupportedSchema,
                "unsupported observation-batch schema",
            ));
        }
        identifier(&self.integration_id)?;
        canonical_u64(&self.snapshot_generation)?;
        identifier(&self.batch_id)?;
        canonical_u64(&self.observed_at_ms)?;
        if self.observations.is_empty() || self.observations.len() > MAX_OBSERVATIONS {
            return Err(IntegrationContractError::new(
                Code::FieldBound,
                "observation batch must contain between 1 and 65536 entries",
            ));
        }
        for observation in &self.observations {
            observation.validate_wire()?;
        }
        Ok(())
    }

    pub(crate) fn validate_against(
        &self,
        topology: &IntegrationTopologySnapshotV1Alpha1,
    ) -> ContractResult<()> {
        topology.validate()?;
        self.validate_wire()?;
        if self.integration_id != topology.integration_id
            || self.snapshot_generation != topology.snapshot_generation
        {
            return Err(IntegrationContractError::new(
                Code::ReferenceNotFound,
                "observation batch does not bind the supplied topology generation",
            ));
        }

        for observation in &self.observations {
            let descriptor = topology
                .entities
                .iter()
                .find(|entity| entity.entity_id == observation.entity_id)
                .and_then(|entity| {
                    entity
                        .points
                        .iter()
                        .find(|point| point.point_key == observation.point_key)
                })
                .ok_or_else(|| {
                    IntegrationContractError::new(
                        Code::ReferenceNotFound,
                        "observation refers to an undeclared point",
                    )
                })?;
            if observation
                .value
                .as_ref()
                .is_some_and(|value| value.value_type() != descriptor.value_type)
            {
                return Err(IntegrationContractError::new(
                    Code::ValueTypeMismatch,
                    "observation value differs from the declared point type",
                ));
            }
        }
        Ok(())
    }

    /// Returns the public integration identity.
    #[must_use]
    pub fn integration_id(&self) -> &str {
        &self.integration_id
    }

    /// Returns the exact canonical topology generation binding.
    #[must_use]
    pub fn snapshot_generation(&self) -> &str {
        &self.snapshot_generation
    }

    /// Returns the stable batch identity.
    #[must_use]
    pub fn batch_id(&self) -> &str {
        &self.batch_id
    }

    /// Returns the canonical batch observation time.
    #[must_use]
    pub fn observed_at_ms(&self) -> &str {
        &self.observed_at_ms
    }

    /// Returns all point observations.
    #[must_use]
    pub fn observations(&self) -> &[IntegrationObservationV1Alpha1] {
        &self.observations
    }
}

fn bound_collection(length: usize, maximum: usize, message: &'static str) -> ContractResult<()> {
    if length > maximum {
        return Err(IntegrationContractError::new(Code::FieldBound, message));
    }
    Ok(())
}

fn optional_identifier(value: Option<&str>) -> ContractResult<()> {
    value.map_or(Ok(()), identifier)
}

fn optional_text(value: Option<&str>, maximum: usize, message: &'static str) -> ContractResult<()> {
    value.map_or(Ok(()), |value| text_bound(value, maximum, message))
}

fn optional_display_or_evidence_text(
    value: Option<&str>,
    maximum: usize,
    message: &'static str,
) -> ContractResult<()> {
    value.map_or(Ok(()), |value| {
        display_or_evidence_text(value, maximum, message)
    })
}

fn identity_conflict<T>() -> ContractResult<T> {
    Err(IntegrationContractError::new(
        Code::IdentityConflict,
        "topology identities are not unique",
    ))
}
