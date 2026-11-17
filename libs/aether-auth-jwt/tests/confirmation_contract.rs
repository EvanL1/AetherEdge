//! Every transport must agree on what counts as an explicit confirmation.
//!
//! Before this contract existed the same header was read by seven call sites
//! with three different meanings: two services trimmed and ignored case, the
//! gateway compared the first value exactly, and one route rejected anything
//! that was not a single literal boolean. These tests pin the one reading.

use aether_auth_jwt::{
    AccessTokenAuthenticator, CONFIRMATION_HEADER, Confirmation, MalformedConfirmation,
};
use aether_domain::TimestampMs;
use http::{HeaderMap, HeaderValue};

const SECRET: &str = "test-only-jwt-secret-32-bytes-minimum";

fn parse(values: &[&[u8]]) -> Result<Confirmation, MalformedConfirmation> {
    Confirmation::from_header_values(values.iter().copied())
}

#[test]
fn the_wire_name_is_stable() {
    assert_eq!(CONFIRMATION_HEADER, "x-aether-confirmed");
}

#[test]
fn a_missing_header_is_not_a_confirmation() {
    assert_eq!(parse(&[]), Ok(Confirmation::NotConfirmed));
}

#[test]
fn exactly_one_literal_true_confirms() {
    assert_eq!(parse(&[b"true"]), Ok(Confirmation::Confirmed));
}

#[test]
fn exactly_one_literal_false_declines() {
    assert_eq!(parse(&[b"false"]), Ok(Confirmation::NotConfirmed));
}

#[test]
fn near_misses_are_malformed_rather_than_confirmed() {
    for value in [
        &b"TRUE"[..],
        b"True",
        b" true",
        b"true ",
        b"yes",
        b"1",
        b"",
        b"\xfftrue",
    ] {
        assert_eq!(
            parse(&[value]),
            Err(MalformedConfirmation),
            "{:?} must not be read as a decision",
            String::from_utf8_lossy(value)
        );
    }
}

#[test]
fn repeated_headers_are_malformed_even_when_they_agree() {
    assert_eq!(parse(&[b"true", b"true"]), Err(MalformedConfirmation));
    assert_eq!(parse(&[b"true", b"false"]), Err(MalformedConfirmation));
    assert_eq!(parse(&[b"false", b"true"]), Err(MalformedConfirmation));
}

fn headers(values: &[&'static str]) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for value in values {
        headers.append(CONFIRMATION_HEADER, HeaderValue::from_static(value));
    }
    headers
}

#[test]
fn an_http_header_map_is_read_with_the_same_rule() {
    assert_eq!(
        Confirmation::from_headers(&headers(&[])),
        Ok(Confirmation::NotConfirmed)
    );
    assert_eq!(
        Confirmation::from_headers(&headers(&["true"])),
        Ok(Confirmation::Confirmed)
    );
    assert_eq!(
        Confirmation::from_headers(&headers(&["false"])),
        Ok(Confirmation::NotConfirmed)
    );
    for malformed in [
        &["true", "true"][..],
        &["true", "false"],
        &["true, true"],
        &[" true"],
        &["TRUE"],
        &["yes"],
    ] {
        assert_eq!(
            Confirmation::from_headers(&headers(malformed)),
            Err(MalformedConfirmation),
            "{malformed:?}"
        );
    }
}

#[test]
fn the_malformed_error_tells_the_caller_which_header_and_what_is_accepted() {
    let message = MalformedConfirmation.to_string();

    assert!(message.contains(CONFIRMATION_HEADER), "{message}");
    assert!(
        message.contains("`true`") && message.contains("`false`"),
        "{message}"
    );
}

#[test]
fn an_interface_can_name_the_header_without_declaring_it() {
    assert_eq!(Confirmation::header_name().as_str(), CONFIRMATION_HEADER);
}

#[test]
fn an_invocation_carries_the_parsed_decision_into_the_request_context() {
    let authenticator = AccessTokenAuthenticator::new(SECRET).expect("valid secret");
    let timestamp = TimestampMs::new(1);

    let confirmed = authenticator.invocation(None, None, Confirmation::Confirmed, timestamp);
    let declined = authenticator.invocation(None, None, Confirmation::NotConfirmed, timestamp);

    assert!(confirmed.context().confirmed());
    assert!(!declined.context().confirmed());
}
