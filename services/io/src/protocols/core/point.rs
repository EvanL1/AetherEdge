//! Protocol-neutral point configuration and value transformation primitives.

use aether_core::PointType;
use serde::{Deserialize, Serialize};

use crate::protocols::core::error::GatewayError;

/// Point configuration with an adapter-owned address and SCADA type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PointConfig<A> {
    pub id: u32,
    pub point_type: PointType,
    pub address: A,
    #[serde(default)]
    pub transform: TransformConfig,
}

impl<A> PointConfig<A> {
    pub fn new(id: u32, point_type: PointType, address: A) -> Self {
        Self {
            id,
            point_type,
            address,
            transform: TransformConfig::default(),
        }
    }

    pub fn telemetry(id: u32, address: A) -> Self {
        Self::new(id, PointType::Telemetry, address)
    }

    pub fn signal(id: u32, address: A) -> Self {
        Self::new(id, PointType::Signal, address)
    }

    pub fn control(id: u32, address: A) -> Self {
        Self::new(id, PointType::Control, address)
    }

    pub fn adjustment(id: u32, address: A) -> Self {
        Self::new(id, PointType::Adjustment, address)
    }

    #[must_use]
    pub fn with_transform(mut self, transform: TransformConfig) -> Self {
        self.transform = transform;
        self
    }
}

/// Data format for protocol values (case-insensitive deserialization).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub enum DataFormat {
    Bool,
    #[default]
    UInt16,
    Int16,
    UInt32,
    Int32,
    UInt64,
    Int64,
    Float32,
    Float64,
    String,
}

impl DataFormat {
    pub fn register_count(&self) -> u16 {
        match self {
            Self::Bool | Self::UInt16 | Self::Int16 => 1,
            Self::UInt32 | Self::Int32 | Self::Float32 => 2,
            Self::UInt64 | Self::Int64 | Self::Float64 => 4,
            Self::String => 8, // Default 16 characters
        }
    }
}

impl<'de> Deserialize<'de> for DataFormat {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct DataFormatVisitor;

        impl<'de> serde::de::Visitor<'de> for DataFormatVisitor {
            type Value = DataFormat;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a data format string like 'int32', 'uint16', 'float32', etc.")
            }

            fn visit_str<E>(self, value: &str) -> Result<DataFormat, E>
            where
                E: serde::de::Error,
            {
                match value.to_lowercase().as_str() {
                    "bool" | "boolean" => Ok(DataFormat::Bool),
                    "uint16" | "u16" => Ok(DataFormat::UInt16),
                    "int16" | "i16" => Ok(DataFormat::Int16),
                    "uint32" | "u32" => Ok(DataFormat::UInt32),
                    "int32" | "i32" => Ok(DataFormat::Int32),
                    "uint64" | "u64" => Ok(DataFormat::UInt64),
                    "int64" | "i64" => Ok(DataFormat::Int64),
                    "float32" | "f32" | "float" => Ok(DataFormat::Float32),
                    "float64" | "f64" | "double" => Ok(DataFormat::Float64),
                    "string" => Ok(DataFormat::String),
                    _ => Err(serde::de::Error::unknown_variant(
                        value,
                        &[
                            "bool", "uint16", "int16", "uint32", "int32", "uint64", "int64",
                            "float32", "float64", "string",
                        ],
                    )),
                }
            }
        }

        deserializer.deserialize_str(DataFormatVisitor)
    }
}

/// Byte order for multi-byte values (supports serde aliases: BE/LE/WORD_SWAP/BYTE_SWAP).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum ByteOrder {
    #[default]
    #[serde(alias = "big_endian", alias = "BIG_ENDIAN", alias = "BE")]
    Abcd,
    #[serde(alias = "little_endian", alias = "LITTLE_ENDIAN", alias = "LE")]
    Dcba,
    #[serde(alias = "WORD_SWAP", alias = "word_swap")]
    Badc,
    #[serde(alias = "BYTE_SWAP", alias = "byte_swap")]
    Cdab,
}

impl ByteOrder {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Abcd => "ABCD",
            Self::Dcba => "DCBA",
            Self::Badc => "BADC",
            Self::Cdab => "CDAB",
        }
    }
}

/// Data transformation: result = raw * scale + offset.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TransformConfig {
    #[serde(default = "default_scale")]
    pub scale: f64,
    #[serde(default)]
    pub offset: f64,
    #[serde(default)]
    pub reverse: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadband: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_value: Option<f64>,
}

fn default_scale() -> f64 {
    1.0
}

/// Must stay the identity transform and agree with the serde field defaults:
/// a derived `Default` would set `scale = 0.0` and silently zero every value
/// built through `..TransformConfig::default()`.
impl Default for TransformConfig {
    fn default() -> Self {
        Self {
            scale: default_scale(),
            offset: 0.0,
            reverse: false,
            deadband: None,
            min_value: None,
            max_value: None,
        }
    }
}

impl TransformConfig {
    pub fn linear(scale: f64, offset: f64) -> Self {
        Self {
            scale,
            offset,
            ..Default::default()
        }
    }

    pub fn apply(&self, raw: f64) -> f64 {
        raw * self.scale + self.offset
    }

    pub fn reverse_apply(&self, value: f64) -> Result<f64, GatewayError> {
        if self.scale == 0.0 {
            return Err(GatewayError::DataConversion(
                "Cannot reverse transform: scale is zero".into(),
            ));
        }
        Ok((value - self.offset) / self.scale)
    }

    pub fn apply_bool(&self, raw: bool) -> bool {
        if self.reverse { !raw } else { raw }
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // unwrap in tests
mod tests {
    use super::*;

    #[test]
    fn test_transform() {
        let t = TransformConfig::linear(0.1, 10.0);
        assert_eq!(t.apply(100.0), 20.0); // 100 * 0.1 + 10 = 20
        assert_eq!(t.reverse_apply(20.0).unwrap(), 100.0);
    }

    #[test]
    fn default_transform_is_the_identity_and_matches_serde_defaults() {
        let constructed = TransformConfig::default();
        assert_eq!(constructed.apply(42.5), 42.5);

        let deserialized: TransformConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(deserialized.scale, constructed.scale);
        assert_eq!(deserialized.offset, constructed.offset);
        assert_eq!(deserialized.reverse, constructed.reverse);
    }

    #[test]
    fn test_transform_zero_scale() {
        let t = TransformConfig::linear(0.0, 10.0);
        assert!(t.reverse_apply(20.0).is_err());
    }

    #[test]
    fn test_data_format_register_count() {
        assert_eq!(DataFormat::UInt16.register_count(), 1);
        assert_eq!(DataFormat::Float32.register_count(), 2);
        assert_eq!(DataFormat::Float64.register_count(), 4);
    }

    #[test]
    fn test_data_format_case_insensitive() {
        let formats = vec![
            ("\"int32\"", DataFormat::Int32),
            ("\"Int32\"", DataFormat::Int32),
            ("\"INT32\"", DataFormat::Int32),
            ("\"i32\"", DataFormat::Int32),
            ("\"float32\"", DataFormat::Float32),
            ("\"Float32\"", DataFormat::Float32),
        ];

        for (json, expected) in formats {
            let result: DataFormat = serde_json::from_str(json).unwrap();
            assert_eq!(result, expected, "Failed for {}", json);
        }
    }
}
