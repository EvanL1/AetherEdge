use std::fmt;

use aether_ports::MAX_CLOUD_ENROLLMENT_REVISION;
use base64::Engine as _;
use reqwest::Url;
use serde::{Deserialize, Serialize};

const CLAIM_ENDPOINT_PATH: &str = "api/v1/fleet/enrollment-claims:claim";
const CLAIM_REQUEST_SCHEMA: &str = "aether.cloud.gateway-enrollment-claim.v1";
const CLAIMED_RESPONSE_SCHEMA: &str = "aether.cloud.gateway-enrollment-claimed.v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WireValidationError(&'static str);

impl fmt::Display for WireValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ClaimedResponse {
    schema: String,
    pub(crate) gateway_id: String,
    state: String,
    pub(crate) revision: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ClaimRequest<'a> {
    schema: &'static str,
    tenant_id: &'a str,
    project_id: &'a str,
    gateway_id: &'a str,
    enrollment_token: &'a str,
    credential_request: CredentialRequest<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CredentialRequest<'a> {
    algorithm: &'static str,
    public_key: String,
    fingerprint: &'a str,
}

pub(crate) fn encode_claim_request(
    tenant_id: &str,
    project_id: &str,
    gateway_id: &str,
    enrollment_token: &str,
    public_key: &[u8; 32],
    fingerprint: &str,
) -> Result<Vec<u8>, WireValidationError> {
    validate_public_key_fingerprint(fingerprint)?;
    let request = ClaimRequest {
        schema: CLAIM_REQUEST_SCHEMA,
        tenant_id,
        project_id,
        gateway_id,
        enrollment_token,
        credential_request: CredentialRequest {
            algorithm: "ed25519",
            public_key: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(public_key),
            fingerprint,
        },
    };
    serde_json::to_vec(&request)
        .map_err(|_| WireValidationError("Cloud Claim request could not be encoded"))
}

pub(crate) fn claim_endpoint(
    cloud_origin: &str,
    allow_loopback_http: bool,
) -> Result<Url, WireValidationError> {
    validate_cloud_origin(cloud_origin, allow_loopback_http)?
        .join(CLAIM_ENDPOINT_PATH)
        .map_err(|_| WireValidationError("Cloud Claim endpoint is invalid"))
}

fn validate_cloud_origin(
    value: &str,
    allow_loopback_http: bool,
) -> Result<Url, WireValidationError> {
    let url =
        Url::parse(value).map_err(|_| WireValidationError("Cloud origin is not a valid URL"))?;
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return Err(WireValidationError(
            "Cloud URL must contain only a trusted origin",
        ));
    }

    match url.scheme() {
        "https" => Ok(url),
        "http" if allow_loopback_http && has_exact_loopback_http_authority(value) => Ok(url),
        "http" => Err(WireValidationError(
            "Cloud enrollment requires HTTPS outside explicit loopback development",
        )),
        _ => Err(WireValidationError(
            "Cloud enrollment URL scheme is unsupported",
        )),
    }
}

fn has_exact_loopback_http_authority(value: &str) -> bool {
    let Some(scheme) = value.get(..7) else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("http://") {
        return false;
    }
    let remainder = &value[7..];
    let authority_end = remainder.find(['/', '?', '#']).unwrap_or(remainder.len());
    let authority = &remainder[..authority_end];
    if authority.is_empty() || authority.contains('@') || authority.starts_with('[') {
        return false;
    }

    let mut parts = authority.split(':');
    let Some(host) = parts.next() else {
        return false;
    };
    let port_is_valid = match parts.next() {
        Some(port) => !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()),
        None => true,
    };
    port_is_valid
        && parts.next().is_none()
        && (host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1")
}

fn validate_public_key_fingerprint(value: &str) -> Result<(), WireValidationError> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        Ok(())
    } else {
        Err(WireValidationError(
            "public-key fingerprint must be lowercase SHA-256 hex",
        ))
    }
}

pub(crate) fn decode_claimed_response(
    bytes: &[u8],
    expected_gateway_id: &str,
) -> Result<ClaimedResponse, WireValidationError> {
    let response: ClaimedResponse = serde_json::from_slice(bytes)
        .map_err(|_| WireValidationError("Cloud Claim response is invalid"))?;
    if response.schema != CLAIMED_RESPONSE_SCHEMA {
        return Err(WireValidationError(
            "Cloud Claim response schema is unsupported",
        ));
    }
    if response.gateway_id != expected_gateway_id {
        return Err(WireValidationError(
            "Cloud Claim response Gateway identity does not match",
        ));
    }
    if response.state != "claimed" {
        return Err(WireValidationError(
            "Cloud Claim response state is not claimed",
        ));
    }
    if !(1..=MAX_CLOUD_ENROLLMENT_REVISION).contains(&response.revision) {
        return Err(WireValidationError(
            "Cloud Claim response revision is unsafe",
        ));
    }
    Ok(response)
}

pub(crate) fn validate_json_content_type(value: &str) -> Result<(), WireValidationError> {
    let mut parts = value.split(';');
    if !parts
        .next()
        .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case("application/json"))
    {
        return Err(WireValidationError(
            "Cloud Claim response media type is invalid",
        ));
    }

    let mut charset_count = 0_u8;
    for parameter in parts {
        if parameter.trim().eq_ignore_ascii_case("charset=utf-8") {
            charset_count = charset_count.saturating_add(1);
        } else {
            return Err(WireValidationError(
                "Cloud Claim response media type is invalid",
            ));
        }
    }
    if charset_count <= 1 {
        Ok(())
    } else {
        Err(WireValidationError(
            "Cloud Claim response media type is invalid",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        claim_endpoint, decode_claimed_response, encode_claim_request, validate_cloud_origin,
        validate_json_content_type, validate_public_key_fingerprint,
    };
    use aether_ports::MAX_CLOUD_ENROLLMENT_REVISION;
    use serde_json::json;

    const GATEWAY_ID: &str = "33333333-3333-4333-8333-333333333333";

    #[test]
    fn production_accepts_only_a_root_https_origin() {
        let origin =
            validate_cloud_origin("https://api.aetheriot.dev", false).expect("HTTPS origin");
        assert_eq!(origin.as_str(), "https://api.aetheriot.dev/");

        for rejected in [
            "http://api.aetheriot.dev",
            "ftp://api.aetheriot.dev",
            "https://user:password@api.aetheriot.dev",
            "https://api.aetheriot.dev/base",
            "https://api.aetheriot.dev/?tenant=secret",
            "https://api.aetheriot.dev/#fragment",
        ] {
            assert!(
                validate_cloud_origin(rejected, false).is_err(),
                "{rejected} must be rejected"
            );
        }
    }

    #[test]
    fn development_http_is_explicit_and_limited_to_exact_loopback_names() {
        for allowed in [
            "http://localhost",
            "http://localhost:4321/",
            "http://127.0.0.1",
            "http://127.0.0.1:4321/",
        ] {
            validate_cloud_origin(allowed, true)
                .unwrap_or_else(|error| panic!("{allowed} should be allowed: {error}"));
        }

        for rejected in [
            "http://localhost",
            "http://127.0.0.1",
            "http://127.0.0.2",
            "http://[::1]",
            "http://localhost.example.test",
            "http://192.0.2.1",
        ] {
            let allow_loopback_http =
                rejected != "http://localhost" && rejected != "http://127.0.0.1";
            assert!(
                validate_cloud_origin(rejected, allow_loopback_http).is_err(),
                "{rejected} must be rejected with allow_loopback_http={allow_loopback_http}"
            );
        }
    }

    #[test]
    fn claimed_response_is_closed_and_requires_matching_identity_and_safe_revision() {
        let valid = serde_json::to_vec(&json!({
            "schema": "aether.cloud.gateway-enrollment-claimed.v1",
            "gatewayId": GATEWAY_ID,
            "state": "claimed",
            "revision": MAX_CLOUD_ENROLLMENT_REVISION,
        }))
        .expect("response JSON");
        let decoded = decode_claimed_response(&valid, GATEWAY_ID).expect("valid response");
        assert_eq!(decoded.gateway_id, GATEWAY_ID);
        assert_eq!(decoded.revision, MAX_CLOUD_ENROLLMENT_REVISION);

        let invalid_cases = [
            json!({
                "schema": "aether.cloud.gateway-enrollment-claimed.v2",
                "gatewayId": GATEWAY_ID,
                "state": "claimed",
                "revision": 1,
            }),
            json!({
                "schema": "aether.cloud.gateway-enrollment-claimed.v1",
                "gatewayId": "44444444-4444-4444-8444-444444444444",
                "state": "claimed",
                "revision": 1,
            }),
            json!({
                "schema": "aether.cloud.gateway-enrollment-claimed.v1",
                "gatewayId": GATEWAY_ID,
                "state": "credential-active",
                "revision": 1,
            }),
            json!({
                "schema": "aether.cloud.gateway-enrollment-claimed.v1",
                "gatewayId": GATEWAY_ID,
                "state": "claimed",
                "revision": 0,
            }),
            json!({
                "schema": "aether.cloud.gateway-enrollment-claimed.v1",
                "gatewayId": GATEWAY_ID,
                "state": "claimed",
                "revision": MAX_CLOUD_ENROLLMENT_REVISION + 1,
            }),
            json!({
                "schema": "aether.cloud.gateway-enrollment-claimed.v1",
                "gatewayId": GATEWAY_ID,
                "state": "claimed",
                "revision": 1,
                "credential": "must-not-be-accepted",
            }),
        ];
        for invalid in invalid_cases {
            let bytes = serde_json::to_vec(&invalid).expect("invalid fixture JSON");
            assert!(
                decode_claimed_response(&bytes, GATEWAY_ID).is_err(),
                "response must fail closed: {invalid}"
            );
        }
    }

    #[test]
    fn response_media_type_is_strict_json() {
        for allowed in [
            "application/json",
            "application/json; charset=utf-8",
            "Application/JSON; Charset=UTF-8",
        ] {
            validate_json_content_type(allowed)
                .unwrap_or_else(|error| panic!("{allowed} should be accepted: {error}"));
        }
        for rejected in [
            "",
            "text/plain",
            "application/problem+json",
            "application/json; profile=secret",
            "application/json; charset=iso-8859-1",
            "application/json; charset=utf-8; charset=utf-8",
        ] {
            assert!(
                validate_json_content_type(rejected).is_err(),
                "{rejected} must be rejected"
            );
        }
    }

    #[test]
    fn claim_endpoint_is_fixed_below_the_validated_origin() {
        let endpoint = claim_endpoint("https://api.aetheriot.dev", false).expect("Claim endpoint");
        assert_eq!(
            endpoint.as_str(),
            "https://api.aetheriot.dev/api/v1/fleet/enrollment-claims:claim"
        );
    }

    #[test]
    fn claim_request_serializes_the_exact_v1_contract() {
        let bytes = encode_claim_request(
            "11111111-1111-4111-8111-111111111111",
            "22222222-2222-4222-8222-222222222222",
            GATEWAY_ID,
            "opaque-enrollment-token",
            &[0_u8; 32],
            "66687aadf862bd776c8fc18b8e9f8e20089714856ee233b3902a591d0d5f2925",
        )
        .expect("Claim request");
        let actual: serde_json::Value = serde_json::from_slice(&bytes).expect("request JSON");
        assert_eq!(
            actual,
            json!({
                "schema": "aether.cloud.gateway-enrollment-claim.v1",
                "tenantId": "11111111-1111-4111-8111-111111111111",
                "projectId": "22222222-2222-4222-8222-222222222222",
                "gatewayId": GATEWAY_ID,
                "enrollmentToken": "opaque-enrollment-token",
                "credentialRequest": {
                    "algorithm": "ed25519",
                    "publicKey": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                    "fingerprint":
                        "66687aadf862bd776c8fc18b8e9f8e20089714856ee233b3902a591d0d5f2925",
                },
            })
        );
    }

    #[test]
    fn fingerprint_is_exact_lowercase_sha256_hex() {
        validate_public_key_fingerprint(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .expect("lowercase SHA-256 fingerprint");

        for rejected in [
            "",
            "0123456789abcdef",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcde",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0",
            "0123456789ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef",
            "g123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        ] {
            assert!(
                validate_public_key_fingerprint(rejected).is_err(),
                "{rejected} must be rejected"
            );
        }
    }
}
