//! SunSpec model JSON types (mirrors sunspec-json/schema.json).

use serde::Deserialize;

/// Top-level SunSpec model definition.
#[derive(Debug, Clone, Deserialize)]
pub struct SunSpecModel {
    pub id: u16,
    pub group: SunSpecGroup,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub desc: Option<String>,
}

/// A SunSpec group (flat or nested register block).
#[derive(Debug, Clone, Deserialize)]
pub struct SunSpecGroup {
    pub name: String,
    #[serde(rename = "type")]
    pub group_type: String,
    #[serde(default)]
    pub count: SunSpecCount,
    #[serde(default)]
    pub points: Vec<SunSpecPoint>,
    #[serde(default)]
    pub groups: Vec<SunSpecGroup>,
}

/// Repeat count: fixed integer or runtime reference to another point name.
#[derive(Debug, Clone, Default)]
pub enum SunSpecCount {
    #[default]
    One,
    Fixed(u16),
    Ref(String),
}

impl<'de> Deserialize<'de> for SunSpecCount {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        match value {
            serde_json::Value::Number(n) => n
                .as_u64()
                .map(|v| SunSpecCount::Fixed(v as u16))
                .ok_or_else(|| serde::de::Error::custom("invalid count")),
            serde_json::Value::String(s) => Ok(SunSpecCount::Ref(s)),
            serde_json::Value::Null => Ok(SunSpecCount::One),
            _ => Err(serde::de::Error::custom("invalid count")),
        }
    }
}

impl SunSpecCount {
    pub fn as_fixed(&self) -> Option<u16> {
        match self {
            Self::One => Some(1),
            Self::Fixed(n) => Some(*n),
            Self::Ref(_) => None,
        }
    }
}

/// A single SunSpec point within a model.
#[derive(Debug, Clone, Deserialize)]
pub struct SunSpecPoint {
    pub name: String,
    #[serde(rename = "type")]
    pub point_type: String,
    pub size: u16,
    #[serde(default)]
    pub value: Option<serde_json::Value>,
    #[serde(default)]
    pub sf: Option<String>,
    #[serde(default)]
    pub units: Option<String>,
    #[serde(default)]
    pub access: Option<String>,
    #[serde(default)]
    pub mandatory: Option<String>,
    #[serde(default)]
    pub r#static: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub desc: Option<String>,
}
