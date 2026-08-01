//! One-shot AetherCloud gateway enrollment commands.

use std::io::{IsTerminal, Read};
use std::path::Path;
use std::sync::Arc;

use aether_application::EnrollGatewayWithAetherCloud;
use aether_cloud_enrollment_http::{HttpCloudEnrollmentClient, HttpCloudEnrollmentConfig};
use aether_ports::{
    CloudEndpointPolicy, ConfiguredGatewayIdentity, GatewayEnrollmentPhase,
    GatewayEnrollmentStatus, GatewayEnrollmentTarget, GatewayIdentityStore, SecretMaterial,
};
use aether_store_local::{
    FileGatewayIdentityStore, OsEd25519GatewayIdentityKeyGenerator, SystemClock,
};
use anyhow::{Result, anyhow, bail};
use clap::Subcommand;
use serde::Serialize;
use zeroize::Zeroizing;

use crate::output;

const MAX_ENROLLMENT_TOKEN_BYTES: u64 = 64 * 1024;
const CLAIMED_MESSAGE: &str = "身份已配对，CloudLink 凭据尚未激活";

/// AetherCloud identity commands.
#[derive(Subcommand)]
pub enum CloudCommands {
    /// Generate or reuse a local key and submit one AetherCloud Claim
    Enroll {
        /// Bare AetherCloud origin, such as https://api.aetheriot.ai
        #[arg(long)]
        cloud_url: String,

        /// Canonical non-nil Tenant UUID
        #[arg(long)]
        tenant_id: String,

        /// Canonical non-nil Project UUID
        #[arg(long)]
        project_id: String,

        /// Canonical non-nil Gateway UUID
        #[arg(long)]
        gateway_id: String,

        /// Read one bounded Enrollment Token from stdin
        #[arg(long)]
        token_stdin: bool,

        /// Development only: permit HTTP on exactly localhost or 127.0.0.1
        #[arg(long)]
        allow_insecure_localhost: bool,
    },

    /// Read local gateway enrollment state without contacting AetherCloud
    Status,
}

#[derive(Debug, Serialize)]
struct CloudStatusOutput {
    cloud_url: Option<String>,
    tenant_id: Option<String>,
    project_id: Option<String>,
    gateway_id: Option<String>,
    public_key_fingerprint: Option<String>,
    enrollment_state: &'static str,
    claimed_revision: Option<u64>,
}

impl CloudStatusOutput {
    fn from_status(status: &GatewayEnrollmentStatus) -> Self {
        match status {
            GatewayEnrollmentStatus::Unconfigured => Self {
                cloud_url: None,
                tenant_id: None,
                project_id: None,
                gateway_id: None,
                public_key_fingerprint: None,
                enrollment_state: "unconfigured",
                claimed_revision: None,
            },
            GatewayEnrollmentStatus::Configured(identity) => Self::from_identity(identity),
        }
    }

    fn from_identity(identity: &ConfiguredGatewayIdentity) -> Self {
        let target = identity.target();
        let enrollment_state = match identity.phase() {
            GatewayEnrollmentPhase::KeyGenerated => "key-generated",
            GatewayEnrollmentPhase::ClaimPending(_) => "claim-pending",
            GatewayEnrollmentPhase::Claimed(_) => "claimed",
        };
        Self {
            cloud_url: Some(target.cloud_origin().to_owned()),
            tenant_id: Some(target.tenant_id().to_string()),
            project_id: Some(target.project_id().to_string()),
            gateway_id: Some(target.gateway_id().to_string()),
            public_key_fingerprint: Some(identity.fingerprint().as_str().to_owned()),
            enrollment_state,
            claimed_revision: identity.claimed_revision(),
        }
    }
}

/// Composes the one-shot application use case without starting an uplink session.
pub async fn handle_command(
    command: CloudCommands,
    data_directory: &Path,
    json: bool,
) -> Result<()> {
    let identity_directory = data_directory.join("uplink/identity");
    let identity_store =
        Arc::new(FileGatewayIdentityStore::new(identity_directory).map_err(anyhow::Error::new)?);
    match command {
        CloudCommands::Enroll {
            cloud_url,
            tenant_id,
            project_id,
            gateway_id,
            token_stdin,
            allow_insecure_localhost,
        } => {
            let endpoint_policy = if allow_insecure_localhost {
                CloudEndpointPolicy::AllowLoopbackHttp
            } else {
                CloudEndpointPolicy::Production
            };
            let target = GatewayEnrollmentTarget::new(
                &cloud_url,
                &tenant_id,
                &project_id,
                &gateway_id,
                endpoint_policy,
            )
            .map_err(anyhow::Error::new)?;
            let client = Arc::new(
                HttpCloudEnrollmentClient::new(HttpCloudEnrollmentConfig::default())
                    .map_err(anyhow::Error::new)?,
            );
            let use_case = EnrollGatewayWithAetherCloud::new(
                client,
                Arc::new(OsEd25519GatewayIdentityKeyGenerator),
                identity_store,
                Arc::new(SystemClock),
            );
            enroll(&use_case, target, token_stdin, json).await
        },
        CloudCommands::Status => {
            let status = identity_store.load().await.map_err(anyhow::Error::new)?;
            print_status(&CloudStatusOutput::from_status(&status), json, false);
            Ok(())
        },
    }
}

async fn enroll(
    use_case: &EnrollGatewayWithAetherCloud,
    target: GatewayEnrollmentTarget,
    token_stdin: bool,
    json: bool,
) -> Result<()> {
    let current = use_case.status().await.map_err(anyhow::Error::new)?;
    if let GatewayEnrollmentStatus::Configured(identity) = &current {
        if identity.target() != &target {
            bail!("a different gateway identity is already configured");
        }
        if matches!(identity.phase(), GatewayEnrollmentPhase::Claimed(_)) {
            print_status(&CloudStatusOutput::from_identity(identity), json, true);
            return Ok(());
        }
    }

    let enrollment_token = read_enrollment_token(token_stdin)?;
    let result = use_case
        .enroll(target, enrollment_token)
        .await
        .map_err(anyhow::Error::new)?;
    print_status(
        &CloudStatusOutput::from_identity(result.identity()),
        json,
        true,
    );
    Ok(())
}

fn read_enrollment_token(token_stdin: bool) -> Result<SecretMaterial> {
    if token_stdin {
        return read_enrollment_token_from_stdin();
    }
    read_interactive_enrollment_token(std::io::stdin().is_terminal(), |prompt| {
        rpassword::prompt_password(prompt)
    })
}

fn read_interactive_enrollment_token(
    stdin_is_terminal: bool,
    hidden_prompt: impl FnOnce(&str) -> std::io::Result<String>,
) -> Result<SecretMaterial> {
    if !stdin_is_terminal {
        bail!("interactive Enrollment Token prompt requires a terminal; use --token-stdin");
    }
    let token = hidden_prompt("Enrollment Token: ")
        .map_err(|_| anyhow!("failed to read Enrollment Token from the terminal"))?;
    SecretMaterial::new(token).map_err(|_| anyhow!("Enrollment Token is empty or too large"))
}

fn read_enrollment_token_from_stdin() -> Result<SecretMaterial> {
    read_enrollment_token_from_reader(std::io::stdin().lock())
}

fn read_enrollment_token_from_reader(reader: impl Read) -> Result<SecretMaterial> {
    let mut bytes = Zeroizing::new(Vec::new());
    reader
        .take(MAX_ENROLLMENT_TOKEN_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow!("failed to read Enrollment Token from stdin"))?;
    if bytes.len() as u64 > MAX_ENROLLMENT_TOKEN_BYTES {
        bail!("Enrollment Token from stdin exceeds the 64 KiB limit");
    }
    let token = std::str::from_utf8(&bytes)
        .map_err(|_| anyhow!("Enrollment Token from stdin must be UTF-8"))?;
    let token = token.strip_suffix('\n').unwrap_or(token);
    let token = token.strip_suffix('\r').unwrap_or(token);
    if token.contains('\r') || token.contains('\n') {
        bail!("Enrollment Token stdin input must contain exactly one line");
    }
    SecretMaterial::new(token.to_owned())
        .map_err(|_| anyhow!("Enrollment Token is empty or too large"))
}

fn print_status(status: &CloudStatusOutput, json: bool, enrolled: bool) {
    if json {
        output::print_success(status);
        return;
    }

    println!("Enrollment state: {}", status.enrollment_state);
    if let Some(cloud_url) = &status.cloud_url {
        println!("Cloud URL: {cloud_url}");
    }
    if let Some(tenant_id) = &status.tenant_id {
        println!("Tenant ID: {tenant_id}");
    }
    if let Some(project_id) = &status.project_id {
        println!("Project ID: {project_id}");
    }
    if let Some(gateway_id) = &status.gateway_id {
        println!("Gateway ID: {gateway_id}");
    }
    if let Some(fingerprint) = &status.public_key_fingerprint {
        println!("Public-key fingerprint: {fingerprint}");
    }
    if let Some(revision) = status.claimed_revision {
        println!("Claimed revision: {revision}");
    }
    if enrolled {
        println!("{CLAIMED_MESSAGE}");
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::io::Cursor;

    use super::{
        MAX_ENROLLMENT_TOKEN_BYTES, read_enrollment_token_from_reader,
        read_interactive_enrollment_token,
    };

    #[test]
    fn interactive_token_uses_the_hidden_prompt_boundary_only_for_a_terminal() {
        let prompt_called = Cell::new(false);
        let token = read_interactive_enrollment_token(true, |prompt| {
            prompt_called.set(true);
            assert_eq!(prompt, "Enrollment Token: ");
            Ok("opaque-interactive-token".to_owned())
        })
        .expect("hidden terminal prompt token");
        assert!(prompt_called.get());
        assert_eq!(token.expose(), "opaque-interactive-token");

        let prompt_called = Cell::new(false);
        let error = read_interactive_enrollment_token(false, |_| {
            prompt_called.set(true);
            Ok("must-not-be-read".to_owned())
        })
        .expect_err("non-terminal input requires --token-stdin");
        assert!(!prompt_called.get());
        assert!(error.to_string().contains("--token-stdin"));
    }

    #[test]
    fn stdin_token_is_one_bounded_line() {
        let token = read_enrollment_token_from_reader(Cursor::new(b"opaque-token\r\n"))
            .expect("one token line");
        assert_eq!(token.expose(), "opaque-token");
        assert!(
            read_enrollment_token_from_reader(Cursor::new(b"first\nsecond\n")).is_err(),
            "multiple lines must be rejected"
        );
        assert!(
            read_enrollment_token_from_reader(Cursor::new(vec![
                b'x';
                (MAX_ENROLLMENT_TOKEN_BYTES + 1)
                    as usize
            ]))
            .is_err(),
            "oversized token must be rejected"
        );
    }
}
