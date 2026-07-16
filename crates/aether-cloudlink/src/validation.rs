//! Shared closed-field validation helpers.

use crate::{CLOUDLINK_PROTOCOL_VERSION, CloudLinkCodecError};

pub(crate) fn protocol_version(value: &str) -> Result<(), CloudLinkCodecError> {
    if value == CLOUDLINK_PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(CloudLinkCodecError::UnsupportedProtocolVersion {
            found: value.to_string(),
            supported: CLOUDLINK_PROTOCOL_VERSION,
        })
    }
}

pub(crate) fn canonical_u64(value: &str, field: &'static str) -> Result<u64, CloudLinkCodecError> {
    let canonical = value == "0"
        || (!value.starts_with('0')
            && !value.is_empty()
            && value.bytes().all(|byte| byte.is_ascii_digit()));
    if !canonical {
        return Err(CloudLinkCodecError::InvalidCanonicalUint64 { field });
    }
    value
        .parse()
        .map_err(|_| CloudLinkCodecError::IntegerOutOfRange { field })
}

pub(crate) fn positive_u64(value: &str, field: &'static str) -> Result<u64, CloudLinkCodecError> {
    let parsed = canonical_u64(value, field)?;
    if parsed == 0 {
        Err(CloudLinkCodecError::InvalidField {
            field,
            message: "must be greater than zero",
        })
    } else {
        Ok(parsed)
    }
}

pub(crate) fn identifier(
    value: &str,
    field: &'static str,
    maximum: usize,
) -> Result<(), CloudLinkCodecError> {
    let valid = !value.is_empty()
        && value.len() <= maximum
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(CloudLinkCodecError::InvalidField {
            field,
            message: "must be a bounded transport-safe identifier",
        })
    }
}

pub(crate) fn uuid(value: &str, field: &'static str) -> Result<(), CloudLinkCodecError> {
    let bytes = value.as_bytes();
    let valid = bytes.len() == 36
        && bytes[8] == b'-'
        && bytes[13] == b'-'
        && bytes[18] == b'-'
        && bytes[23] == b'-'
        && matches!(bytes[14], b'1'..=b'8')
        && matches!(bytes[19], b'8' | b'9' | b'a' | b'b')
        && bytes.iter().enumerate().all(|(index, byte)| {
            matches!(index, 8 | 13 | 18 | 23)
                || (byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        });
    if valid {
        Ok(())
    } else {
        Err(CloudLinkCodecError::InvalidField {
            field,
            message: "must be a canonical lowercase UUID",
        })
    }
}

pub(crate) fn digest(value: &str, field: &'static str) -> Result<(), CloudLinkCodecError> {
    let valid = value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase());
    if valid {
        Ok(())
    } else {
        Err(CloudLinkCodecError::InvalidField {
            field,
            message: "must be sha256 followed by 64 lowercase hexadecimal digits",
        })
    }
}

pub(crate) fn topology_digest(value: &str) -> Result<(), CloudLinkCodecError> {
    let fx64 = value.len() == 21
        && value.starts_with("fx64:")
        && value[5..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase());
    if fx64 {
        Ok(())
    } else {
        digest(value, "topology.snapshot_digest")
    }
}

pub(crate) fn traceparent(value: &str) -> Result<(), CloudLinkCodecError> {
    let bytes = value.as_bytes();
    let valid = bytes.len() == 55
        && bytes[2] == b'-'
        && bytes[35] == b'-'
        && bytes[52] == b'-'
        && bytes.iter().enumerate().all(|(index, byte)| {
            matches!(index, 2 | 35 | 52) || (byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        })
        && &value[0..2] != "ff"
        && value[3..35] != *"00000000000000000000000000000000"
        && value[36..52] != *"0000000000000000";
    if valid {
        Ok(())
    } else {
        Err(CloudLinkCodecError::InvalidField {
            field: "traceparent",
            message: "must be a strict 55-byte W3C traceparent",
        })
    }
}

pub(crate) fn schema(value: &str, expected: &'static str) -> Result<(), CloudLinkCodecError> {
    if value == expected {
        Ok(())
    } else {
        Err(CloudLinkCodecError::UnsupportedMessage {
            found: value.to_string(),
        })
    }
}
