#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const CLAIM_PATH: &str = "/api/v1/fleet/enrollment-claims:claim";
const TENANT_ID: &str = "11111111-1111-4111-8111-111111111111";
const PROJECT_ID: &str = "22222222-2222-4222-8222-222222222222";
const GATEWAY_ID: &str = "33333333-3333-4333-8333-333333333333";
const OTHER_GATEWAY_ID: &str = "44444444-4444-4444-8444-444444444444";
const TOKEN: &str = "opaque-enrollment-token-never-log";

struct ProcessWorkspace {
    _root: TempDir,
    config: PathBuf,
    data: PathBuf,
}

impl ProcessWorkspace {
    fn new() -> Self {
        let root = TempDir::new().expect("create process workspace");
        let canonical_root = root
            .path()
            .canonicalize()
            .expect("canonical process workspace");
        let config = canonical_root.join("config");
        let data = canonical_root.join("data");
        fs::create_dir(&config).expect("create config directory");
        Self {
            _root: root,
            config,
            data,
        }
    }

    fn identity_directory(&self) -> PathBuf {
        self.data.join("uplink/identity")
    }

    fn base_args(&self) -> Vec<OsString> {
        vec![
            OsString::from("--json"),
            OsString::from("--config-path"),
            self.config.as_os_str().to_owned(),
            OsString::from("--db-path"),
            self.data.as_os_str().to_owned(),
        ]
    }
}

async fn invoke(
    workspace: &ProcessWorkspace,
    command_args: impl IntoIterator<Item = OsString>,
    stdin: Option<&str>,
) -> Output {
    let mut args = workspace.base_args();
    args.extend(command_args);
    invoke_args(args, stdin).await
}

async fn invoke_human(
    workspace: &ProcessWorkspace,
    command_args: impl IntoIterator<Item = OsString>,
    stdin: Option<&str>,
) -> Output {
    let mut args = workspace.base_args();
    args.retain(|argument| argument != "--json");
    args.extend(command_args);
    invoke_args(args, stdin).await
}

async fn invoke_args(args: Vec<OsString>, stdin: Option<&str>) -> Output {
    let stdin = stdin.map(str::to_owned);
    tokio::task::spawn_blocking(move || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_aether"));
        command
            .args(args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("spawn real aether binary");
        if let Some(stdin) = stdin {
            child
                .stdin
                .take()
                .expect("piped stdin")
                .write_all(stdin.as_bytes())
                .expect("write enrollment token");
        }
        child.wait_with_output().expect("wait for aether")
    })
    .await
    .expect("process task joins")
}

fn enroll_args(cloud_url: &str, gateway_id: &str) -> Vec<OsString> {
    enroll_args_with_scope(cloud_url, TENANT_ID, PROJECT_ID, gateway_id, true)
}

fn enroll_args_with_scope(
    cloud_url: &str,
    tenant_id: &str,
    project_id: &str,
    gateway_id: &str,
    allow_insecure_localhost: bool,
) -> Vec<OsString> {
    let mut arguments = vec![
        OsString::from("cloud"),
        OsString::from("enroll"),
        OsString::from("--cloud-url"),
        OsString::from(cloud_url),
        OsString::from("--tenant-id"),
        OsString::from(tenant_id),
        OsString::from("--project-id"),
        OsString::from(project_id),
        OsString::from("--gateway-id"),
        OsString::from(gateway_id),
    ];
    if allow_insecure_localhost {
        arguments.push(OsString::from("--allow-insecure-localhost"));
    }
    arguments.push(OsString::from("--token-stdin"));
    arguments
}

fn status_args() -> Vec<OsString> {
    ["cloud", "status"]
        .into_iter()
        .map(OsString::from)
        .collect()
}

fn json_envelope(output: &Output) -> Value {
    let stdout = String::from_utf8(output.stdout.clone()).expect("stdout is UTF-8");
    serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("stdout is not one JSON envelope: {error}\n{stdout}"))
}

fn request_json(request: &Request) -> Value {
    serde_json::from_slice(&request.body).expect("Claim request is JSON")
}

fn request_header(request: &Request, name: &str) -> String {
    request
        .headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_else(|| panic!("request has {name} header"))
        .to_owned()
}

fn inspect_durable_pending_identity(identity_directory: &Path) -> Result<(), String> {
    let directory_metadata = fs::symlink_metadata(identity_directory)
        .map_err(|error| format!("identity directory missing: {error}"))?;
    if directory_metadata.file_type().is_symlink() || !directory_metadata.is_dir() {
        return Err("identity path is not a regular directory".to_owned());
    }
    if directory_metadata.permissions().mode() & 0o777 != 0o700 {
        return Err("identity directory mode is not 0700".to_owned());
    }

    let mut private_seed = false;
    let mut pending_state = false;
    for entry in fs::read_dir(identity_directory)
        .map_err(|error| format!("identity directory is unreadable: {error}"))?
    {
        let entry = entry.map_err(|error| format!("identity entry is unreadable: {error}"))?;
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|error| format!("identity metadata is unreadable: {error}"))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err("identity directory contains a non-regular file".to_owned());
        }
        if metadata.permissions().mode() & 0o777 != 0o600 {
            return Err("identity file mode is not 0600".to_owned());
        }
        let bytes = fs::read(entry.path())
            .map_err(|error| format!("identity file is unreadable: {error}"))?;
        match entry.file_name().to_str() {
            Some("gateway-identity.seed") => {
                if bytes.len() != 32 {
                    return Err("private seed file is not exactly 32 bytes".to_owned());
                }
                private_seed = true;
            },
            Some("gateway-enrollment.json") => {
                pending_state = std::str::from_utf8(&bytes).is_ok_and(|text| {
                    text.contains("claim-pending")
                        || text.contains("claim_pending")
                        || text.contains("claimPending")
                });
            },
            _ => return Err("identity directory contains an unexpected temporary file".to_owned()),
        }
    }

    if !private_seed {
        return Err("private seed was not durable before HTTP".to_owned());
    }
    if !pending_state {
        return Err("durable state was not claim-pending before HTTP".to_owned());
    }
    Ok(())
}

fn claimed_response(revision: u64) -> Value {
    json!({
        "schema": "aether.cloud.gateway-enrollment-claimed.v1",
        "gatewayId": GATEWAY_ID,
        "state": "claimed",
        "revision": revision,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_aether_process_persists_pending_then_sends_exact_claim_and_reports_claimed() {
    let workspace = ProcessWorkspace::new();
    let server = MockServer::start().await;
    let pending_observation = Arc::new(Mutex::new(None));
    let responder_observation = pending_observation.clone();
    let identity_directory = workspace.identity_directory();
    Mock::given(method("POST"))
        .and(path(CLAIM_PATH))
        .respond_with(move |_request: &Request| {
            let observation = inspect_durable_pending_identity(&identity_directory);
            *responder_observation
                .lock()
                .expect("pending observation lock") = Some(observation.clone());
            if observation.is_ok() {
                ResponseTemplate::new(200).set_body_json(claimed_response(7))
            } else {
                ResponseTemplate::new(500).set_body_json(json!({"error": "pending state absent"}))
            }
        })
        .expect(1)
        .mount(&server)
        .await;

    let output = invoke(
        &workspace,
        enroll_args(&server.uri(), GATEWAY_ID),
        Some(&format!("{TOKEN}\n")),
    )
    .await;

    assert!(
        output.status.success(),
        "enrollment failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    pending_observation
        .lock()
        .expect("pending observation lock")
        .clone()
        .expect("Cloud request observed")
        .expect("pending identity was durable before HTTP");

    let requests = server.received_requests().await.expect("received requests");
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request_header(request, "content-type"), "application/json");
    assert!(Uuid::parse_str(&request_header(request, "idempotency-key")).is_ok());
    assert!(!request.headers.contains_key("authorization"));
    let body = request_json(request);
    assert_eq!(
        body,
        json!({
            "schema": "aether.cloud.gateway-enrollment-claim.v1",
            "tenantId": TENANT_ID,
            "projectId": PROJECT_ID,
            "gatewayId": GATEWAY_ID,
            "enrollmentToken": TOKEN,
            "credentialRequest": {
                "algorithm": "ed25519",
                "publicKey": body.pointer("/credentialRequest/publicKey").expect("public key"),
                "fingerprint": body.pointer("/credentialRequest/fingerprint").expect("fingerprint"),
            },
        })
    );
    let encoded_public_key = body
        .pointer("/credentialRequest/publicKey")
        .and_then(Value::as_str)
        .expect("public key string");
    let public_key = URL_SAFE_NO_PAD
        .decode(encoded_public_key)
        .expect("unpadded base64url");
    assert_eq!(public_key.len(), 32);
    let fingerprint = body
        .pointer("/credentialRequest/fingerprint")
        .and_then(Value::as_str)
        .expect("fingerprint");
    assert_eq!(fingerprint, format!("{:x}", Sha256::digest(&public_key)));

    let enrollment = json_envelope(&output);
    assert_eq!(enrollment.pointer("/success"), Some(&Value::Bool(true)));
    assert_eq!(
        enrollment.pointer("/data/enrollment_state"),
        Some(&Value::String("claimed".to_owned()))
    );
    assert_eq!(
        enrollment.pointer("/data/claimed_revision"),
        Some(&json!(7))
    );
    let enrollment_data = enrollment
        .pointer("/data")
        .and_then(Value::as_object)
        .expect("enrollment data object");
    for forbidden_field in [
        "enrollment_token",
        "private_key",
        "public_key",
        "online",
        "connected",
    ] {
        assert!(!enrollment_data.contains_key(forbidden_field));
    }
    let rendered = String::from_utf8_lossy(&output.stdout);
    assert!(!rendered.contains(TOKEN));
    assert!(!rendered.contains(encoded_public_key));
    assert!(!rendered.contains("online"));
    assert!(!rendered.contains("connected"));

    let status = invoke(&workspace, status_args(), None).await;
    assert!(status.status.success());
    let status = json_envelope(&status);
    assert_eq!(
        status.pointer("/data/cloud_url"),
        Some(&Value::String(server.uri()))
    );
    assert_eq!(
        status.pointer("/data/public_key_fingerprint"),
        Some(&Value::String(fingerprint.to_owned()))
    );
    assert_eq!(
        status.pointer("/data/enrollment_state"),
        Some(&Value::String("claimed".to_owned()))
    );
    assert_eq!(status.pointer("/data/claimed_revision"), Some(&json!(7)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retry_reuses_public_key_fingerprint_and_idempotency_without_leaking_tokens() {
    let workspace = ProcessWorkspace::new();
    let server = MockServer::start().await;
    let attempts = Arc::new(AtomicUsize::new(0));
    let responder_attempts = attempts.clone();
    Mock::given(method("POST"))
        .and(path(CLAIM_PATH))
        .respond_with(move |_request: &Request| {
            if responder_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(503).set_body_json(json!({"error": "unavailable"}))
            } else {
                ResponseTemplate::new(200).set_body_json(claimed_response(9))
            }
        })
        .expect(2)
        .mount(&server)
        .await;

    let first_token = "first-token-must-not-leak";
    let first = invoke(
        &workspace,
        enroll_args(&server.uri(), GATEWAY_ID),
        Some(&format!("{first_token}\n")),
    )
    .await;
    assert!(!first.status.success());
    assert!(!String::from_utf8_lossy(&first.stdout).contains(first_token));
    assert!(!String::from_utf8_lossy(&first.stderr).contains(first_token));
    let pending_status = invoke(&workspace, status_args(), None).await;
    assert_eq!(
        json_envelope(&pending_status).pointer("/data/enrollment_state"),
        Some(&Value::String("claim-pending".to_owned()))
    );

    let second_token = "second-token-must-not-leak";
    let second = invoke(
        &workspace,
        enroll_args(&server.uri(), GATEWAY_ID),
        Some(&format!("{second_token}\n")),
    )
    .await;
    assert!(
        second.status.success(),
        "retry failed: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(!String::from_utf8_lossy(&second.stdout).contains(second_token));
    assert!(!String::from_utf8_lossy(&second.stderr).contains(second_token));

    let requests = server.received_requests().await.expect("received requests");
    assert_eq!(requests.len(), 2);
    let first_body = request_json(&requests[0]);
    let second_body = request_json(&requests[1]);
    assert_eq!(
        first_body.pointer("/credentialRequest/publicKey"),
        second_body.pointer("/credentialRequest/publicKey")
    );
    assert_eq!(
        first_body.pointer("/credentialRequest/fingerprint"),
        second_body.pointer("/credentialRequest/fingerprint")
    );
    assert_eq!(
        request_header(&requests[0], "idempotency-key"),
        request_header(&requests[1], "idempotency-key")
    );
    assert_ne!(
        first_body.get("enrollmentToken"),
        second_body.get("enrollmentToken")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claimed_scope_is_idempotent_and_different_gateway_fails_before_prompt_or_http() {
    let workspace = ProcessWorkspace::new();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CLAIM_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(claimed_response(11)))
        .expect(1)
        .mount(&server)
        .await;
    let initial = invoke(
        &workspace,
        enroll_args(&server.uri(), GATEWAY_ID),
        Some(&format!("{TOKEN}\n")),
    )
    .await;
    assert!(initial.status.success());

    let mut same_scope = enroll_args(&server.uri(), GATEWAY_ID);
    same_scope.retain(|argument| argument != "--token-stdin");
    let repeated = invoke(&workspace, same_scope, None).await;
    assert!(
        repeated.status.success(),
        "claimed repeat unexpectedly prompted: {}",
        String::from_utf8_lossy(&repeated.stderr)
    );
    let mut human_scope = enroll_args(&server.uri(), GATEWAY_ID);
    human_scope.retain(|argument| argument != "--token-stdin");
    let human = invoke_human(&workspace, human_scope, None).await;
    assert!(human.status.success());
    assert!(
        String::from_utf8_lossy(&human.stdout)
            .contains("Identity claimed; CloudLink credentials not yet activated")
    );

    let mut other_scope = enroll_args(&server.uri(), OTHER_GATEWAY_ID);
    other_scope.retain(|argument| argument != "--token-stdin");
    let conflict = invoke(&workspace, other_scope, None).await;
    assert!(!conflict.status.success());
    let conflict_output = json_envelope(&conflict);
    assert_eq!(
        conflict_output.pointer("/success"),
        Some(&Value::Bool(false))
    );
    assert!(
        conflict_output
            .pointer("/error")
            .and_then(Value::as_str)
            .is_some_and(|message| message.contains("different gateway identity"))
    );
    assert_eq!(
        server
            .received_requests()
            .await
            .expect("received requests")
            .len(),
        1
    );
}

#[test]
fn token_is_not_a_cli_argument_and_rejected_value_is_not_echoed() {
    let workspace = ProcessWorkspace::new();
    let forbidden_token = "forbidden-token-in-argv";
    for forbidden_arguments in [
        vec![OsString::from("--token"), OsString::from(forbidden_token)],
        vec![OsString::from(format!("--token={forbidden_token}"))],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_aether"))
            .args(workspace.base_args())
            .args(["cloud", "enroll"])
            .args(forbidden_arguments)
            .output()
            .expect("run aether");

        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stdout).contains(forbidden_token));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(forbidden_token));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn noninteractive_enrollment_requires_explicit_token_stdin() {
    let workspace = ProcessWorkspace::new();
    let mut args = enroll_args("http://127.0.0.1:9", GATEWAY_ID);
    args.retain(|argument| argument != "--token-stdin");

    let output = invoke(&workspace, args, None).await;

    assert!(!output.status.success());
    assert!(
        json_envelope(&output)
            .pointer("/error")
            .and_then(Value::as_str)
            .is_some_and(|message| message.contains("use --token-stdin"))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_origin_or_noncanonical_uuid_fails_before_token_persistence() {
    let workspace = ProcessWorkspace::new();
    let invalid_commands = [
        enroll_args_with_scope(
            "http://example.com",
            TENANT_ID,
            PROJECT_ID,
            GATEWAY_ID,
            true,
        ),
        enroll_args_with_scope(
            "http://127.0.0.1:9",
            TENANT_ID,
            PROJECT_ID,
            GATEWAY_ID,
            false,
        ),
        enroll_args_with_scope(
            "https://example.com",
            "11111111-1111-4111-8111-11111111111A",
            PROJECT_ID,
            GATEWAY_ID,
            false,
        ),
    ];

    for command in invalid_commands {
        let output = invoke(&workspace, command, Some(&format!("{TOKEN}\n"))).await;
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stdout).contains(TOKEN));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(TOKEN));
        assert!(
            !workspace.identity_directory().exists(),
            "invalid target must not create identity state"
        );
    }
}
