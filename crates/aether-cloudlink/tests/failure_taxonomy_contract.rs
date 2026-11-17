//! Every failure code this product can emit is one the pinned contract
//! release defines.
//!
//! The codes used to be free-form strings in a hand-written `match`, and
//! three of them (`INVALID_JSON`, `UNSUPPORTED_MESSAGE`, `IDENTIFIER_INVALID`)
//! were not in the AetherContracts failure taxonomy at all, so a consumer
//! switching on the contractual identifiers could not recognise them. The
//! codes are now enums, and these tests bind every variant to the taxonomy
//! file the consumer lock pins.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use aether_cloudlink::{CloudLinkCodec, CloudLinkFailure, CloudLinkFailureCode};
use aether_integration_contract::IntegrationContractErrorCode;
use serde_json::Value;

/// Scopes that describe the release tooling or a language binding's own API,
/// never a failure of a message on the wire.
const NON_WIRE_SCOPES: &[&str] = &[
    "distribution",
    "distribution-tool",
    "binding-api-misuse-only",
    "binding-lookup",
];

/// Reads `code -> scope` from the taxonomy file named by the consumer lock, so
/// the test follows the pin instead of hard-coding a release directory.
fn pinned_taxonomy() -> HashMap<String, String> {
    let repository_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let lock: Value = serde_json::from_slice(
        &fs::read(repository_root.join("aether-contracts.lock.json")).expect("consumer lock"),
    )
    .expect("consumer lock JSON");
    let destination = lock["imports"]
        .as_array()
        .expect("lock imports")
        .iter()
        .find(|import| import["source"] == "compatibility/failure-codes.json")
        .and_then(|import| import["destination"].as_str())
        .expect("the lock must import the failure taxonomy");
    let taxonomy: Value = serde_json::from_slice(
        &fs::read(repository_root.join(destination)).expect("pinned failure taxonomy"),
    )
    .expect("failure taxonomy JSON");

    taxonomy["failures"]
        .as_array()
        .expect("taxonomy failures")
        .iter()
        .map(|failure| {
            (
                failure["code"].as_str().expect("failure code").to_owned(),
                failure["scope"].as_str().expect("failure scope").to_owned(),
            )
        })
        .collect()
}

fn assert_contractual(taxonomy: &HashMap<String, String>, origin: &str, code: &str) {
    let scope = taxonomy.get(code).unwrap_or_else(|| {
        panic!("{origin} emits `{code}`, which the pinned failure taxonomy does not define")
    });
    assert!(
        !NON_WIRE_SCOPES.contains(&scope.as_str()),
        "{origin} emits `{code}`, whose taxonomy scope `{scope}` is not a wire failure"
    );
}

#[test]
fn every_cloudlink_failure_code_is_defined_by_the_pinned_taxonomy() {
    let taxonomy = pinned_taxonomy();

    assert!(!CloudLinkFailureCode::ALL.is_empty());
    for code in CloudLinkFailureCode::ALL {
        assert_contractual(&taxonomy, "aether-cloudlink", code.as_str());
    }
}

#[test]
fn every_integration_contract_failure_code_is_defined_by_the_pinned_taxonomy() {
    let taxonomy = pinned_taxonomy();

    assert!(!IntegrationContractErrorCode::ALL.is_empty());
    for code in IntegrationContractErrorCode::ALL {
        assert_contractual(&taxonomy, "aether-integration-contract", code.as_str());
    }
}

#[test]
fn malformed_json_is_reported_as_a_syntax_error() {
    for bytes in [&b"{not json"[..], b"", b"[1, 2", b"{\"schema\": }"] {
        let error = CloudLinkCodec::decode(bytes).expect_err("malformed JSON must not decode");
        assert_eq!(
            error.failure_code(),
            "JSON_SYNTAX_ERROR",
            "{:?}",
            String::from_utf8_lossy(bytes)
        );
    }
}

#[test]
fn a_message_of_an_unknown_kind_is_reported_as_an_unsupported_version() {
    // The contract's reference binding reports an unknown schema or kind as
    // `UNSUPPORTED_VERSION`; there is no separate "unsupported message" code.
    let error = CloudLinkCodec::decode(
        br#"{"schema":"aether.cloudlink.v9.from-the-future","kind":"teleport"}"#,
    )
    .expect_err("an unknown message kind must not decode");

    assert_eq!(error.failure_code(), "UNSUPPORTED_VERSION");
}

#[test]
fn the_typed_code_and_the_string_code_agree() {
    let error = CloudLinkCodec::decode(b"{not json").expect_err("malformed JSON must not decode");

    assert_eq!(
        error.failure(),
        CloudLinkFailure::Codec(CloudLinkFailureCode::JsonSyntaxError)
    );
    assert_eq!(
        error.failure_code(),
        CloudLinkFailureCode::JsonSyntaxError.as_str()
    );
}
