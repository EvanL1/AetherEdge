//! Truthful mapping from edge point facts into business telemetry.

use aether_domain::{PointKind, PointQuality, PointSample};
use serde::{Deserialize, Serialize};

use crate::validation::{canonical_u64, identifier, positive_u64, topology_digest};
use crate::{CloudLinkCodecError, MAX_POINT_SAMPLES};

/// Coherent SHM topology generation attached to one batch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TopologyBinding {
    publication_epoch: String,
    snapshot_digest: String,
}

impl TopologyBinding {
    /// Creates a topology binding from the coordinated publication witness.
    pub fn new(
        publication_epoch: u64,
        snapshot_digest: impl Into<String>,
    ) -> Result<Self, CloudLinkCodecError> {
        let value = Self {
            publication_epoch: publication_epoch.to_string(),
            snapshot_digest: snapshot_digest.into(),
        };
        value.validate()?;
        Ok(value)
    }

    pub(crate) fn validate(&self) -> Result<(), CloudLinkCodecError> {
        let epoch = canonical_u64(&self.publication_epoch, "topology.publication_epoch")?;
        if epoch == 0 {
            return Err(CloudLinkCodecError::InvalidField {
                field: "topology.publication_epoch",
                message: "must be greater than zero",
            });
        }
        topology_digest(&self.snapshot_digest)
    }
}

/// One real point fact. No Thing Model revision is invented.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PointFact {
    instance_id: String,
    point_kind: PointKindWire,
    point_id: String,
    value: f64,
    source_timestamp_ms: String,
    quality: PointQualityWire,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<PointModelBinding>,
}

impl PointFact {
    fn from_sample(sample: PointSample) -> Result<Self, CloudLinkCodecError> {
        if !sample.value().is_finite() {
            return Err(CloudLinkCodecError::NonFinitePointValue);
        }
        if !sample.address().kind().is_acquisition_owned() {
            return Err(CloudLinkCodecError::ControlPointForbidden);
        }
        Ok(Self {
            instance_id: sample.address().instance_id().get().to_string(),
            point_kind: PointKindWire::from(sample.address().kind()),
            point_id: sample.address().point_id().get().to_string(),
            value: sample.value(),
            source_timestamp_ms: sample.timestamp().get().to_string(),
            quality: PointQualityWire::from(sample.quality()),
            model: None,
        })
    }

    pub(crate) fn validate(&self) -> Result<(), CloudLinkCodecError> {
        let instance = canonical_u64(&self.instance_id, "samples.instance_id")?;
        let point = canonical_u64(&self.point_id, "samples.point_id")?;
        if instance > u64::from(u32::MAX) || point > u64::from(u32::MAX) {
            return Err(CloudLinkCodecError::InvalidField {
                field: "samples.point identity",
                message: "must fit the current Edge uint32 identity",
            });
        }
        canonical_u64(&self.source_timestamp_ms, "samples.source_timestamp_ms")?;
        if !self.value.is_finite() {
            return Err(CloudLinkCodecError::NonFinitePointValue);
        }
        if matches!(
            self.point_kind,
            PointKindWire::Command | PointKindWire::Action
        ) {
            return Err(CloudLinkCodecError::ControlPointForbidden);
        }
        if let Some(model) = &self.model {
            model.validate()?;
        }
        Ok(())
    }
}

/// Optional semantic binding supplied only when commissioning established it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PointModelBinding {
    model_id: String,
    revision: String,
}

impl PointModelBinding {
    /// Creates an explicit commissioned model reference.
    pub fn new(model_id: impl Into<String>, revision: u64) -> Result<Self, CloudLinkCodecError> {
        let value = Self {
            model_id: model_id.into(),
            revision: revision.to_string(),
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), CloudLinkCodecError> {
        identifier(&self.model_id, "samples.model.model_id", 128)?;
        positive_u64(&self.revision, "samples.model.revision")?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum PointKindWire {
    Telemetry,
    Status,
    Command,
    Action,
}

impl From<PointKind> for PointKindWire {
    fn from(value: PointKind) -> Self {
        match value {
            PointKind::Telemetry => Self::Telemetry,
            PointKind::Status => Self::Status,
            PointKind::Command => Self::Command,
            PointKind::Action => Self::Action,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum PointQualityWire {
    Good,
    Uncertain,
    Bad,
    Unavailable,
}

impl From<PointQuality> for PointQualityWire {
    fn from(value: PointQuality) -> Self {
        match value {
            PointQuality::Good => Self::Good,
            PointQuality::Uncertain => Self::Uncertain,
            PointQuality::Bad => Self::Bad,
            PointQuality::Unavailable => Self::Unavailable,
        }
    }
}

/// Bounded business telemetry payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelemetryBatch {
    topology: TopologyBinding,
    samples: Vec<PointFact>,
}

impl TelemetryBatch {
    pub(crate) fn from_samples(
        topology: TopologyBinding,
        samples: &[PointSample],
    ) -> Result<Self, CloudLinkCodecError> {
        if samples.is_empty() {
            return Err(CloudLinkCodecError::InvalidField {
                field: "samples",
                message: "must contain at least one point fact",
            });
        }
        if samples.len() > MAX_POINT_SAMPLES {
            return Err(CloudLinkCodecError::TooManySamples {
                found: samples.len(),
                maximum: MAX_POINT_SAMPLES,
            });
        }
        topology.validate()?;
        let samples = samples
            .iter()
            .copied()
            .map(PointFact::from_sample)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { topology, samples })
    }

    pub(crate) fn validate(&self) -> Result<(), CloudLinkCodecError> {
        if self.samples.is_empty() || self.samples.len() > MAX_POINT_SAMPLES {
            return Err(CloudLinkCodecError::TooManySamples {
                found: self.samples.len(),
                maximum: MAX_POINT_SAMPLES,
            });
        }
        self.topology.validate()?;
        for sample in &self.samples {
            sample.validate()?;
        }
        Ok(())
    }
}
