//! Small closed-contract validators shared by wire and local types.

use crate::error::{ControlResult, IntegrationControlError, IntegrationControlErrorCode};

pub(crate) fn invalid(message: &'static str) -> IntegrationControlError {
    IntegrationControlError::new(IntegrationControlErrorCode::InvalidMessage, message)
}

pub(crate) fn exact(found: &str, expected: &str, message: &'static str) -> ControlResult<()> {
    if found == expected {
        Ok(())
    } else {
        Err(invalid(message))
    }
}

pub(crate) fn canonical_u64(value: &str, positive: bool) -> ControlResult<u64> {
    let parsed = value
        .parse::<u64>()
        .map_err(|_source| invalid("protocol integer is outside uint64"))?;
    if parsed.to_string() != value || (positive && parsed == 0) {
        return Err(invalid("protocol integer is not canonical"));
    }
    Ok(parsed)
}

pub(crate) fn identifier(value: &str) -> ControlResult<()> {
    let mut bytes = value.bytes();
    let first = bytes.next().ok_or_else(|| invalid("identifier is empty"))?;
    if value.len() > 128
        || !first.is_ascii_alphanumeric()
        || !bytes
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        return Err(invalid(
            "identifier is outside the frozen alphabet or bound",
        ));
    }
    Ok(())
}

pub(crate) fn uuid(value: &str) -> ControlResult<()> {
    let bytes = value.as_bytes();
    if bytes.len() != 36
        || bytes[8] != b'-'
        || bytes[13] != b'-'
        || bytes[18] != b'-'
        || bytes[23] != b'-'
        || !matches!(bytes[14], b'1'..=b'8')
        || !matches!(bytes[19], b'8' | b'9' | b'a' | b'b')
        || !bytes.iter().enumerate().all(|(index, byte)| {
            matches!(index, 8 | 13 | 18 | 23)
                || (byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        })
    {
        return Err(invalid("UUID is not canonical lowercase RFC 4122 text"));
    }
    Ok(())
}

pub(crate) fn digest(value: &str) -> ControlResult<()> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(invalid("digest must use sha256"));
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid("digest is not canonical lowercase SHA-256"));
    }
    Ok(())
}

pub(crate) fn signature(value: &str) -> ControlResult<()> {
    if value.len() != 86
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(invalid(
            "signature is not canonical unpadded Base64url Ed25519 material",
        ));
    }
    Ok(())
}

pub(crate) fn failure_code(value: &str) -> ControlResult<()> {
    let mut bytes = value.bytes();
    let first = bytes
        .next()
        .ok_or_else(|| invalid("failure code is empty"))?;
    if !first.is_ascii_uppercase()
        || !bytes.all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(invalid("failure code is not canonical"));
    }
    Ok(())
}
