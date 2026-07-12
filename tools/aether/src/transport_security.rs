//! Shared transport checks for CLI and MCP commands carrying access tokens.

use std::net::IpAddr;

use anyhow::{Context, Result, bail};

/// Refuses to attach a Bearer credential to remote plaintext HTTP.
///
/// Direct service ports are loopback interfaces. A remote command must use a
/// certificate-validated HTTPS ingress; read-only compatibility queries do not
/// call this guard because they carry no access token.
pub(crate) fn require_secure_bearer_transport(base_url: &str) -> Result<()> {
    let url = reqwest::Url::parse(base_url)
        .with_context(|| format!("invalid governed service URL `{base_url}`"))?;
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
        "refusing to send an Aether Bearer token over a non-loopback plaintext transport; use the on-device CLI/loopback or a certificate-validated HTTPS ingress"
    )
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
    use super::require_secure_bearer_transport;

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
