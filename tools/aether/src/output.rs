//! Output and HTTP-error helpers for aether commands.
//!
//! Two responsibilities live here:
//!
//! 1. stdout envelope formatting, a consistent shape for all commands:
//!    - `print_success` / `print_error` / `print_ok` — the `--json` envelope
//!      (`{"success": true, "data": ...}` / `{"success": false, "error": ...}`).
//!    - `print_value` — a read command's payload: the `{success, data}` envelope
//!      in `--json` mode, else pretty-printed JSON. The shared read-arm helper
//!      across `net.rs`, `alarms.rs`, `channels.rs`, and the Task 10 modules.
//!    - `print_action` / `action_message` — the same envelope logic specialized
//!      for an action-only command (update/delete/enable/…): in `--json` mode
//!      `print_action` forwards the server's response through `print_success`,
//!      and in human mode it prints the server's `message` (via `action_message`)
//!      or a caller-supplied fallback. Called from the command handlers
//!      (`net.rs`, `alarms.rs`, and the Task 8–10 modules).
//!
//! 2. `parse_error_body` — turns a non-2xx HTTP response from a backend service
//!    into an `anyhow::Error` that carries the server's own message. Unlike the
//!    print helpers this runs on error paths regardless of `--json`, and is
//!    called from the HTTP clients (`net.rs`, `alarms.rs`, `channels.rs`,
//!    `models/client.rs`, …), not just from `main.rs`.

use serde::Serialize;

/// Print `{"success": true, "data": ...}` to stdout
pub fn print_success(data: impl Serialize) {
    let envelope = serde_json::json!({ "success": true, "data": data });
    match serde_json::to_string_pretty(&envelope) {
        Ok(s) => println!("{s}"),
        Err(e) => println!(r#"{{"success":false,"error":"serialize error: {e}"}}"#),
    }
}

/// Print `{"success": false, "error": "..."}` to stdout
pub fn print_error(msg: &str) {
    let envelope = serde_json::json!({ "success": false, "error": msg });
    if let Ok(s) = serde_json::to_string_pretty(&envelope) {
        println!("{s}");
    }
}

/// Print `{"success": true, "data": null}` for action-only commands
pub fn print_ok() {
    println!(r#"{{"success":true,"data":null}}"#);
}

/// Print an action-only command's result (update/delete/enable/disable/…).
///
/// Unlike a `create`, these endpoints return a small ack envelope rather than
/// a payload worth tabulating. In `--json` mode we forward the server's
/// response verbatim as the `data` of the CLI's `{success, data}` envelope, so
/// a script still reads the server's own fields instead of a bare `null`. In
/// human mode we print the server's `message` when it is a string, else
/// `fallback`. Whether the server's message carries information beyond the
/// fallback is a per-endpoint fact — see the call sites for that reasoning.
pub fn print_action(data: &serde_json::Value, fallback: &str, json: bool) {
    if json {
        print_success(data);
    } else {
        println!("{}", action_message(data, fallback));
    }
}

/// Pick the human line for an action: the server's `message` field when present
/// as a string, else `fallback`. Split out so the selection logic is
/// unit-testable without capturing stdout.
pub fn action_message<'a>(data: &'a serde_json::Value, fallback: &'a str) -> &'a str {
    data.get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(fallback)
}

/// Print a read command's JSON payload: the `{success, data}` envelope in
/// `--json` mode, else pretty-printed JSON. This is the de-facto shape of every
/// read arm across the command modules (net/alarms/channels/models).
///
/// Serializing a `serde_json::Value` cannot actually fail, so the human branch
/// swallows the theoretical error rather than propagating it — matching the
/// swallowing style of `print_success`/`print_error` (and the former private
/// `net::print_value`, not the `?`-propagating inline copies it replaces).
pub fn print_value(data: &serde_json::Value, json: bool) {
    if json {
        print_success(data);
    } else if let Ok(s) = serde_json::to_string_pretty(data) {
        println!("{s}");
    }
}

/// Turn a non-2xx response into an `anyhow::Error` that carries the server's own message.
///
/// AetherEMS services return two different JSON error shapes:
///   typed  — io (`AppError`), automation (`AutomationError`):
///            `{"success":false,"error":{"code":..,"message":..,"suggestion":..}}`
///   inline — alarm, uplink:
///            `{"success":false,"message":..,"data":null}`
///
/// Not every error body is JSON: axum's `Json<T>` extractor rejects a wrong-shape
/// request with a `text/plain` 422 whose body is a useful deserialization message
/// (e.g. `missing field \`broker_port\``). A non-JSON body is surfaced raw (trimmed
/// and truncated to `MAX_RAW_BODY` chars) rather than discarded.
///
/// Falls back to the bare status code only when the body is empty or unreadable.
pub async fn parse_error_body(context: &str, resp: reqwest::Response) -> anyhow::Error {
    let status = resp.status();

    // Read the body as text first: `resp.json()` consumes it, so a failed JSON
    // parse would otherwise leave nothing to fall back on.
    let Ok(text) = resp.text().await else {
        return anyhow::anyhow!("{context}: HTTP {status}");
    };

    if let Ok(body) = serde_json::from_str::<serde_json::Value>(&text) {
        let typed = body.get("error");
        let message = typed
            .and_then(|e| e.get("message"))
            .or_else(|| body.get("message"))
            .and_then(serde_json::Value::as_str);
        let suggestion = typed
            .and_then(|e| e.get("suggestion"))
            .and_then(serde_json::Value::as_str);

        // A `suggestion` present without a `message` is treated as no message at
        // all (the `(None, _)` arm drops the suggestion). This is safe only because
        // `common::api_types::ErrorInfo.message` is a mandatory Rust `String`, so any
        // `error` object always carries a string `message`; the inline-shape services
        // (alarm/uplink) never emit an `error` key or a `suggestion`. So the lossy
        // case is unreachable through the four real services.
        return match (message, suggestion) {
            (Some(m), Some(s)) => {
                anyhow::anyhow!("{context}: HTTP {status} — {m} (suggestion: {s})")
            },
            (Some(m), None) => anyhow::anyhow!("{context}: HTTP {status} — {m}"),
            (None, _) => anyhow::anyhow!("{context}: HTTP {status}"),
        };
    }

    // Non-JSON body (e.g. the text/plain 422 above). Surface it, but bound the
    // length so a stray HTML error page can't flood the terminal.
    const MAX_RAW_BODY: usize = 300;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        anyhow::anyhow!("{context}: HTTP {status}")
    } else if trimmed.chars().count() > MAX_RAW_BODY {
        let truncated: String = trimmed.chars().take(MAX_RAW_BODY).collect();
        anyhow::anyhow!("{context}: HTTP {status} — {truncated}…")
    } else {
        anyhow::anyhow!("{context}: HTTP {status} — {trimmed}")
    }
}

#[cfg(test)]
mod tests {
    use super::{action_message, parse_error_body};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn action_message_prefers_server_message() {
        // uplink's cert-delete returns HTTP 200 with different messages for a real
        // delete vs a no-op. The no-op message must reach the human, not "deleted".
        let no_op = serde_json::json!({
            "success": true,
            "message": "File does not exist, nothing to delete",
            "data": null,
        });
        assert_eq!(
            action_message(&no_op, "Certificate ca_cert deleted"),
            "File does not exist, nothing to delete"
        );

        let deleted = serde_json::json!({
            "success": true,
            "message": "Deleted successfully",
            "data": { "deleted": "AmazonRootCA1.pem" },
        });
        assert_eq!(
            action_message(&deleted, "Certificate ca_cert deleted"),
            "Deleted successfully"
        );
    }

    #[test]
    fn action_message_falls_back_when_message_absent_or_non_string() {
        let no_message = serde_json::json!({ "success": true, "data": null });
        assert_eq!(
            action_message(&no_message, "fallback used"),
            "fallback used"
        );

        let non_string_message = serde_json::json!({ "message": 42 });
        assert_eq!(
            action_message(&non_string_message, "fallback used"),
            "fallback used"
        );
    }

    /// Serve `template` at GET /x and fetch it.
    ///
    /// Returns `(MockServer, Response)` so the caller binds `_server` and keeps
    /// the mock alive for the whole test scope. This is cheap defensive practice
    /// against future changes to wiremock's shutdown semantics — NOT a workaround
    /// for a known failure: wiremock 0.6.5's `Drop` shuts down gracefully (in-flight
    /// connections finish), so dropping the server before `resp.json()` still reads
    /// the body fine. What actually gives these tests teeth is the mutation check —
    /// forcing `parse_error_body` to always take its fallback branch fails 3 of the
    /// 4 tests (the 4th exercises the fallback by design).
    async fn serve(template: ResponseTemplate) -> (MockServer, reqwest::Response) {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .respond_with(template)
            .mount(&server)
            .await;
        let resp = reqwest::get(format!("{}/x", server.uri())).await.unwrap();
        (server, resp)
    }

    #[tokio::test]
    async fn typed_shape_yields_message_and_suggestion() {
        let (_server, resp) = serve(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "success": false,
            "error": {
                "code": "INVALID_POINT",
                "message": "point 999 out of range",
                "suggestion": "run provision first"
            }
        })))
        .await;

        let msg = parse_error_body("Failed to write point", resp)
            .await
            .to_string();

        assert!(msg.contains("Failed to write point"), "{msg}");
        assert!(msg.contains("400"), "{msg}");
        assert!(msg.contains("point 999 out of range"), "{msg}");
        assert!(msg.contains("run provision first"), "{msg}");
    }

    #[tokio::test]
    async fn inline_shape_yields_top_level_message() {
        let (_server, resp) = serve(ResponseTemplate::new(404).set_body_json(
            serde_json::json!({ "success": false, "message": "Rule 7 not found", "data": null }),
        ))
        .await;

        let msg = parse_error_body("Failed to get rule", resp)
            .await
            .to_string();

        assert!(msg.contains("Rule 7 not found"), "{msg}");
        assert!(msg.contains("404"), "{msg}");
    }

    #[tokio::test]
    async fn typed_shape_without_suggestion_omits_it() {
        let (_server, resp) = serve(ResponseTemplate::new(503).set_body_json(serde_json::json!({
            "success": false,
            "error": { "code": "CHANNEL_OFFLINE", "message": "channel 1001 offline" }
        })))
        .await;

        let msg = parse_error_body("Failed to execute action", resp)
            .await
            .to_string();

        assert!(msg.contains("channel 1001 offline"), "{msg}");
        assert!(!msg.contains("suggestion"), "{msg}");
    }

    #[tokio::test]
    async fn non_json_body_is_surfaced_not_discarded() {
        // axum's Json<T> extractor rejects a wrong-shape body with a text/plain
        // 422 whose message is the real diagnostic — it must reach the user.
        let (_server, resp) = serve(ResponseTemplate::new(422).set_body_string(
            "Failed to deserialize the JSON body into the target type: missing field `broker_port`",
        ))
        .await;

        let msg = parse_error_body("Failed to update uplink config", resp)
            .await
            .to_string();

        assert!(msg.contains("Failed to update uplink config"), "{msg}");
        assert!(msg.contains("422"), "{msg}");
        assert!(msg.contains("missing field `broker_port`"), "{msg}");
    }

    #[tokio::test]
    async fn empty_non_json_body_falls_back_to_status_code() {
        let (_server, resp) = serve(ResponseTemplate::new(503).set_body_string("")).await;

        let msg = parse_error_body("Failed to reach uplink", resp)
            .await
            .to_string();

        assert!(msg.contains("Failed to reach uplink"), "{msg}");
        assert!(msg.contains("503"), "{msg}");
        // Nothing but context + status; no trailing " — " separator for a body.
        assert!(!msg.contains('—'), "{msg}");
    }

    #[tokio::test]
    async fn non_json_body_is_truncated_by_chars_not_bytes() {
        // "测" is 3 bytes/char; a leading single-byte 'A' shifts every later
        // char boundary off any multiple of 3, so byte offset 300 (bytes[300])
        // lands mid-codepoint — NOT a valid char boundary. A byte-based
        // `&trimmed[..300]` would panic on this body; `chars().take(300)`
        // must not. `tail_marker` uses characters absent from the filler, so
        // "message doesn't contain the tail" can't pass by coincidence.
        let filler = "测".repeat(299);
        let tail_marker = "尾巴标记";
        let body = format!("A{filler}{tail_marker}");
        assert!(
            !body.is_char_boundary(300),
            "test body must straddle byte 300 to pin char-based truncation"
        );

        let (_server, resp) = serve(ResponseTemplate::new(422).set_body_string(body.clone())).await;

        let msg = parse_error_body("Failed to parse uplink response", resp)
            .await
            .to_string();

        let expected_prefix: String = body.chars().take(300).collect();
        assert!(msg.contains(&expected_prefix), "{msg}");
        assert!(!msg.contains(tail_marker), "{msg}");
        assert!(msg.ends_with('…'), "{msg}");
    }
}
