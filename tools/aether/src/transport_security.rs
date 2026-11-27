//! Shared transport checks for CLI and MCP commands carrying access tokens.

use std::net::IpAddr;

use anyhow::{Context, Result, bail};

/// Refuses to attach a Bearer credential to remote plaintext HTTP.
///
/// Direct service ports are loopback interfaces. A remote command must use a
/// certificate-validated HTTPS ingress; read-only compatibility queries do not
/// call this guard because they carry no access token.
pub(crate) fn require_secure_bearer_transport(base_url: &str) -> Result<()> {
    require_secure_transport(base_url, "an Aether Bearer token")
}

pub(crate) fn require_secure_extra_header_transport(base_url: &str) -> Result<()> {
    require_secure_transport(base_url, "AETHER_API_HEADERS values")
}

fn require_secure_transport(base_url: &str, credential: &str) -> Result<()> {
    let url = reqwest::Url::parse(base_url)
        .context("the gateway URL from AETHER_API_URL or --host is not a valid URL")?;
    if !url.username().is_empty() || url.password().is_some() {
        bail!("governed service URLs must not contain embedded credentials");
    }

    if url.scheme() == "https" {
        return Ok(());
    }
    if url.scheme() == "http" && url.host_str().is_some_and(is_loopback_host) {
        return Ok(());
    }

    bail!(
        "refusing to send {credential} over a non-loopback plaintext transport; use the on-device CLI/loopback or a certificate-validated HTTPS ingress"
    )
}

pub(crate) fn is_loopback_base(base_url: &str) -> bool {
    reqwest::Url::parse(base_url).is_ok_and(|url| url.host_str().is_some_and(is_loopback_host))
}

fn is_loopback_host(host: &str) -> bool {
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

#[cfg(test)]
mod tests {
    use super::{
        is_loopback_base, require_secure_bearer_transport, require_secure_extra_header_transport,
    };

    #[test]
    fn errors_never_echo_the_url_or_its_credentials() {
        for url in [
            "http://operator:hunter2-password@edge.example.test:notaport",
            "operator:hunter2-password@edge.example.test",
            "http://operator:hunter2-password@localhost:6005",
            "http://operator:hunter2-password@edge.example.test:6005",
        ] {
            for guard in [
                require_secure_bearer_transport,
                require_secure_extra_header_transport,
            ] {
                let error = format!("{:#}", guard(url).unwrap_err());
                assert!(!error.contains("hunter2-password"), "{error}");
                assert!(!error.contains("operator"), "{error}");
                assert!(!error.contains("edge.example.test"), "{error}");
            }
        }
        let error = format!(
            "{:#}",
            require_secure_bearer_transport("http://edge.example.test:notaport").unwrap_err()
        );
        assert!(error.contains("AETHER_API_URL"), "{error}");
    }

    #[test]
    fn loopback_base_matches_the_hosts_the_guard_allows_over_http() {
        for loopback in [
            "http://localhost:6005",
            "http://LOCALHOST:6005/api/v1/io",
            "http://127.0.0.1:6005",
            "http://[::1]:6005",
            "https://localhost:8443",
        ] {
            assert!(is_loopback_base(loopback), "{loopback}");
        }
        for remote in [
            "https://edge.example.test",
            "http://192.0.2.10:6005",
            "http://localhost.example.test:6005",
            "not a url",
        ] {
            assert!(!is_loopback_base(remote), "{remote}");
        }
    }

    #[test]
    fn bearer_transport_allows_loopback_http_and_remote_https_only() {
        for allowed in [
            "http://localhost:6001",
            "http://127.0.0.1:6001",
            "http://[::1]:6001",
            "https://edge.example.test/aether",
        ] {
            require_secure_bearer_transport(allowed)
                .unwrap_or_else(|error| panic!("{allowed} should be allowed: {error:#}"));
        }

        for rejected in [
            "http://192.0.2.10:6001",
            "http://edge.example.test:6001",
            "ftp://localhost/aether",
            "http://user:password@localhost:6001",
        ] {
            assert!(
                require_secure_bearer_transport(rejected).is_err(),
                "{rejected} should be rejected"
            );
        }
    }
}
