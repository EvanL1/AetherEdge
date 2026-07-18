//! Allocation-bounded validation shared by decoding and domain projection.

use crate::error::{
    ContractResult, IntegrationContractError, IntegrationContractErrorCode as Code,
};

pub(crate) const MAX_AREAS: usize = 4_096;
pub(crate) const MAX_DEVICES: usize = 16_384;
pub(crate) const MAX_ENTITIES: usize = 65_536;
pub(crate) const MAX_POINTS_PER_ENTITY: usize = 64;
pub(crate) const MAX_OBSERVATIONS: usize = 65_536;
pub(crate) const MAX_IDENTIFIER_CHARS: usize = 128;
pub(crate) const MAX_TITLE_CHARS: usize = 256;
pub(crate) const MAX_UNIT_CHARS: usize = 32;
pub(crate) const MAX_VERSION_CHARS: usize = 128;
pub(crate) const MAX_DIAGNOSTIC_CHARS: usize = 512;
pub(crate) const MAX_STRING_VALUE_CHARS: usize = 4_096;
pub(crate) const MAX_DECIMAL_CHARS: usize = 96;
pub(crate) const MAX_BASE64URL_CHARS: usize = 16_384;

const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
const BASE64URL_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

pub(crate) fn identifier(value: &str) -> ContractResult<()> {
    if value.is_empty()
        || value.len() > MAX_IDENTIFIER_CHARS
        || !value
            .bytes()
            .enumerate()
            .all(|(index, byte)| identifier_byte(index, byte))
    {
        return Err(IntegrationContractError::new(
            Code::IdentifierInvalid,
            "public integration identifier is invalid",
        ));
    }
    Ok(())
}

const fn identifier_byte(index: usize, byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || (index > 0 && matches!(byte, b'.' | b'_' | b':' | b'-'))
}

pub(crate) fn text_bound(value: &str, maximum: usize, message: &'static str) -> ContractResult<()> {
    let length = value.chars().count();
    if length == 0 || length > maximum {
        return Err(IntegrationContractError::new(Code::FieldBound, message));
    }
    Ok(())
}

pub(crate) fn display_or_evidence_text(
    value: &str,
    maximum: usize,
    bound_message: &'static str,
) -> ContractResult<()> {
    if value.chars().count() > maximum {
        return Err(IntegrationContractError::new(
            Code::FieldBound,
            bound_message,
        ));
    }
    let contains_forbidden_control = value
        .chars()
        .any(|character| matches!(character, '\u{0000}'..='\u{001f}' | '\u{007f}'));
    if value.is_empty()
        || contains_forbidden_control
        || !value.chars().any(|character| !character.is_whitespace())
    {
        return Err(IntegrationContractError::new(
            Code::TextInvalid,
            "display or evidence text is invalid",
        ));
    }
    Ok(())
}

pub(crate) fn canonical_u64(value: &str) -> ContractResult<u64> {
    if value.len() > 20 {
        return Err(IntegrationContractError::new(
            Code::IntegerOutOfRange,
            "uint64 exceeds its wire bound",
        ));
    }
    if value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return Err(IntegrationContractError::new(
            Code::IntegerNonCanonical,
            "uint64 is not canonical",
        ));
    }
    value.parse::<u64>().map_err(|_source| {
        IntegrationContractError::new(Code::IntegerOutOfRange, "uint64 is out of range")
    })
}

pub(crate) fn canonical_i64(value: &str) -> ContractResult<i64> {
    if value.len() > 20 {
        return Err(IntegrationContractError::new(
            Code::IntegerOutOfRange,
            "int64 exceeds its wire bound",
        ));
    }
    let unsigned = value.strip_prefix('-').unwrap_or(value);
    if unsigned.is_empty()
        || !unsigned.bytes().all(|byte| byte.is_ascii_digit())
        || (unsigned.len() > 1 && unsigned.starts_with('0'))
        || value == "-0"
        || value.starts_with('+')
    {
        return Err(IntegrationContractError::new(
            Code::IntegerNonCanonical,
            "int64 is not canonical",
        ));
    }
    value.parse::<i64>().map_err(|_source| {
        IntegrationContractError::new(Code::IntegerOutOfRange, "int64 is out of range")
    })
}

pub(crate) fn canonical_decimal(value: &str) -> ContractResult<()> {
    if value.len() > MAX_DECIMAL_CHARS {
        return Err(IntegrationContractError::new(
            Code::FieldBound,
            "decimal exceeds 96 characters",
        ));
    }
    if value.is_empty() {
        return Err(IntegrationContractError::new(
            Code::ValueEncodingInvalid,
            "decimal is empty",
        ));
    }
    if value == "0" {
        return Ok(());
    }

    let unsigned = value.strip_prefix('-').unwrap_or(value);
    let (integer, fractional) = match unsigned.split_once('.') {
        Some((integer, fractional)) => (integer, Some(fractional)),
        None => (unsigned, None),
    };
    let integer_is_canonical = !integer.is_empty()
        && integer.bytes().all(|byte| byte.is_ascii_digit())
        && (integer == "0" || !integer.starts_with('0'));
    let fraction_is_canonical = fractional.is_none_or(|fraction| {
        !fraction.is_empty()
            && fraction.bytes().all(|byte| byte.is_ascii_digit())
            && !fraction.ends_with('0')
    });
    let is_negative_zero = value.starts_with('-')
        && integer == "0"
        && fractional.is_none_or(|fraction| fraction.bytes().all(|byte| byte == b'0'));
    if !integer_is_canonical
        || !fraction_is_canonical
        || is_negative_zero
        || unsigned.matches('.').count() > 1
    {
        return Err(IntegrationContractError::new(
            Code::ValueEncodingInvalid,
            "decimal is not canonically encoded",
        ));
    }
    Ok(())
}

pub(crate) fn canonical_base64url(value: &str) -> ContractResult<()> {
    if value.len() > MAX_BASE64URL_CHARS {
        return Err(IntegrationContractError::new(
            Code::FieldBound,
            "Base64url exceeds 16384 characters",
        ));
    }
    if value.len() % 4 == 1 || !value.bytes().all(|byte| base64url_index(byte).is_some()) {
        return Err(IntegrationContractError::new(
            Code::ValueEncodingInvalid,
            "bytes are not canonical Base64url",
        ));
    }

    let trailing_bits_are_zero = match value.len() % 4 {
        0 => true,
        2 => value
            .as_bytes()
            .last()
            .and_then(|byte| base64url_index(*byte))
            .is_some_and(|index| index & 0x0f == 0),
        3 => value
            .as_bytes()
            .last()
            .and_then(|byte| base64url_index(*byte))
            .is_some_and(|index| index & 0x03 == 0),
        _ => false,
    };
    if !trailing_bits_are_zero {
        return Err(IntegrationContractError::new(
            Code::ValueEncodingInvalid,
            "Base64url has non-zero trailing bits",
        ));
    }
    Ok(())
}

const fn base64url_index(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'-' => Some(62),
        b'_' => Some(63),
        _ => None,
    }
}

pub(crate) fn encode_base64url(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        encoded.push(char::from(BASE64URL_ALPHABET[usize::from(first >> 2)]));
        let second_index = (first & 0x03) << 4 | chunk.get(1).copied().unwrap_or(0) >> 4;
        encoded.push(char::from(BASE64URL_ALPHABET[usize::from(second_index)]));
        if let Some(second) = chunk.get(1).copied() {
            let third_index = (second & 0x0f) << 2 | chunk.get(2).copied().unwrap_or(0) >> 6;
            encoded.push(char::from(BASE64URL_ALPHABET[usize::from(third_index)]));
        }
        if let Some(third) = chunk.get(2).copied() {
            encoded.push(char::from(BASE64URL_ALPHABET[usize::from(third & 0x3f)]));
        }
    }
    encoded
}

pub(crate) fn foundation_float64(value: f64) -> ContractResult<()> {
    if !value.is_finite() {
        return Err(IntegrationContractError::new(
            Code::JsonNonFiniteNumber,
            "float64 must be finite",
        ));
    }
    if value.fract() == 0.0 && value.abs() > MAX_SAFE_INTEGER {
        let canonical = serde_json_canonicalizer::to_string(&value).map_err(|_source| {
            IntegrationContractError::new(Code::JsonUnsafeNumber, "float64 cannot be canonicalized")
        })?;
        if !canonical.contains('.') {
            return Err(IntegrationContractError::new(
                Code::JsonUnsafeNumber,
                "integer-valued float64 is outside Foundation safe semantics",
            ));
        }
    }
    Ok(())
}
