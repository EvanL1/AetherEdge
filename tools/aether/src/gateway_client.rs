//! Builds the HTTP client for every request to the API gateway.

use std::fmt;

use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::redirect::Policy;
use reqwest::{Client, ClientBuilder, Url};

const RESERVED_HEADERS: &[&str] = &[
    "authorization",
    "x-request-id",
    "idempotency-key",
    "content-type",
    "content-length",
    "host",
    "accept",
    "if-match",
    "if-none-match",
    "transfer-encoding",
    "connection",
    "te",
    "trailer",
    "upgrade",
];
const RESERVED_PREFIX: &str = "x-aether-";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
enum ApiHeadersError {
    #[error("AETHER_API_HEADERS is not valid Unicode")]
    NotUnicode,
    #[error("AETHER_API_HEADERS entry {position} is not a `Name=value` pair")]
    MissingSeparator { position: usize },
    #[error("AETHER_API_HEADERS entry {position} does not start with a valid HTTP header name")]
    InvalidName { position: usize },
    #[error("AETHER_API_HEADERS gives header `{name}` an empty value")]
    EmptyValue { name: HeaderName },
    #[error(
        "AETHER_API_HEADERS gives header `{name}` a value that is not a valid HTTP header value"
    )]
    InvalidValue { name: HeaderName },
    #[error("AETHER_API_HEADERS sets header `{name}` more than once")]
    Duplicate { name: HeaderName },
    #[error("AETHER_API_HEADERS must not set `{name}`: the CLI owns that header")]
    Reserved { name: HeaderName },
}

#[derive(Debug, thiserror::Error)]
#[error(
    "refusing to follow a redirect to another origin ({target}) while AETHER_API_HEADERS is set"
)]
struct CrossOriginRedirect {
    target: String,
}

#[derive(Clone, Default)]
struct ExtraHeaders(HeaderMap);

impl fmt::Debug for ExtraHeaders {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_set()
            .entries(self.0.keys().map(HeaderName::as_str))
            .finish()
    }
}

impl ExtraHeaders {
    fn parse(raw: &str) -> Result<Self, ApiHeadersError> {
        let mut headers = HeaderMap::new();
        for (index, entry) in raw.split(',').enumerate() {
            let position = index + 1;
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            let (name, value) = entry
                .split_once('=')
                .ok_or(ApiHeadersError::MissingSeparator { position })?;
            let name = HeaderName::from_bytes(name.trim().as_bytes())
                .map_err(|_| ApiHeadersError::InvalidName { position })?;
            if is_reserved(&name) {
                return Err(ApiHeadersError::Reserved { name });
            }
            if headers.contains_key(&name) {
                return Err(ApiHeadersError::Duplicate { name });
            }
            let value = value.trim();
            if value.is_empty() {
                return Err(ApiHeadersError::EmptyValue { name });
            }
            let parsed = HeaderValue::from_str(value)
                .ok()
                .filter(|_| value.is_ascii());
            let Some(mut value) = parsed else {
                return Err(ApiHeadersError::InvalidValue { name });
            };
            value.set_sensitive(true);
            headers.insert(name, value);
        }
        Ok(Self(headers))
    }

    fn from_env() -> Result<Self, ApiHeadersError> {
        match std::env::var("AETHER_API_HEADERS") {
            Ok(raw) => Self::parse(&raw),
            Err(std::env::VarError::NotPresent) => Ok(Self::default()),
            Err(std::env::VarError::NotUnicode(_)) => Err(ApiHeadersError::NotUnicode),
        }
    }
}

fn is_reserved(name: &HeaderName) -> bool {
    let name = name.as_str();
    name.starts_with(RESERVED_PREFIX) || RESERVED_HEADERS.contains(&name)
}

pub(crate) fn client(base_url: &str) -> Result<Client> {
    Ok(builder(base_url)?.build()?)
}

pub(crate) fn builder(base_url: &str) -> Result<ClientBuilder> {
    builder_with(base_url, ExtraHeaders::from_env()?)
}

fn builder_with(base_url: &str, extra: ExtraHeaders) -> Result<ClientBuilder> {
    let mut builder = Client::builder();
    if crate::transport_security::is_loopback_base(base_url) {
        builder = builder.no_proxy();
    }
    if extra.0.is_empty() {
        return Ok(builder);
    }
    crate::transport_security::require_secure_extra_header_transport(base_url)?;
    let gateway = Url::parse(base_url).context("invalid gateway URL")?;
    Ok(builder
        .default_headers(extra.0)
        .redirect(same_origin_redirects(&gateway)))
}

fn same_origin_redirects(gateway: &Url) -> Policy {
    let origin = gateway.origin();
    let limited = Policy::default();
    Policy::custom(move |attempt| {
        let target = attempt.url().origin();
        if target == origin {
            limited.redirect(attempt)
        } else {
            attempt.error(CrossOriginRedirect {
                target: target.ascii_serialization(),
            })
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{ApiHeadersError, ExtraHeaders, RESERVED_HEADERS, builder_with};
    use reqwest::header::HeaderName;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SECRET: &str = "placeholder-secret-never-print";

    fn name(value: &'static str) -> HeaderName {
        HeaderName::from_static(value)
    }

    fn value_of<'a>(headers: &'a ExtraHeaders, header: &str) -> Option<&'a str> {
        headers.0.get(header).and_then(|value| value.to_str().ok())
    }

    #[test]
    fn parses_pairs_trimming_whitespace_and_skipping_empty_segments() {
        let headers = ExtraHeaders::parse(
            " CF-Access-Client-Id = placeholder-id.access ,, CF-Access-Client-Secret=abc ,",
        )
        .unwrap();

        assert_eq!(headers.0.len(), 2);
        assert_eq!(
            value_of(&headers, "cf-access-client-id"),
            Some("placeholder-id.access")
        );
        assert_eq!(value_of(&headers, "cf-access-client-secret"), Some("abc"));
    }

    #[test]
    fn splits_each_pair_on_the_first_equals_sign() {
        let headers = ExtraHeaders::parse("X-Probe=a=b==").unwrap();

        assert_eq!(value_of(&headers, "x-probe"), Some("a=b=="));
    }

    #[test]
    fn blank_input_yields_no_headers() {
        for raw in ["", "   ", ",", " , ,, "] {
            assert!(ExtraHeaders::parse(raw).unwrap().0.is_empty(), "{raw:?}");
        }
    }

    #[test]
    fn values_are_marked_sensitive() {
        let headers = ExtraHeaders::parse(&format!("X-Probe={SECRET}")).unwrap();

        assert!(headers.0.values().all(|value| value.is_sensitive()));
    }

    #[test]
    fn rejects_malformed_entries_with_typed_errors() {
        let cases = [
            (
                "X-Probe=1,no-separator",
                ApiHeadersError::MissingSeparator { position: 2 },
            ),
            ("=value", ApiHeadersError::InvalidName { position: 1 }),
            (
                "X-Probe=1,,bad name=2",
                ApiHeadersError::InvalidName { position: 3 },
            ),
            (
                " ,X-Probe=1, ,no-separator",
                ApiHeadersError::MissingSeparator { position: 4 },
            ),
            (
                "X-Probe=caf\u{e9}",
                ApiHeadersError::InvalidValue {
                    name: name("x-probe"),
                },
            ),
            (
                "X-Probe=",
                ApiHeadersError::EmptyValue {
                    name: name("x-probe"),
                },
            ),
            (
                "X-Probe=a\nInjected: b",
                ApiHeadersError::InvalidValue {
                    name: name("x-probe"),
                },
            ),
            (
                "X-Probe=a\u{7f}b",
                ApiHeadersError::InvalidValue {
                    name: name("x-probe"),
                },
            ),
            (
                "X-Probe=1,x-PROBE=2",
                ApiHeadersError::Duplicate {
                    name: name("x-probe"),
                },
            ),
        ];

        for (raw, expected) in cases {
            assert_eq!(ExtraHeaders::parse(raw).unwrap_err(), expected, "{raw:?}");
        }
    }

    #[test]
    fn rejects_every_reserved_name_in_any_case() {
        let governed = [
            "x-aether-confirmed",
            "x-aether-expected-revision",
            "x-aether-actor-id",
            "x-aether-anything-added-later",
        ];
        for reserved in RESERVED_HEADERS.iter().chain(&governed) {
            let mixed_case: String = reserved
                .chars()
                .enumerate()
                .map(|(index, character)| {
                    if index % 2 == 0 {
                        character.to_ascii_uppercase()
                    } else {
                        character
                    }
                })
                .collect();
            for spelling in [
                (*reserved).to_string(),
                reserved.to_ascii_uppercase(),
                mixed_case,
            ] {
                let error = ExtraHeaders::parse(&format!("X-Probe=1,{spelling}=true")).unwrap_err();
                assert_eq!(
                    error,
                    ApiHeadersError::Reserved {
                        name: HeaderName::from_bytes(reserved.as_bytes()).unwrap()
                    },
                    "{spelling}"
                );
            }
        }
    }

    #[test]
    fn confirmation_header_cannot_come_from_the_environment() {
        for raw in [
            "X-Aether-Confirmed=true",
            "x-aether-confirmed=true",
            " X-AETHER-CONFIRMED = true ",
            "CF-Access-Client-Id=placeholder-id.access,X-aEtHeR-cOnFiRmEd=true",
        ] {
            let error = ExtraHeaders::parse(raw).unwrap_err();
            assert_eq!(
                error,
                ApiHeadersError::Reserved {
                    name: name("x-aether-confirmed")
                },
                "{raw:?}"
            );
            assert!(error.to_string().contains("x-aether-confirmed"));
        }
    }

    #[test]
    fn errors_never_contain_any_part_of_the_input_values() {
        for raw in [
            format!("X-Probe=1,{SECRET}"),
            format!("{SECRET} bad={SECRET}"),
            format!("X-Probe={SECRET}\u{7f}"),
            format!("X-Probe={SECRET},X-Probe={SECRET}"),
            format!("Authorization=Bearer {SECRET}"),
            format!("X-Aether-Confirmed={SECRET}"),
        ] {
            let error = ExtraHeaders::parse(&raw).unwrap_err();
            let rendered = format!("{error} {error:?}");
            assert!(!rendered.contains(SECRET), "{rendered}");
            assert!(rendered.contains("AETHER_API_HEADERS"), "{rendered}");
        }
    }

    #[test]
    fn debug_output_names_headers_without_values() {
        let headers = ExtraHeaders::parse(&format!("CF-Access-Client-Secret={SECRET}")).unwrap();
        let client = builder_with("https://edge.example.test", headers.clone())
            .unwrap()
            .build()
            .unwrap();

        let rendered = format!("{headers:?} {client:?}");
        assert!(rendered.contains("cf-access-client-secret"), "{rendered}");
        assert!(!rendered.contains(SECRET), "{rendered}");
    }

    #[test]
    fn extra_headers_are_refused_on_remote_plaintext_http() {
        let headers = ExtraHeaders::parse(&format!("X-Probe={SECRET}")).unwrap();

        for rejected in [
            "http://192.0.2.10:6005/api/v1/io",
            "http://edge.example.test:6005",
            "https://user:password@edge.example.test",
        ] {
            let error = builder_with(rejected, headers.clone()).unwrap_err();
            assert!(!format!("{error:#}").contains(SECRET), "{error:#}");
        }
        let error = builder_with("http://192.0.2.10:6005", headers.clone()).unwrap_err();
        assert!(
            format!("{error:#}")
                .contains("AETHER_API_HEADERS values over a non-loopback plaintext"),
            "{error:#}"
        );

        for allowed in ["http://127.0.0.1:6005", "https://edge.example.test"] {
            assert!(builder_with(allowed, headers.clone()).is_ok(), "{allowed}");
        }
        assert!(builder_with("http://192.0.2.10:6005", ExtraHeaders::default()).is_ok());
    }

    #[tokio::test]
    async fn every_request_from_the_built_client_carries_the_headers() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/probe"))
            .and(header("cf-access-client-id", "placeholder-id.access"))
            .and(header("cf-access-client-secret", SECRET))
            .respond_with(ResponseTemplate::new(204))
            .expect(2)
            .mount(&server)
            .await;
        let headers = ExtraHeaders::parse(&format!(
            "CF-Access-Client-Id=placeholder-id.access,CF-Access-Client-Secret={SECRET}"
        ))
        .unwrap();
        let client = builder_with(&server.uri(), headers)
            .unwrap()
            .build()
            .unwrap();

        for _ in 0..2 {
            let response = client
                .get(format!("{}/probe", server.uri()))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 204);
        }
    }

    #[tokio::test]
    async fn cross_origin_redirect_is_an_error_that_names_only_the_target_origin() {
        let other_origin = MockServer::start().await;
        let gateway = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(302).insert_header(
                "location",
                format!("{}/login?state={SECRET}", other_origin.uri()),
            ))
            .mount(&gateway)
            .await;
        let headers = ExtraHeaders::parse("X-Probe=1").unwrap();
        let client = builder_with(&gateway.uri(), headers)
            .unwrap()
            .build()
            .unwrap();

        let error = client
            .get(format!("{}/probe", gateway.uri()))
            .send()
            .await
            .unwrap_err();

        let rendered = format!("{:#}", anyhow::Error::from(error));
        assert!(rendered.contains("AETHER_API_HEADERS"), "{rendered}");
        assert!(rendered.contains(&other_origin.uri()), "{rendered}");
        assert!(!rendered.contains(SECRET), "{rendered}");
        assert!(other_origin.received_requests().await.unwrap().is_empty());
    }
}
