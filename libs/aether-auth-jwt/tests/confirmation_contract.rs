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

const SECRET: &str = "test-only-jwt-secret-32-bytes-minimum";

fn parse(values: &[&[u8]]) -> Result<Confirmation, MalformedConfirmation> {
    Confirmation::from_header_values(values.iter().copied())
}

#[test]
fn the_header_name_is_published_once() {
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

#[test]
fn the_fail_closed_reading_never_turns_malformed_input_into_confirmation() {
    let fail_closed =
        |values: &[&[u8]]| Confirmation::from_header_values_fail_closed(values.iter().copied());

    assert_eq!(fail_closed(&[b"true"]), Confirmation::Confirmed);
    assert_eq!(fail_closed(&[]), Confirmation::NotConfirmed);
    assert_eq!(fail_closed(&[b"false"]), Confirmation::NotConfirmed);
    assert_eq!(fail_closed(&[b" TRUE "]), Confirmation::NotConfirmed);
    assert_eq!(fail_closed(&[b"true", b"true"]), Confirmation::NotConfirmed);
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
