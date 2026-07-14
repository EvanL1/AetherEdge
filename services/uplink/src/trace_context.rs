//! W3C trace-context propagation across the cloud↔gateway boundary (ADR-0016).
//!
//! A cloud-issued `read`/`write`/`call-*` request fans out across loopback
//! services before it reaches a device. Without a correlation identifier the
//! cloud can only observe "I asked, and 8 seconds later I got an answer" — it
//! cannot attribute that latency to a hop. Carrying the caller's `traceparent`
//! through the envelope and onto the loopback hop lets it.
//!
//! The gateway does not mint trace ids and does not export spans. It only
//! preserves causality a caller established.
//!
//! The value arrives from the network and is forwarded into an HTTP header, so
//! it is parsed strictly rather than passed through: an unvalidated string
//! containing CR/LF would be header injection on the loopback hop.

use serde::{Deserialize, Serialize};

/// A validated W3C `traceparent`.
///
/// Format: `{version}-{trace-id}-{parent-id}-{flags}`, e.g.
/// `00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct TraceParent(String);

impl TraceParent {
    /// Parses a `traceparent`, returning `None` for anything malformed.
    ///
    /// Rejecting is always safe: a dropped trace context costs an unlinked
    /// span, while a forged one costs a corrupted header.
    pub fn parse(raw: &str) -> Option<Self> {
        let mut fields = raw.split('-');
        let version = fields.next()?;
        let trace_id = fields.next()?;
        let parent_id = fields.next()?;
        let flags = fields.next()?;
        if fields.next().is_some() {
            return None;
        }

        // Lowercase hex only, per the W3C field-value grammar. This is also what
        // makes CR/LF, spaces, and header separators unrepresentable.
        let hex_of_len = |field: &str, len: usize| {
            field.len() == len
                && field
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        if !hex_of_len(version, 2)
            || !hex_of_len(trace_id, 32)
            || !hex_of_len(parent_id, 16)
            || !hex_of_len(flags, 2)
        {
            return None;
        }

        // `ff` is forbidden by the spec, and an all-zero id is the "invalid"
        // sentinel — propagating either would link a span to nothing.
        if version == "ff"
            || trace_id.bytes().all(|b| b == b'0')
            || parent_id.bytes().all(|b| b == b'0')
        {
            return None;
        }

        Some(Self(raw.to_string()))
    }

    /// Returns the validated header value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Deserializes an envelope field, dropping a malformed value instead of failing.
///
/// A cloud that sends a broken `traceparent` must still get its command executed:
/// rejecting the whole body would make observability an availability dependency
/// of the control path. Use as
/// `#[serde(default, deserialize_with = "trace_context::deserialize_optional")]`.
pub fn deserialize_optional<'de, D>(deserializer: D) -> Result<Option<TraceParent>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(raw
        .as_ref()
        .and_then(serde_json::Value::as_str)
        .and_then(TraceParent::parse))
}

/// Extracts a `traceparent` from an arbitrary inbound JSON body.
///
/// `call-data` and `call-alarm` have no request struct — their handlers pluck
/// fields off a raw `Value` — so they need this rather than a typed field.
pub fn from_json(body: &serde_json::Value) -> Option<TraceParent> {
    TraceParent::parse(body.get("traceparent")?.as_str()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";

    #[test]
    fn well_formed_traceparent_round_trips() {
        let parsed = TraceParent::parse(VALID).expect("valid traceparent");
        assert_eq!(parsed.as_str(), VALID);
    }

    /// The reason this parser exists. The value reaches an HTTP header on the
    /// loopback hop to automation; a CR/LF that survived parsing would let a
    /// cloud-side caller inject arbitrary headers into an authenticated,
    /// control-plane request.
    #[test]
    fn crlf_injection_is_rejected() {
        let attack = format!("{VALID}\r\nauthorization: AetherService forged");
        assert_eq!(TraceParent::parse(&attack), None);
        assert_eq!(TraceParent::parse("00-4bf9\r\n2f35-00f0-01"), None);
    }

    #[test]
    fn non_hex_and_whitespace_are_rejected() {
        assert_eq!(
            TraceParent::parse("00-4bf92f3577b34da6a3ce929d0e0e473g-00f067aa0ba902b7-01"),
            None
        );
        assert_eq!(
            TraceParent::parse("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01 "),
            None
        );
        // Uppercase is outside the W3C grammar; accepting it would let two
        // spellings of one id fail to join in a consumer.
        assert_eq!(
            TraceParent::parse("00-4BF92F3577B34DA6A3CE929D0E0E4736-00f067aa0ba902b7-01"),
            None
        );
    }

    #[test]
    fn wrong_field_lengths_and_arity_are_rejected() {
        assert_eq!(TraceParent::parse("00-4bf92f35-00f067aa0ba902b7-01"), None);
        assert_eq!(
            TraceParent::parse("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa-01"),
            None
        );
        assert_eq!(TraceParent::parse(&format!("{VALID}-extra")), None);
        assert_eq!(
            TraceParent::parse("00-4bf92f3577b34da6a3ce929d0e0e4736"),
            None
        );
        assert_eq!(TraceParent::parse(""), None);
    }

    #[test]
    fn all_zero_ids_and_forbidden_version_are_rejected() {
        assert_eq!(
            TraceParent::parse("00-00000000000000000000000000000000-00f067aa0ba902b7-01"),
            None
        );
        assert_eq!(
            TraceParent::parse("00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01"),
            None
        );
        assert_eq!(
            TraceParent::parse("ff-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"),
            None
        );
    }

    #[test]
    fn extracted_from_a_raw_json_body() {
        let body = serde_json::json!({"msgId": "m-1", "traceparent": VALID});
        assert_eq!(from_json(&body).expect("present").as_str(), VALID);

        assert_eq!(from_json(&serde_json::json!({"msgId": "m-1"})), None);
        assert_eq!(
            from_json(&serde_json::json!({"traceparent": "garbage"})),
            None
        );
        assert_eq!(from_json(&serde_json::json!({"traceparent": 42})), None);
    }

    /// A cloud that sends a broken traceparent must still get its command run.
    /// Observability is not allowed to become an availability dependency, so the
    /// field is dropped rather than failing the whole request body.
    #[test]
    fn a_malformed_value_drops_the_field_without_failing_the_request() {
        #[derive(Deserialize)]
        struct Request {
            #[serde(default, deserialize_with = "deserialize_optional")]
            traceparent: Option<TraceParent>,
            msg_id: String,
        }

        let request: Request = serde_json::from_value(serde_json::json!({
            "traceparent": "not-a-traceparent",
            "msg_id": "m-1"
        }))
        .expect("body still parses");

        assert_eq!(request.traceparent, None);
        assert_eq!(request.msg_id, "m-1");
    }
}
