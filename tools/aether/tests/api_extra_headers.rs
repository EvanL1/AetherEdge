use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const CLIENT_ID: &str = "placeholder-id.access";
const CLIENT_SECRET: &str = "placeholder-secret-never-print";
const CHANNELS_PATH: &str = "/api/v1/io/api/channels";
const RECONCILE_PATH: &str = "/api/v1/io/api/channels/reconcile";

fn access_headers() -> String {
    format!("CF-Access-Client-Id={CLIENT_ID}, CF-Access-Client-Secret={CLIENT_SECRET}")
}

fn aether(api_url: &str, api_headers: Option<&str>) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_aether"));
    for variable in [
        "AETHER_API_HEADERS",
        "AETHER_ACCESS_TOKEN",
        "AETHER_JSON",
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "NO_PROXY",
        "no_proxy",
    ] {
        command.env_remove(variable);
    }
    command.env("AETHER_API_URL", api_url);
    if let Some(headers) = api_headers {
        command.env("AETHER_API_HEADERS", headers);
    }
    command
}

async fn run(mut command: Command) -> Output {
    tokio::task::spawn_blocking(move || command.output().expect("run aether"))
        .await
        .expect("join aether process")
}

fn rendered(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

async fn serve_channel_list(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path(CHANNELS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "channels": [] })))
        .mount(server)
        .await;
}

async fn channel_list_server() -> MockServer {
    let server = MockServer::start().await;
    serve_channel_list(&server).await;
    server
}

async fn catch_all_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "channels": [] })))
        .mount(&server)
        .await;
    server
}

fn through_proxy(command: &mut Command, proxy: &MockServer) {
    for variable in [
        "http_proxy",
        "HTTP_PROXY",
        "https_proxy",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "all_proxy",
    ] {
        command.env(variable, proxy.uri());
    }
}

async fn requests(server: &MockServer) -> Vec<Request> {
    server
        .received_requests()
        .await
        .expect("request recording is enabled")
}

fn header<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
    request
        .headers
        .get(name)
        .and_then(|value| value.to_str().ok())
}

fn header_names(request: &Request) -> BTreeSet<String> {
    request
        .headers
        .keys()
        .map(|name| name.as_str().to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extra_headers_reach_the_gateway_on_a_query() {
    let server = channel_list_server().await;

    let mut command = aether(&server.uri(), Some(&access_headers()));
    command.args(["--verbose", "--json", "channels", "list"]);
    let output = run(command).await;

    assert!(output.status.success(), "{}", rendered(&output));
    let received = requests(&server).await;
    assert_eq!(received.len(), 1);
    assert_eq!(header(&received[0], "cf-access-client-id"), Some(CLIENT_ID));
    assert_eq!(
        header(&received[0], "cf-access-client-secret"),
        Some(CLIENT_SECRET)
    );
    assert!(!rendered(&output).contains(CLIENT_SECRET));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_gateway_client_sends_the_extra_headers() {
    for arguments in [
        vec!["channels", "list"],
        vec!["channels", "points", "list", "1001"],
        vec!["alarms", "list"],
        vec!["rules", "list"],
        vec!["routing", "list"],
        vec!["history", "health"],
        vec!["models", "instances", "list"],
        vec!["templates", "list"],
        vec!["net", "mqtt", "status"],
    ] {
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;

        let mut command = aether(&server.uri(), Some(&access_headers()));
        command.arg("--json").args(&arguments);
        let output = run(command).await;

        let received = requests(&server).await;
        assert!(!received.is_empty(), "{arguments:?}: {}", rendered(&output));
        for request in &received {
            assert_eq!(
                header(request, "cf-access-client-secret"),
                Some(CLIENT_SECRET),
                "{arguments:?}: {}",
                request.url
            );
        }
        assert!(!rendered(&output).contains(CLIENT_SECRET), "{arguments:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extra_headers_accompany_the_governed_command_headers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(RECONCILE_PATH))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;

    let mut command = aether(&server.uri(), Some(&access_headers()));
    command
        .env("AETHER_ACCESS_TOKEN", "signed-access-token")
        .args(["--json", "channels", "reconcile", "--confirmed"]);
    let output = run(command).await;

    let received = requests(&server).await;
    assert_eq!(received.len(), 1, "{}", rendered(&output));
    let request = &received[0];
    assert_eq!(header(request, "cf-access-client-id"), Some(CLIENT_ID));
    assert_eq!(
        header(request, "cf-access-client-secret"),
        Some(CLIENT_SECRET)
    );
    assert_eq!(
        header(request, "authorization"),
        Some("Bearer signed-access-token")
    );
    assert_eq!(header(request, "x-aether-confirmed"), Some("true"));
    assert!(header(request, "x-request-id").is_some());
    assert!(!rendered(&output).contains(CLIENT_SECRET));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn environment_cannot_supply_the_confirmation_header() {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    let forged = format!("X-Aether-Confirmed=true,X-Probe={CLIENT_SECRET}");

    for arguments in [
        vec!["channels", "reconcile"],
        vec!["channels", "reconcile", "--confirmed"],
        vec!["channels", "list"],
        vec!["--json", "alarms", "list"],
    ] {
        let mut command = aether(&server.uri(), Some(&forged));
        command
            .env("AETHER_ACCESS_TOKEN", "signed-access-token")
            .args(&arguments);
        let output = run(command).await;
        let text = rendered(&output);

        assert!(!output.status.success(), "{arguments:?}: {text}");
        assert!(text.contains("AETHER_API_HEADERS"), "{arguments:?}: {text}");
        assert!(text.contains("x-aether-confirmed"), "{arguments:?}: {text}");
        assert!(!text.contains(CLIENT_SECRET), "{arguments:?}: {text}");
    }

    assert!(requests(&server).await.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_headers_fail_before_any_request_without_echoing_values() {
    let server = channel_list_server().await;

    for malformed in [
        format!("CF-Access-Client-Id={CLIENT_ID},{CLIENT_SECRET}"),
        format!("CF-Access-Client-Secret={CLIENT_SECRET}\nX-Injected: 1"),
        format!("bad name={CLIENT_SECRET}"),
        format!("X-Probe={CLIENT_SECRET},x-probe={CLIENT_SECRET}"),
        format!("Authorization=Bearer {CLIENT_SECRET}"),
    ] {
        let mut command = aether(&server.uri(), Some(&malformed));
        command.args(["--json", "channels", "list"]);
        let output = run(command).await;
        let text = rendered(&output);

        assert!(!output.status.success(), "{text}");
        assert!(!text.contains(CLIENT_SECRET), "{text}");
        let envelope: Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("expected one JSON envelope: {error}\n{text}"));
        assert_eq!(envelope["success"], json!(false));
        assert!(
            envelope["error"]
                .as_str()
                .is_some_and(|message| message.contains("AETHER_API_HEADERS")),
            "{text}"
        );
    }

    assert!(requests(&server).await.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unset_and_empty_variable_send_identical_requests() {
    let server = channel_list_server().await;

    for headers in [None, Some(""), Some(" , ,, ")] {
        let mut command = aether(&server.uri(), headers);
        command.args(["--json", "channels", "list"]);
        let output = run(command).await;
        assert!(
            output.status.success(),
            "{headers:?}: {}",
            rendered(&output)
        );
    }

    let received = requests(&server).await;
    assert_eq!(received.len(), 3);
    let baseline = header_names(&received[0]);
    assert!(!baseline.contains("cf-access-client-id"));
    for request in &received[1..] {
        assert_eq!(header_names(request), baseline);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extra_headers_are_refused_on_remote_plaintext_http() {
    let mut command = aether("http://edge.invalid:6005", Some(&access_headers()));
    command.args(["--json", "channels", "list"]);
    let output = run(command).await;
    let text = rendered(&output);

    assert!(!output.status.success(), "{text}");
    assert!(text.contains("non-loopback plaintext"), "{text}");
    assert!(text.contains("AETHER_API_HEADERS"), "{text}");
    assert!(!text.contains(CLIENT_SECRET), "{text}");
}

async fn redirecting_gateway(target: &MockServer) -> MockServer {
    let gateway = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(CHANNELS_PATH))
        .respond_with(
            ResponseTemplate::new(307)
                .insert_header("location", format!("{}{CHANNELS_PATH}", target.uri())),
        )
        .mount(&gateway)
        .await;
    gateway
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extra_headers_never_follow_a_redirect_to_another_origin() {
    let other_origin = channel_list_server().await;
    let gateway = redirecting_gateway(&other_origin).await;

    let mut command = aether(&gateway.uri(), Some(&access_headers()));
    command.args(["--json", "channels", "list"]);
    let output = run(command).await;
    let text = rendered(&output);

    assert!(!output.status.success(), "{text}");
    assert_eq!(requests(&gateway).await.len(), 1);
    assert!(requests(&other_origin).await.is_empty(), "{text}");
    assert!(text.contains("AETHER_API_HEADERS"), "{text}");
    assert!(!text.contains(CLIENT_SECRET), "{text}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redirects_without_extra_headers_keep_their_existing_behaviour() {
    let other_origin = channel_list_server().await;
    let gateway = redirecting_gateway(&other_origin).await;

    let mut command = aether(&gateway.uri(), None);
    command.args(["--json", "channels", "list"]);
    let output = run(command).await;

    assert!(output.status.success(), "{}", rendered(&output));
    assert_eq!(requests(&other_origin).await.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_origin_redirects_keep_the_extra_headers() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(CHANNELS_PATH))
        .respond_with(ResponseTemplate::new(307).insert_header("location", "/moved"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/moved"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "channels": [] })))
        .mount(&server)
        .await;

    let mut command = aether(&server.uri(), Some(&access_headers()));
    command.args(["--json", "channels", "list"]);
    let output = run(command).await;

    assert!(output.status.success(), "{}", rendered(&output));
    let received = requests(&server).await;
    assert_eq!(received.len(), 2);
    assert_eq!(
        header(&received[1], "cf-access-client-secret"),
        Some(CLIENT_SECRET)
    );
}

fn mcp_command(api_url: &str, api_headers: &str, workspace: &TempDir) -> Command {
    let mut command = aether(api_url, Some(api_headers));
    command
        .arg("--config-path")
        .arg(workspace.path().join("config"))
        .arg("--db-path")
        .arg(workspace.path().join("data"))
        .arg("mcp");
    command
}

fn call_channels_list_over_stdio(mut command: Command) -> Value {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn aether mcp");
    let mut stdin = child.stdin.take().expect("mcp stdin");
    let stdout = child.stdout.take().expect("mcp stdout");

    for frame in [
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": { "name": "api-extra-headers-test", "version": "0" }
            }
        }),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": { "name": "channels_list", "arguments": {} }
        }),
    ] {
        writeln!(stdin, "{frame}").expect("write MCP frame");
    }
    stdin.flush().expect("flush MCP frames");

    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let Ok(frame) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if frame["id"] == json!(2) {
                let _ = sender.send(frame);
                return;
            }
        }
    });
    let response = receiver.recv_timeout(Duration::from_secs(60));
    let _ = child.kill();
    let _ = child.wait();
    response.expect("MCP tools/call response")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_server_requests_carry_the_extra_headers() {
    let server = channel_list_server().await;
    let workspace = TempDir::new().expect("create workspace");
    let command = mcp_command(&server.uri(), &access_headers(), &workspace);

    let response = tokio::task::spawn_blocking(move || call_channels_list_over_stdio(command))
        .await
        .expect("join MCP session");

    assert!(!response.to_string().contains(CLIENT_SECRET), "{response}");
    let received = requests(&server).await;
    assert_eq!(received.len(), 1, "{response}");
    assert_eq!(header(&received[0], "cf-access-client-id"), Some(CLIENT_ID));
    assert_eq!(
        header(&received[0], "cf-access-client-secret"),
        Some(CLIENT_SECRET)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_server_refuses_to_start_with_a_reserved_header() {
    let server = channel_list_server().await;
    let workspace = TempDir::new().expect("create workspace");
    let mut command = mcp_command(
        &server.uri(),
        &format!("x-AETHER-confirmed=true,X-Probe={CLIENT_SECRET}"),
        &workspace,
    );
    command.arg("--allow-write").stdin(Stdio::null());
    let output = run(command).await;
    let text = rendered(&output);

    assert!(!output.status.success(), "{text}");
    assert!(text.contains("x-aether-confirmed"), "{text}");
    assert!(!text.contains(CLIENT_SECRET), "{text}");
    assert!(requests(&server).await.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loopback_gateway_requests_never_go_through_a_proxy() {
    let proxy = catch_all_server().await;
    let ipv4 = channel_list_server().await;
    let mut gateways = vec![
        (ipv4.uri(), &ipv4),
        (ipv4.uri().replace("127.0.0.1", "localhost"), &ipv4),
    ];
    let ipv6 = match std::net::TcpListener::bind("[::1]:0") {
        Ok(listener) => {
            let server = MockServer::builder().listener(listener).start().await;
            serve_channel_list(&server).await;
            Some(server)
        },
        Err(error) => {
            eprintln!("skipping the [::1] gateway: {error}");
            None
        },
    };
    if let Some(server) = &ipv6 {
        assert!(
            server.uri().starts_with("http://[::1]:"),
            "{}",
            server.uri()
        );
        gateways.push((server.uri(), server));
    }

    for (api_url, gateway) in gateways {
        let credentials = [
            (Some(access_headers()), None),
            (None, Some("signed-access-token")),
        ];
        for (api_headers, access_token) in credentials {
            let before = requests(gateway).await.len();
            let mut command = aether(&api_url, api_headers.as_deref());
            through_proxy(&mut command, &proxy);
            if let Some(token) = access_token {
                command.env("AETHER_ACCESS_TOKEN", token);
            }
            command.args(["--json", "channels", "list"]);
            let output = run(command).await;
            let context = format!(
                "{api_url} headers={} token={}: {}",
                api_headers.is_some(),
                access_token.is_some(),
                rendered(&output)
            );

            assert!(requests(&proxy).await.is_empty(), "{context}");
            assert!(output.status.success(), "{context}");
            let received = requests(gateway).await;
            assert_eq!(received.len(), before + 1, "{context}");
            let request = &received[before];
            if api_headers.is_some() {
                assert_eq!(
                    header(request, "cf-access-client-secret"),
                    Some(CLIENT_SECRET),
                    "{context}"
                );
            }
            if let Some(token) = access_token {
                assert_eq!(
                    header(request, "authorization"),
                    Some(format!("Bearer {token}").as_str()),
                    "{context}"
                );
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_https_gateway_requests_still_honour_the_proxy() {
    let proxy = catch_all_server().await;

    let mut command = aether("https://edge.invalid", Some(&access_headers()));
    through_proxy(&mut command, &proxy);
    command.args(["--json", "channels", "list"]);
    let output = run(command).await;

    let received = requests(&proxy).await;
    assert!(!received.is_empty(), "{}", rendered(&output));
    for request in &received {
        assert_eq!(request.method.as_str(), "CONNECT");
        assert!(header(request, "cf-access-client-secret").is_none());
    }
    assert!(!rendered(&output).contains(CLIENT_SECRET));
}
