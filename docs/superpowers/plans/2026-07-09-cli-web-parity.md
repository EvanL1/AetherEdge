# CLI / Web 功能对等 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 给 `aether` CLI 补齐相对 Vue 前端缺失的能力（netsrv MQTT/证书、告警规则写操作、channels·instances 零碎缺口），使两者功能对等。

**Architecture:** 不新增 service/crate/抽象层。新建一个模块 `tools/aether/src/net.rs`，照抄现有 8 个 `XClient` 的形状（`XCommands` enum + `handle_command` + 私有 `XClient`）；其余在 `alarms.rs`、`channels.rs`、`models.rs` 原地扩展。所有变更经 HTTP 打到各服务，不碰 Redis/SQLite。

**Tech Stack:** Rust 1.90 · clap 4（derive）· reqwest 0.12（新增 `multipart` feature）· anyhow · serde_json · wiremock 0.6（新增 dev-dep）

**Spec:** `docs/superpowers/specs/2026-07-09-cli-web-parity-design.md`

---

## File Structure

| 文件 | 动作 | 职责 |
|---|---|---|
| `Cargo.toml` (workspace root) | Modify | 给 `reqwest` 加 `multipart` feature |
| `workspace-hack/Cargo.toml` | Regenerate | `cargo hakari generate` 产出，勿手改 |
| `tools/aether/Cargo.toml` | Modify | 首次引入 `[dev-dependencies]`（wiremock） |
| `tools/aether/src/output.rs` | Modify | 新增 `parse_error_body`，解析服务端错误体 |
| `tools/aether/src/net.rs` | **Create** | netsrv 的 `NetCommands` + `NetClient` |
| `tools/aether/src/main.rs` | Modify | `mod net;` + `Commands::Net` variant + dispatch arm |
| `tools/aether/src/alarms.rs` | Modify | 告警规则写操作（5 个子命令 + client 方法） |
| `tools/aether/src/channels.rs` | Modify | enabled / mappings / unmapped-points / write / points batch / point-mapping |
| `tools/aether/src/models.rs`, `models/client.rs` | Modify | instances action / measurement |
| `CLAUDE.md` | Modify | 修正「alarmsrv 用 AppError」这一错误陈述 |

**测试位置**：`AlarmClient` / `ChannelClient` / `NetClient` 都是**模块私有**，因此测试必须写成同文件内的 `#[cfg(test)] mod tests`（子模块可访问父模块私有项）。**不能**放进 `tools/aether/tests/`。

---

## 关键事实（实施前必读）

这些是调查代码得出的，与直觉或 CLAUDE.md 的表述不一致，写代码时会踩：

1. **错误体有三种形状，不是两种。** `parse_error_body` 必须全部处理：
   - **typed**（comsrv 经 `AppError`、modsrv 经 `ModSrvError`）：`{"success":false,"error":{"code":..,"message":..,"suggestion":..}}`
   - **inline**（alarmsrv 的 `bad_request`/`not_found`/`server_error`，netsrv 的 handler）：`{"success":false,"message":..,"data":null}`
   - **非 JSON 纯文本**（axum `Json<T>` 提取失败时的 rejection）：`422` + `content-type: text/plain` + `Failed to deserialize the JSON body into the target type: missing field \`broker_port\` …`

   第三种是 Task 4–6 code review 发现的。初版 `parse_error_body` 只在 body 是合法 JSON 时提取 message，因此把 axum 那句极有用的诊断整个丢掉，用户只看到 `HTTP 422`——**正是这个函数存在的意义所要消灭的情形**。已修正：非 JSON body 若非空，trim 后截断至 300 字符附于错误信息末尾。

   CLAUDE.md 目前称 alarmsrv 走 `AppError`——**这是错的**，`services/alarmsrv/src/` 中不存在 `AppError` 引用。Task 1 一并修正该文档。

2. **同语义、不同 HTTP 方法。** 逐个照抄，不要推广：
   - modsrv 规则启停：`POST /api/rules/{id}/enable`
   - alarmsrv 规则启停：`PATCH /alarmApi/rules/{id}/enable`

3. **`Client::new()` 不设 timeout。** 现有 8 个 client 都这么写（`alarms.rs:439`）。新代码保持一致，不要顺手加 `Client::builder().timeout(..)`。

4. **clippy 两遍，规则不同。**
   - `cargo clippy --all-targets --all-features -- -D warnings`（含测试）
   - `cargo clippy --workspace --lib --bins -- -D clippy::unwrap_used -D clippy::expect_used`（**不含测试**）

   所以：**运行时代码不得出现 `unwrap`/`expect`；测试里随便用**。

5. **新模块必须同时在 `main.rs` 里挂上。** 只建 `net.rs` 不 wiring，会因 dead_code 触发 `-D warnings` 而构建失败。

   同理，**未被调用的新 `pub fn` 在 bin crate 里也会触发 `dead_code`**（bin crate 不像 lib crate 那样对 `pub` 项网开一面）。Task 1 的 `parse_error_body` 因此临时带了 `#[allow(dead_code)]`。**Task 3 是第一个调用它的任务，必须把该属性删掉。**

6. **`aether` 是 bin-only crate，没有 lib target。**
   - `cargo test -p aether --lib` 会直接报 `no library targets found`。用 `cargo test -p aether <filter>`。
   - 更要紧的是：`quick-check.sh` 跑的是 `cargo test --workspace --lib`，**不带 `--bins`**，所以 aether 的 73 个既有单元测试在本地检查里从未被执行。而 CI（`.github/workflows/rust-check.yml:117`）跑的是 `cargo nextest run --workspace --lib --bins`，是跑的。两者覆盖面不一致。
   - Task 11 会修正 `quick-check.sh` 以对齐 CI。在那之前，各任务用 `cargo test -p aether` 自行验证。

6. **`points/batch` 与 `mqtt config set` 的请求体是复杂嵌套结构**（`PointBatchRequest{create[],update[],delete[]}`、完整 `NetConfig`）。CLI 一律用 `--file <json>` 传原始 JSON，不为它们造 flag。

7. **输出约定（Task 4–7 确立，`output.rs` 提供，Task 8–10 直接复用）**：
   - 有载荷的查询命令 → `print_value(&data, json)`
   - 动作型命令（写入、启停、删除）→ `crate::output::print_action(&data, fallback, json)`
     - `--json`：输出 `{success:true, data: <服务器响应>}`，**不要** `print_ok()`——那会把服务器返回的 `data.saved_as`、`data.deleted`、`{rule_id}` 等字段抹成 `null`
     - 人类模式：优先打印服务器自己的 `message`，仅当服务器没给时才用 `fallback`
   - `--file` 参数的 io 错误与 JSON 解析错误**都要带上文件路径**。裸 `No such file or directory (os error 2)` 是我们要消灭的东西。

8. **wiremock 测试：一个 mock server 只挂一个待验证路径。**

   反例（真实踩过）：`enable_and_disable_use_patch_not_post` 在同一 server 上同时挂 `/enable` 和 `/disable`，各 `.expect(1)`，然后连续调用两次。它只验证"两条路径各被打了一次"，**不关联哪次调用打了哪条**。把 `set_rule_enabled` 里的 enable/disable 字符串对调（语义完全反了，`rule-enable` 会去禁用规则），测试照样绿。

   正确做法是拆成两个测试，每个只挂一个 mock——路径打错就会 404，测试立刻失败。凡是"同一方法按参数走不同路径"的场景，都必须这样测。

9. **每个 client 方法都要有错误路径测试。** `parse_error_body` 在 `output.rs` 里被测得很透，但那只证明**解析器**能用，不证明某个方法**真的调用了它**。

   实证：把 `update_rule` 的 `else` 分支改成无条件 `Ok(json!({}))`（吞掉 404/500），全部 92 个测试照样通过。用户会看到假的成功。

   所以每个新增 client 方法至少两个测试：一个成功路径（断言 method + path + body），一个错误路径（mock 返回非 2xx + 该服务的错误体形状，断言错误信息里含服务器的 message）。

10. **不要把 gate 命令管道给 `head`/`tail`。** 那会吞掉退出码，让失败看起来像成功。要退出码就直接读 `$?`。（这个坑在本项目已经踩过一次。）

11. **clap 的 `bool` 字段永远不能做位置参数。**

    clap-derive 对任何 `bool` 字段自动推断 `ArgAction::SetTrue`（因为 bool 通常是 `--flag`）。位置参数不能是 `SetTrue`：
    - **debug build**：`debug_assert` 直接 panic —— `Argument 'enabled' is positional and it must take a value but action is SetTrue`
    - **release build**：断言被编译掉，退化成零值标志。`aether channels enabled 1` 静默地把 `enabled` 设成 `false`（bool 默认值），**没有任何写法能设成 true**。

    实际发生过：`ChannelCommands::Enabled { channel_id, enabled: bool }`。13 个单测、两遍 clippy、fmt 全绿——因为**所有测试都直接调 client 方法，从不经过 clap**。整个参数解析层从未被执行过。

    修法是改成两个子命令（`channels enable` / `channels disable`），与 `alarms rule-enable` / `rule-disable` 一致，彻底避开 bool。

12. **共享 `CARGO_TARGET_DIR` 时，不要在临时 worktree 里跑 cargo。**

    `libs/aether-model/build.rs:41` 把**绝对路径**烤进 `OUT_DIR` 里生成的代码：`format!("include_str!({:?})", abs.to_string_lossy())`。若某个临时 worktree 用同一个 `CARGO_TARGET_DIR` 构建过 `aether-model`，`OUT_DIR` 里就留下了指向那个 worktree 的绝对 include 路径。worktree 一删，整个 workspace 编译失败：

    ```
    error: couldn't read .../scratchpad/baseline-check/libs/aether-model/src/products/Battery.json
    ```

    实际发生过（一个 subagent 为量基线建了临时 worktree）。**恢复办法**：`git worktree prune && cargo clean -p aether-model`。分支代码本身没问题，坏的是构建缓存。

13. **必须有一个 `Cli::command().debug_assert()` 测试。**

    ```rust
    #[cfg(test)]
    mod cli_tests {
        use super::Cli;
        use clap::CommandFactory;
        #[test]
        fn cli_definition_is_valid() { Cli::command().debug_assert(); }
    }
    ```

    这是 clap 自带的校验器，一次遍历整份命令定义（全部 17 个顶层命令及其所有子命令与参数），抓住类型系统抓不到的结构性错误。它是这类问题的**权威审计手段**——比 grep 准，且随命令增长自动生效。

---

## Task 1: `parse_error_body` helper + dev-dependencies

**Files:**
- Modify: `tools/aether/Cargo.toml`
- Modify: `tools/aether/src/output.rs`
- Modify: `CLAUDE.md`

- [ ] **Step 1: 加 dev-dependencies**

`tools/aether/Cargo.toml` 目前没有 `[dev-dependencies]` 段。追加到文件末尾：

```toml
[dev-dependencies]
wiremock = "0.6.5"
```

（`tokio` 已是普通依赖且启用 `full` feature，`#[tokio::test]` 可直接用，无需重复声明。）

- [ ] **Step 2: 写失败的测试**

在 `tools/aether/src/output.rs` 末尾追加：

**关于 `MockServer` 的生命周期（已实测校正）**：把 `MockServer` 与 `Response` 一起返回、在测试里绑定为 `_server`，是**防御性写法**，不是必需。

早期版本的本计划断言"`MockServer` 先 drop 会导致 `resp.json()` 失败、测试静默走 fallback 分支从而假绿"。**该断言经实验证伪**：wiremock 0.6.5 的 `Drop` 走的是优雅关闭（`tokio::sync::watch` 通知，in-flight 连接读完才断），server drop 后 body 仍可正常读取。

仍然采用 `(MockServer, Response)` 的返回形状，理由是：绑定 `_server`（而非 `_`）让 server 活到测试作用域结束，不依赖 wiremock 的关闭语义细节，未来 wiremock 行为若变更也不会静默腐化测试。这是**廉价的保险**，不是对已知缺陷的修补。

**真正防止假绿的手段是变异测试**：实现完成后，人为把 `parse_error_body` 改成只返回 fallback 分支，确认 4 个测试中有 3 个失败（第 4 个本就走 fallback，应当仍然通过），再还原。Task 1 的实现者与两位审查者均独立执行过此步骤，观察到的正是 3 failed / 1 passed。

```rust
#[cfg(test)]
mod tests {
    use super::parse_error_body;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Serve `template` at GET /x and fetch it.
    /// Returns the live server so the caller keeps it alive while the body is read.
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

        let msg = parse_error_body("Failed to write point", resp).await.to_string();

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

        let msg = parse_error_body("Failed to get rule", resp).await.to_string();

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

        let msg = parse_error_body("Failed to execute action", resp).await.to_string();

        assert!(msg.contains("channel 1001 offline"), "{msg}");
        assert!(!msg.contains("suggestion"), "{msg}");
    }

    #[tokio::test]
    async fn unparseable_body_falls_back_to_status_code() {
        let (_server, resp) = serve(ResponseTemplate::new(503).set_body_string("upstream down")).await;

        let msg = parse_error_body("Failed to reach netsrv", resp).await.to_string();

        assert!(msg.contains("Failed to reach netsrv"), "{msg}");
        assert!(msg.contains("503"), "{msg}");
    }
}
```

`serve` 接收已构造好的 `ResponseTemplate`（状态码由 `ResponseTemplate::new(status)` 决定），并把 `MockServer` 一并返回。调用方用 `let (_server, resp) = …` 绑定，`_server` 在整个测试函数作用域内存活。

- [ ] **Step 3: 跑测试确认失败**

Run: `cargo test -p aether output::tests`
Expected: FAIL — 编译错误 `cannot find function 'parse_error_body' in this scope`

- [ ] **Step 4: 实现 `parse_error_body`**

在 `tools/aether/src/output.rs` 中，`print_ok()` 之后、`#[cfg(test)] mod tests` 之前插入：

```rust
/// Turn a non-2xx response into an `anyhow::Error` that carries the server's own message.
///
/// AetherEMS services return two different error shapes:
///   typed  — comsrv (`AppError`), modsrv (`ModSrvError`):
///            `{"success":false,"error":{"code":..,"message":..,"suggestion":..}}`
///   inline — alarmsrv, netsrv:
///            `{"success":false,"message":..,"data":null}`
///
/// Falls back to the bare status code when the body is absent or unparseable.
pub async fn parse_error_body(context: &str, resp: reqwest::Response) -> anyhow::Error {
    let status = resp.status();

    let Ok(body) = resp.json::<serde_json::Value>().await else {
        return anyhow::anyhow!("{context}: HTTP {status}");
    };

    let typed = body.get("error");
    let message = typed
        .and_then(|e| e.get("message"))
        .or_else(|| body.get("message"))
        .and_then(serde_json::Value::as_str);
    let suggestion = typed
        .and_then(|e| e.get("suggestion"))
        .and_then(serde_json::Value::as_str);

    match (message, suggestion) {
        (Some(m), Some(s)) => anyhow::anyhow!("{context}: HTTP {status} — {m} (suggestion: {s})"),
        (Some(m), None) => anyhow::anyhow!("{context}: HTTP {status} — {m}"),
        (None, _) => anyhow::anyhow!("{context}: HTTP {status}"),
    }
}
```

注意：用 `let ... else` 而非 `unwrap`，因为运行时代码受 `-D clippy::unwrap_used` 约束。

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p aether output::tests`
Expected: PASS，4 个测试全绿

- [ ] **Step 6: 修正 CLAUDE.md 的错误陈述**

`CLAUDE.md` 的「错误处理」节当前写着：

```
  - 类型化错误（comsrv/modsrv/alarmsrv 经 `common::api_types::AppError`）
```

改为：

```
  - 类型化错误（comsrv 经 `AppError`、modsrv 经 `ModSrvError`）: `{ success: false, error: { code, message, details?, suggestion?, field_errors? } }`
```

并在其下的「handler 内联校验」一行的服务列表中补入 `alarmsrv`（其 `routes.rs` 的 `bad_request`/`not_found`/`server_error` 三个 helper 均返回 `{ success: false, message, data: null }`，全文件无 `AppError` 引用）。

同时把 comsrv 那条 bullet 里的 `alarmsrv` 去掉。

- [ ] **Step 7: 提交**

```bash
git add tools/aether/Cargo.toml tools/aether/src/output.rs CLAUDE.md
git commit -m "feat: add parse_error_body to surface server error messages in CLI

AetherEMS services return two error shapes. Existing CLI clients discard
the body entirely and show only the status code, hiding the server's
'suggestion' field. parse_error_body handles both shapes.

Also corrects CLAUDE.md: alarmsrv does not use AppError; it returns the
inline {success,message,data} shape from bad_request/not_found/server_error."
```

---

## Task 2: 给 reqwest 打开 `multipart` feature

证书上传是 `multipart/form-data`（`services/netsrv/src/models.rs:336`），workspace 的 reqwest 当前关掉了默认 feature，没有 multipart。

**Files:**
- Modify: `Cargo.toml` (workspace root, line 101)
- Regenerate: `workspace-hack/Cargo.toml`

- [ ] **Step 1: 改 workspace 依赖**

`Cargo.toml` 第 101 行，把：

```toml
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
```

改为：

```toml
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls", "multipart"] }
```

- [ ] **Step 2: 重新生成 workspace-hack**

Run: `cargo hakari generate`
Expected: `workspace-hack/Cargo.toml` 出现 diff（reqwest feature 统一）

**不要手改** `workspace-hack/Cargo.toml`。

- [ ] **Step 3: 验证一致性**

Run: `cargo hakari verify`
Expected: 无输出，exit 0

- [ ] **Step 4: 确认全 workspace 仍能编译**

Run: `cargo check --workspace`
Expected: 成功。（多个 crate 依赖 reqwest；加 feature 只增不减，不应破坏任何 crate。）

- [ ] **Step 5: 提交**

```bash
git add Cargo.toml workspace-hack/Cargo.toml Cargo.lock
git commit -m "build: enable reqwest multipart feature for cert upload"
```

---

## Task 3: `net.rs` 骨架 + `mqtt status` + main.rs wiring

**Files:**
- Create: `tools/aether/src/net.rs`
- Modify: `tools/aether/src/main.rs`

必须一次完成 wiring，否则 `net.rs` 是死代码，`-D warnings` 会让构建失败。

- [ ] **Step 1: 写失败的测试**

创建 `tools/aether/src/net.rs`，先只放测试与最小类型：

```rust
//! netsrv management: MQTT connection/config and TLS certificates.

use anyhow::Result;
use clap::Subcommand;
use reqwest::Client;
use serde_json::Value;

use crate::output::parse_error_body;

#[derive(Subcommand)]
pub enum NetCommands {
    /// MQTT connection and configuration
    #[command(subcommand)]
    Mqtt(MqttCommands),
}

#[derive(Subcommand)]
pub enum MqttCommands {
    /// Show MQTT connection status
    #[command(about = "Show MQTT connection status")]
    Status,
}

pub async fn handle_command(cmd: NetCommands, base_url: &str, json: bool) -> Result<()> {
    let client = NetClient::new(base_url)?;
    match cmd {
        NetCommands::Mqtt(MqttCommands::Status) => {
            let data = client.mqtt_status().await?;
            print_value(&data, json);
        },
    }
    Ok(())
}

fn print_value(data: &Value, json: bool) {
    if json {
        crate::output::print_success(data);
    } else if let Ok(s) = serde_json::to_string_pretty(data) {
        println!("{s}");
    }
}

struct NetClient {
    client: Client,
    base_url: String,
}

impl NetClient {
    fn new(base_url: &str) -> Result<Self> {
        Ok(Self {
            client: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::NetClient;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn mqtt_status_gets_the_status_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/netApi/mqtt/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({ "connected": true, "broker": "tcp://1.2.3.4:1883" }),
            ))
            .expect(1)
            .mount(&server)
            .await;

        let client = NetClient::new(&server.uri()).unwrap();
        let v = client.mqtt_status().await.unwrap();

        assert_eq!(v["connected"], true);
    }

    #[tokio::test]
    async fn mqtt_status_surfaces_server_message_on_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/netApi/mqtt/status"))
            .respond_with(ResponseTemplate::new(500).set_body_json(
                serde_json::json!({ "success": false, "message": "broker unreachable" }),
            ))
            .mount(&server)
            .await;

        let client = NetClient::new(&server.uri()).unwrap();
        let err = client.mqtt_status().await.unwrap_err().to_string();

        assert!(err.contains("broker unreachable"), "{err}");
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p aether net::tests`
Expected: FAIL — `no method named 'mqtt_status' found for struct 'NetClient'`

（此时 `main.rs` 尚未 `mod net;`，`cargo test` 会报模块未声明。若如此，先做 Step 4 的 `mod net;` 一行，再回来跑。）

- [ ] **Step 3: 实现 `mqtt_status`**

在 `net.rs` 的 `impl NetClient` 中，`new` 之后加：

```rust
    async fn mqtt_status(&self) -> Result<Value> {
        let resp = self
            .client
            .get(format!("{}/netApi/mqtt/status", self.base_url))
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(parse_error_body("Failed to get MQTT status", resp).await)
        }
    }
```

- [ ] **Step 4: 在 main.rs 挂上模块**

三处改动。

(a) 模块声明——`tools/aether/src/main.rs` 第 13 行 `mod models;` 之后插入（保持字母序）：

```rust
mod net;
```

(b) `Commands` enum 中，`Alarms {...}` variant 之后插入：

```rust
    /// Manage netsrv: MQTT connection/config and TLS certificates
    #[command(about = "Manage MQTT connection, netsrv config, and TLS certificates")]
    Net {
        #[command(subcommand)]
        command: net::NetCommands,
    },
```

(c) dispatch match 中，`Commands::Alarms {...}` arm 之后插入：

```rust
        Commands::Net { command } => {
            let url = service_url(
                "AETHER_NETSRV_URL",
                "http",
                aether_model::service_ports::NETSRV_PORT,
                host,
            );
            net::handle_command(command, &url, json).await?;
        },
```

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p aether net::tests`
Expected: PASS，2 个测试

- [ ] **Step 6: 确认 CLI 能起来**

Run: `cargo run -p aether -- net mqtt --help`
Expected: 打印 `Show MQTT connection status` 子命令帮助

- [ ] **Step 7: 提交**

```bash
git add tools/aether/src/net.rs tools/aether/src/main.rs
git commit -m "feat: add 'aether net mqtt status' backed by netsrv"
```

---

## Task 4: `net mqtt config` / `config set` / `reconnect` / `disconnect`

`POST /netApi/mqtt/config` 收的是**完整 `NetConfig` 对象**（`services/netsrv/src/routes.rs:250`），不是局部字段。因此 CLI 用 `--file` 传原始 JSON。

**Files:**
- Modify: `tools/aether/src/net.rs`

- [ ] **Step 1: 写失败的测试**

在 `net.rs` 的 `mod tests` 中追加：

```rust
    #[tokio::test]
    async fn mqtt_config_get_hits_config_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/netApi/mqtt/config"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "host": "h" })))
            .expect(1)
            .mount(&server)
            .await;

        let client = NetClient::new(&server.uri()).unwrap();
        let v = client.mqtt_config().await.unwrap();

        assert_eq!(v["host"], "h");
    }

    #[tokio::test]
    async fn mqtt_config_set_posts_the_body_verbatim() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/netApi/mqtt/config"))
            .and(wiremock::matchers::body_json(serde_json::json!({ "host": "new", "port": 1883 })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "success": true })))
            .expect(1)
            .mount(&server)
            .await;

        let client = NetClient::new(&server.uri()).unwrap();
        let cfg = serde_json::json!({ "host": "new", "port": 1883 });
        client.mqtt_config_set(&cfg).await.unwrap();
    }

    #[tokio::test]
    async fn mqtt_reconnect_and_disconnect_post_their_endpoints() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/netApi/mqtt/reconnect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/netApi/mqtt/disconnect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let client = NetClient::new(&server.uri()).unwrap();
        client.mqtt_reconnect().await.unwrap();
        client.mqtt_disconnect().await.unwrap();
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p aether net::tests`
Expected: FAIL — `no method named 'mqtt_config'`

- [ ] **Step 3: 扩展 `MqttCommands`**

```rust
#[derive(Subcommand)]
pub enum MqttCommands {
    /// Show MQTT connection status
    #[command(about = "Show MQTT connection status")]
    Status,

    /// Show the current netsrv configuration
    #[command(about = "Show the current netsrv configuration")]
    Config,

    /// Replace the netsrv configuration from a JSON file
    #[command(about = "Replace netsrv configuration from a JSON file (full NetConfig object)")]
    ConfigSet {
        /// Path to a JSON file containing the complete NetConfig object
        #[arg(long)]
        file: String,
    },

    /// Reconnect the MQTT client
    #[command(about = "Reconnect the MQTT client")]
    Reconnect,

    /// Disconnect the MQTT client
    #[command(about = "Disconnect the MQTT client")]
    Disconnect,
}
```

`#[arg(..)]` 属性由 `Subcommand` derive 处理，`net.rs` 顶部的 `use clap::Subcommand;` 无需改动。

- [ ] **Step 4: 实现四个 client 方法**

在 `impl NetClient` 中追加：

```rust
    async fn mqtt_config(&self) -> Result<Value> {
        let resp = self
            .client
            .get(format!("{}/netApi/mqtt/config", self.base_url))
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(parse_error_body("Failed to get netsrv config", resp).await)
        }
    }

    async fn mqtt_config_set(&self, cfg: &Value) -> Result<Value> {
        let resp = self
            .client
            .post(format!("{}/netApi/mqtt/config", self.base_url))
            .json(cfg)
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(parse_error_body("Failed to update netsrv config", resp).await)
        }
    }

    async fn mqtt_reconnect(&self) -> Result<Value> {
        self.post_empty("/netApi/mqtt/reconnect", "Failed to reconnect MQTT").await
    }

    async fn mqtt_disconnect(&self) -> Result<Value> {
        self.post_empty("/netApi/mqtt/disconnect", "Failed to disconnect MQTT").await
    }

    async fn post_empty(&self, path: &str, context: &str) -> Result<Value> {
        let resp = self
            .client
            .post(format!("{}{}", self.base_url, path))
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(parse_error_body(context, resp).await)
        }
    }
```

- [ ] **Step 5: 重构 `handle_command` 为两级委派，并区分「查询」与「动作」输出**

Task 3 的 code review 提出两条结构性意见，在此落地。**不要**继续堆扁平 match。

**(a) 两级委派**，照抄 `models.rs` 的既有先例（`ModelCommands::Products { command } => handle_product_command(command, base_url, json).await`）。否则 Task 5/6 加进 `Cert` 后，`handle_command` 会变成一个把 MQTT 与 Cert 混在一起的 10 臂扁平 match。

```rust
pub async fn handle_command(cmd: NetCommands, base_url: &str, json: bool) -> Result<()> {
    match cmd {
        NetCommands::Mqtt(command) => handle_mqtt_command(command, base_url, json).await,
    }
}

async fn handle_mqtt_command(cmd: MqttCommands, base_url: &str, json: bool) -> Result<()> {
    let client = NetClient::new(base_url)?;
    match cmd {
        MqttCommands::Status => {
            let data = client.mqtt_status().await?;
            print_value(&data, json);
        },
        MqttCommands::Config => {
            let data = client.mqtt_config().await?;
            print_value(&data, json);
        },
        MqttCommands::ConfigSet { file } => {
            let raw = std::fs::read_to_string(&file)?;
            let cfg: Value = serde_json::from_str(&raw)?;
            client.mqtt_config_set(&cfg).await?;
            print_action("netsrv config updated", json);
        },
        MqttCommands::Reconnect => {
            client.mqtt_reconnect().await?;
            print_action("MQTT reconnect requested", json);
        },
        MqttCommands::Disconnect => {
            client.mqtt_disconnect().await?;
            print_action("MQTT disconnected", json);
        },
    }
    Ok(())
}
```

**(b) 动作型命令不要走 `print_value`，但**也不能丢掉服务器的响应**（此处为 Task 4–6 code review 后的修订版）。

初版把 `print_action(message, json)` 写成固定字符串 + `print_ok()`。这有两个实际缺陷，均已在真实 netsrv 行为下复现：

- netsrv 的 `cert delete` 对"删除成功"与"文件本不存在"**都返回 200**，只是 `message` 不同。固定字符串会在 no-op 时谎报成功。
- `--json` 下 `print_ok()` 恒输出 `{"success":true,"data":null}`，而 `cert upload` 的响应里有 `data.saved_as`（实际落盘文件名），`cert delete` 有 `data.deleted`。脚本拿不到。

正确形态是把服务器响应传进去：

```rust
/// Pick the human-facing line for an action command.
/// netsrv's own `message` distinguishes "Deleted successfully" from
/// "File does not exist, nothing to delete", and "Config updated, reconnecting"
/// from a bare ack — always prefer it over a hardcoded string.
fn action_message<'a>(data: &'a Value, fallback: &'a str) -> &'a str {
    data.get("message")
        .and_then(Value::as_str)
        .unwrap_or(fallback)
}

/// Action endpoints return a small JSON envelope, not a payload worth tabulating.
/// In --json we forward the server's response verbatim (scripts need
/// `data.saved_as` / `data.deleted`). In human mode we print the server's message.
fn print_action(data: &Value, fallback: &str, json: bool) {
    if json {
        crate::output::print_success(data);
    } else {
        println!("{}", action_message(data, fallback));
    }
}
```

调用方保留返回值：`let data = client.cert_delete(&cert_type).await?; print_action(&data, "Certificate deleted", json);`

`action_message` 抽成独立函数是为了**可直接单测**（纯函数，不需捕获 stdout），不是为了抽象。

`print_value` 自此只用于**有实际载荷**的查询命令（`status`、`config`、后续的 `cert info`）。

- [ ] **Step 6: 跑测试确认通过**

Run: `cargo test -p aether net::tests`
Expected: PASS，5 个测试

- [ ] **Step 7: 提交**

```bash
git add tools/aether/src/net.rs
git commit -m "feat: add 'aether net mqtt config/config-set/reconnect/disconnect'"
```

---

## Task 5: `net cert info` / `cert delete`

**Files:**
- Modify: `tools/aether/src/net.rs`

- [ ] **Step 1: 写失败的测试**

追加到 `mod tests`：

```rust
    #[tokio::test]
    async fn cert_info_gets_info_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/netApi/certificate/info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ca_cert": "present" })))
            .expect(1)
            .mount(&server)
            .await;

        let client = NetClient::new(&server.uri()).unwrap();
        let v = client.cert_info().await.unwrap();

        assert_eq!(v["ca_cert"], "present");
    }

    #[tokio::test]
    async fn cert_delete_uses_delete_with_cert_type_in_path() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/netApi/certificate/client_key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let client = NetClient::new(&server.uri()).unwrap();
        client.cert_delete("client_key").await.unwrap();
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p aether net::tests`
Expected: FAIL — `no method named 'cert_info'`

- [ ] **Step 3: 加 `CertCommands` 与 `NetCommands::Cert`**

`cert_type` 的合法取值来自 `services/netsrv/src/models.rs:341`：`ca_cert | client_cert | client_key`。

```rust
#[derive(Subcommand)]
pub enum CertCommands {
    /// Show installed certificate info
    #[command(about = "Show installed TLS certificate info")]
    Info,

    /// Delete a certificate by type
    #[command(about = "Delete a TLS certificate by type")]
    Delete {
        /// Certificate role
        #[arg(value_parser = ["ca_cert", "client_cert", "client_key"])]
        cert_type: String,
    },
}
```

`NetCommands` 追加 variant：

```rust
    /// TLS certificate management
    #[command(subcommand)]
    Cert(CertCommands),
```

- [ ] **Step 4: 实现两个 client 方法**

```rust
    async fn cert_info(&self) -> Result<Value> {
        let resp = self
            .client
            .get(format!("{}/netApi/certificate/info", self.base_url))
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(parse_error_body("Failed to get certificate info", resp).await)
        }
    }

    async fn cert_delete(&self, cert_type: &str) -> Result<Value> {
        let resp = self
            .client
            .delete(format!("{}/netApi/certificate/{}", self.base_url, cert_type))
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(parse_error_body("Failed to delete certificate", resp).await)
        }
    }
```

- [ ] **Step 5: 新增 `handle_cert_command`，并在 `handle_command` 里委派**

延续 Task 4 建立的两级委派结构，**不要**把 Cert 的 arm 塞进 `handle_mqtt_command`，也不要塞回扁平 match：

```rust
pub async fn handle_command(cmd: NetCommands, base_url: &str, json: bool) -> Result<()> {
    match cmd {
        NetCommands::Mqtt(command) => handle_mqtt_command(command, base_url, json).await,
        NetCommands::Cert(command) => handle_cert_command(command, base_url, json).await,
    }
}

async fn handle_cert_command(cmd: CertCommands, base_url: &str, json: bool) -> Result<()> {
    let client = NetClient::new(base_url)?;
    match cmd {
        CertCommands::Info => {
            let data = client.cert_info().await?;
            print_value(&data, json);          // 有载荷 → print_value
        },
        CertCommands::Delete { cert_type } => {
            client.cert_delete(&cert_type).await?;
            print_action(&format!("Certificate {cert_type} deleted"), json);  // 动作 → print_action
        },
    }
    Ok(())
}
```

此步之后 `main.rs` 里 `Net` 命令的 `about`（"Manage MQTT connection, netsrv config, and TLS certificates"）才名副其实——在 Task 3/4 阶段它是超前承诺的。

- [ ] **Step 6: 跑测试确认通过**

Run: `cargo test -p aether net::tests`
Expected: PASS，7 个测试

- [ ] **Step 7: 提交**

```bash
git add tools/aether/src/net.rs
git commit -m "feat: add 'aether net cert info/delete'"
```

---

## Task 6: `net cert upload`（multipart）

依赖 Task 2 已打开 `multipart` feature。

netsrv 侧约束（`services/netsrv/src/routes.rs:359` 起）：字段名必须是 `cert_type`（text）与 `file`（binary）；上限 1 MB；扩展名限 `.pem .crt .key .cer .p12 .pfx`；原始文件名被忽略。

**Files:**
- Modify: `tools/aether/src/net.rs`

- [ ] **Step 1: 写失败的测试**

追加到 `mod tests`：

```rust
    #[tokio::test]
    async fn cert_upload_posts_multipart_with_cert_type_and_file_fields() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/netApi/certificate/upload"))
            .and(wiremock::matchers::header_regex(
                "content-type",
                "^multipart/form-data; boundary=",
            ))
            // 字段名必须断言。仅凭 content-type 无法发现字段改名——
            // reqwest 的 multipart 编码器对任何字段名都发同样的 header。
            .and(wiremock::matchers::body_string_contains("name=\"cert_type\""))
            .and(wiremock::matchers::body_string_contains("name=\"file\""))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "success": true })))
            .expect(1)
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("ca.pem");
        std::fs::write(&cert, b"-----BEGIN CERTIFICATE-----\n").unwrap();

        let client = NetClient::new(&server.uri()).unwrap();
        client.cert_upload("ca_cert", &cert).await.unwrap();
    }

    #[tokio::test]
    async fn cert_upload_reports_missing_file_without_touching_the_network() {
        let client = NetClient::new("http://127.0.0.1:1").unwrap();
        let err = client
            .cert_upload("ca_cert", std::path::Path::new("/nonexistent/ca.pem"))
            .await
            .unwrap_err()
            .to_string();

        assert!(err.contains("/nonexistent/ca.pem"), "{err}");
    }
```

第二个测试指向 `127.0.0.1:1`（必然连不上），因此若它通过，就证明读文件失败发生在发请求**之前**。

- [ ] **Step 2: 加 tempfile dev-dep**

`tempfile = "3.8"` 已声明于 workspace 根 `Cargo.toml:113`。在 `tools/aether/Cargo.toml` 的 `[dev-dependencies]` 追加：

```toml
tempfile = { workspace = true }
```

- [ ] **Step 3: 跑测试确认失败**

Run: `cargo test -p aether net::tests`
Expected: FAIL — `no method named 'cert_upload'`

- [ ] **Step 4: `CertCommands` 追加 `Upload`**

```rust
    /// Upload a certificate file
    #[command(about = "Upload a TLS certificate file (max 1 MB)")]
    Upload {
        /// Certificate role
        #[arg(long = "type", value_parser = ["ca_cert", "client_cert", "client_key"])]
        cert_type: String,

        /// Path to the certificate file (.pem .crt .key .cer .p12 .pfx)
        file: String,
    },
```

- [ ] **Step 5: 实现 `cert_upload`**

`net.rs` 顶部加 `use std::path::Path;`，然后在 `impl NetClient` 追加：

```rust
    async fn cert_upload(&self, cert_type: &str, file: &Path) -> Result<Value> {
        // Read first: a missing file must fail before we open a connection.
        let bytes = std::fs::read(file)
            .map_err(|e| anyhow::anyhow!("Failed to read certificate {}: {e}", file.display()))?;

        let filename = file
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("certificate")
            .to_string();

        let part = reqwest::multipart::Part::bytes(bytes).file_name(filename);
        let form = reqwest::multipart::Form::new()
            .text("cert_type", cert_type.to_string())
            .part("file", part);

        let resp = self
            .client
            .post(format!("{}/netApi/certificate/upload", self.base_url))
            .multipart(form)
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(parse_error_body("Failed to upload certificate", resp).await)
        }
    }
```

`unwrap_or("certificate")` 不是 `unwrap`，不触发 `-D clippy::unwrap_used`。

- [ ] **Step 6: `handle_cert_command` 追加 arm**

上传是动作型（返回 `{"success":true}`），走 `print_action`：

```rust
        CertCommands::Upload { cert_type, file } => {
            client.cert_upload(&cert_type, Path::new(&file)).await?;
            print_action(&format!("Certificate {cert_type} uploaded"), json);
        },
```

- [ ] **Step 7: 跑测试确认通过**

Run: `cargo test -p aether net::tests`
Expected: PASS，9 个测试

- [ ] **Step 8: 提交**

```bash
git add tools/aether/Cargo.toml tools/aether/src/net.rs
git commit -m "feat: add 'aether net cert upload' via multipart/form-data"
```

Task 3–6 合起来完成 Spec 的 Step 1（netsrv 覆盖）。

---

## Task 7: 告警规则写操作

alarmsrv 的路由（`services/alarmsrv/src/routes.rs:38-47`）：`POST /alarmApi/rules`、`PUT|DELETE /alarmApi/rules/{id}`、`PATCH /alarmApi/rules/{id}/enable|disable`。

**注意方法是 PATCH**，不是 modsrv 那套的 POST。

请求体字段来自 `services/alarmsrv/src/models.rs:168`（`CreateRuleRequest`）与 `:187`（`UpdateRuleRequest`）。

**Files:**
- Modify: `tools/aether/src/alarms.rs`

- [ ] **Step 1: 写失败的测试**

`alarms.rs` 末尾新增（该文件当前无 `mod tests`）：

```rust
#[cfg(test)]
mod tests {
    use super::AlarmClient;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn create_rule_posts_full_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/alarmApi/rules"))
            .and(body_json(serde_json::json!({
                "service_type": "comsrv",
                "channel_id": 1001,
                "data_type": "T",
                "point_id": 5,
                "rule_name": "over-temp",
                "warning_level": 3,
                "operator": ">",
                "value": 85.0,
                "enabled": true,
                "description": "cell temperature"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "id": 7 })))
            .expect(1)
            .mount(&server)
            .await;

        let client = AlarmClient::new(&server.uri()).unwrap();
        let body = serde_json::json!({
            "service_type": "comsrv",
            "channel_id": 1001,
            "data_type": "T",
            "point_id": 5,
            "rule_name": "over-temp",
            "warning_level": 3,
            "operator": ">",
            "value": 85.0,
            "enabled": true,
            "description": "cell temperature"
        });
        let v = client.create_rule(&body).await.unwrap();

        assert_eq!(v["id"], 7);
    }

    #[tokio::test]
    async fn update_rule_uses_put_and_forwards_the_body() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/alarmApi/rules/7"))
            // 断言 body，否则一个不转发请求体的实现也能让此测试通过
            .and(body_json(serde_json::json!({ "value": 90.0 })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let client = AlarmClient::new(&server.uri()).unwrap();
        client
            .update_rule(7, &serde_json::json!({ "value": 90.0 }))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn delete_rule_uses_delete() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/alarmApi/rules/7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let client = AlarmClient::new(&server.uri()).unwrap();
        client.delete_rule(7).await.unwrap();
    }

    #[tokio::test]
    async fn enable_and_disable_use_patch_not_post() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/alarmApi/rules/7/enable"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/alarmApi/rules/7/disable"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let client = AlarmClient::new(&server.uri()).unwrap();
        client.set_rule_enabled(7, true).await.unwrap();
        client.set_rule_enabled(7, false).await.unwrap();
    }

    #[tokio::test]
    async fn delete_rule_surfaces_inline_error_message() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/alarmApi/rules/999"))
            .respond_with(ResponseTemplate::new(404).set_body_json(
                serde_json::json!({ "success": false, "message": "Rule 999 not found", "data": null }),
            ))
            .mount(&server)
            .await;

        let client = AlarmClient::new(&server.uri()).unwrap();
        let err = client.delete_rule(999).await.unwrap_err().to_string();

        assert!(err.contains("Rule 999 not found"), "{err}");
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p aether alarms::tests`
Expected: FAIL — `no method named 'create_rule'`

- [ ] **Step 3: 扩展 `AlarmCommands`**

在 `AlarmCommands` 末尾（`Monitor` 之后）追加。`create` / `update` 走 `--file`，因为 `CreateRuleRequest` 有 10 个字段，逐个做 flag 会让命令面目全非。

```rust
    /// Create an alarm rule from a JSON file
    #[command(about = "Create an alarm rule from a JSON file")]
    RuleCreate {
        /// Path to a JSON file matching alarmsrv's CreateRuleRequest
        #[arg(long)]
        file: String,
    },

    /// Update an alarm rule from a JSON file (partial update)
    #[command(about = "Update an alarm rule from a JSON file (only present fields change)")]
    RuleUpdate {
        /// Rule ID
        id: i64,
        /// Path to a JSON file matching alarmsrv's UpdateRuleRequest
        #[arg(long)]
        file: String,
    },

    /// Delete an alarm rule
    #[command(about = "Delete an alarm rule")]
    RuleDelete {
        /// Rule ID
        id: i64,
    },

    /// Enable an alarm rule
    #[command(about = "Enable an alarm rule")]
    RuleEnable {
        /// Rule ID
        id: i64,
    },

    /// Disable an alarm rule
    #[command(about = "Disable an alarm rule")]
    RuleDisable {
        /// Rule ID
        id: i64,
    },
```

- [ ] **Step 4: 实现 client 方法**

在 `impl AlarmClient` 追加。注意 `enable`/`disable` 共用一个方法，避免复制粘贴：

```rust
    async fn create_rule(&self, body: &Value) -> Result<Value> {
        let resp = self
            .client
            .post(format!("{}/alarmApi/rules", self.base_url))
            .json(body)
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(crate::output::parse_error_body("Failed to create alarm rule", resp).await)
        }
    }

    async fn update_rule(&self, id: i64, body: &Value) -> Result<Value> {
        let resp = self
            .client
            .put(format!("{}/alarmApi/rules/{}", self.base_url, id))
            .json(body)
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(crate::output::parse_error_body("Failed to update alarm rule", resp).await)
        }
    }

    async fn delete_rule(&self, id: i64) -> Result<Value> {
        let resp = self
            .client
            .delete(format!("{}/alarmApi/rules/{}", self.base_url, id))
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(crate::output::parse_error_body("Failed to delete alarm rule", resp).await)
        }
    }

    /// alarmsrv uses PATCH here; modsrv uses POST for the same semantics on its own rules.
    async fn set_rule_enabled(&self, id: i64, enabled: bool) -> Result<Value> {
        let action = if enabled { "enable" } else { "disable" };
        let resp = self
            .client
            .patch(format!("{}/alarmApi/rules/{}/{}", self.base_url, id, action))
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(crate::output::parse_error_body("Failed to toggle alarm rule", resp).await)
        }
    }
```

- [ ] **Step 5: 扩展 `handle_command`**

在 `alarms.rs::handle_command` 的 match 中追加：

```rust
        AlarmCommands::RuleCreate { file } => {
            let raw = std::fs::read_to_string(&file)?;
            let body: Value = serde_json::from_str(&raw)?;
            let data = client.create_rule(&body).await?;
            if json { crate::output::print_success(&data); } else { println!("Created rule: {data}"); }
        },
        AlarmCommands::RuleUpdate { id, file } => {
            let raw = std::fs::read_to_string(&file)?;
            let body: Value = serde_json::from_str(&raw)?;
            let data = client.update_rule(id, &body).await?;
            if json { crate::output::print_success(&data); } else { println!("Updated rule {id}"); }
        },
        AlarmCommands::RuleDelete { id } => {
            client.delete_rule(id).await?;
            if json { crate::output::print_ok(); } else { println!("Deleted rule {id}"); }
        },
        AlarmCommands::RuleEnable { id } => {
            client.set_rule_enabled(id, true).await?;
            if json { crate::output::print_ok(); } else { println!("Enabled rule {id}"); }
        },
        AlarmCommands::RuleDisable { id } => {
            client.set_rule_enabled(id, false).await?;
            if json { crate::output::print_ok(); } else { println!("Disabled rule {id}"); }
        },
```

- [ ] **Step 6: 更新模块文档注释**

`alarms.rs` 第 1-4 行当前写着 `//! Provides read-only access to alarmsrv: …`。改为：

```rust
//! Alarm management module
//!
//! Provides access to alarmsrv: active alerts, alarm rules (read and write),
//! historical alert events, statistics, and monitor status.
```

- [ ] **Step 7: 跑测试确认通过**

Run: `cargo test -p aether alarms::tests`
Expected: PASS，5 个测试

- [ ] **Step 8: 提交**

```bash
git add tools/aether/src/alarms.rs
git commit -m "feat: add alarm rule create/update/delete/enable/disable to CLI

alarmsrv uses PATCH for enable/disable, unlike modsrv which uses POST for
the equivalent operation on business rules."
```

这完成 Spec 的 Step 2。

---

## Task 8: `channels enabled` / `mappings` / `unmapped-points`

端点（`services/comsrv/src/api/routes.rs`）：
- `PUT /api/channels/{id}/enabled`，body `{"enabled": bool}`（`ChannelEnabledRequest`，`dto.rs:305`）
- `GET /api/channels/{id}/mappings`
- `GET /api/channels/{id}/unmapped-points`

**Files:**
- Modify: `tools/aether/src/channels.rs`

- [ ] **Step 1: 写失败的测试**

`channels.rs` 已有内容，末尾新增 `#[cfg(test)] mod tests`（若已存在则追加到其中）：

```rust
#[cfg(test)]
mod tests {
    use super::ChannelClient;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn set_enabled_puts_enabled_body() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/channels/1001/enabled"))
            .and(body_json(serde_json::json!({ "enabled": false })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let client = ChannelClient::new(&server.uri()).unwrap();
        client.set_enabled(1001, false).await.unwrap();
    }

    #[tokio::test]
    async fn mappings_and_unmapped_points_use_their_paths() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/channels/1001/mappings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "mappings": [] })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/channels/1001/unmapped-points"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "points": [] })))
            .expect(1)
            .mount(&server)
            .await;

        let client = ChannelClient::new(&server.uri()).unwrap();
        client.mappings(1001).await.unwrap();
        client.unmapped_points(1001).await.unwrap();
    }

    #[tokio::test]
    async fn set_enabled_surfaces_typed_error_suggestion() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/channels/9/enabled"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "success": false,
                "error": { "code": "CHANNEL_NOT_FOUND", "message": "channel 9 missing", "suggestion": "run aether sync" }
            })))
            .mount(&server)
            .await;

        let client = ChannelClient::new(&server.uri()).unwrap();
        let err = client.set_enabled(9, true).await.unwrap_err().to_string();

        assert!(err.contains("channel 9 missing"), "{err}");
        assert!(err.contains("run aether sync"), "{err}");
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p aether channels::tests`
Expected: FAIL — `no method named 'set_enabled'`

- [ ] **Step 3: 扩展 `ChannelCommands`**

追加到 `ChannelCommands`：

```rust
    /// Enable or disable a channel
    #[command(about = "Enable or disable a channel")]
    Enabled {
        /// Channel ID
        channel_id: u32,
        /// Desired state
        #[arg(value_parser = clap::value_parser!(bool))]
        enabled: bool,
    },

    /// Show a channel's point mappings
    #[command(about = "Show a channel's point mappings")]
    Mappings {
        /// Channel ID
        channel_id: u32,
    },

    /// List points on a channel that have no instance mapping
    #[command(about = "List points on a channel with no instance mapping")]
    UnmappedPoints {
        /// Channel ID
        channel_id: u32,
    },
```

- [ ] **Step 4: 实现 client 方法**

在 `impl ChannelClient` 追加：

```rust
    async fn set_enabled(&self, channel_id: u32, enabled: bool) -> Result<Value> {
        let resp = self
            .client
            .put(format!("{}/api/channels/{}/enabled", self.base_url, channel_id))
            .json(&serde_json::json!({ "enabled": enabled }))
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(crate::output::parse_error_body("Failed to set channel enabled state", resp).await)
        }
    }

    async fn mappings(&self, channel_id: u32) -> Result<Value> {
        let resp = self
            .client
            .get(format!("{}/api/channels/{}/mappings", self.base_url, channel_id))
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(crate::output::parse_error_body("Failed to get channel mappings", resp).await)
        }
    }

    async fn unmapped_points(&self, channel_id: u32) -> Result<Value> {
        let resp = self
            .client
            .get(format!("{}/api/channels/{}/unmapped-points", self.base_url, channel_id))
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(crate::output::parse_error_body("Failed to get unmapped points", resp).await)
        }
    }
```

- [ ] **Step 5: 扩展 `handle_command`**

```rust
        ChannelCommands::Enabled { channel_id, enabled } => {
            client.set_enabled(channel_id, enabled).await?;
            if json {
                crate::output::print_ok();
            } else {
                println!("Channel {channel_id} enabled = {enabled}");
            }
        },
        ChannelCommands::Mappings { channel_id } => {
            let data = client.mappings(channel_id).await?;
            if json {
                crate::output::print_success(&data);
            } else if let Ok(s) = serde_json::to_string_pretty(&data) {
                println!("{s}");
            }
        },
        ChannelCommands::UnmappedPoints { channel_id } => {
            let data = client.unmapped_points(channel_id).await?;
            if json {
                crate::output::print_success(&data);
            } else if let Ok(s) = serde_json::to_string_pretty(&data) {
                println!("{s}");
            }
        },
```

- [ ] **Step 6: 跑测试确认通过**

Run: `cargo test -p aether channels::tests`
Expected: PASS，3 个测试

- [ ] **Step 7: 提交**

```bash
git add tools/aether/src/channels.rs
git commit -m "feat: add 'aether channels enabled/mappings/unmapped-points'"
```

---

## Task 9: `channels write` / `points batch` / `point-mapping`

端点：
- `POST /api/channels/{channel_id}/write`，body `WritePointRequest`（`dto.rs:92`）：`{"type":"A","id":"1","value":50.0}`（单点，`data` 是 `#[serde(flatten)]`）或 `{"type":"A","points":[{"id":"1","value":50.0}]}`（批量）
- `POST /api/channels/{channel_id}/points/batch`，body `PointBatchRequest`（`point_types.rs:34`）：`{"create":[],"update":[],"delete":[]}`
- `GET /api/channels/{channel_id}/{type}/points/{point_id}/mapping`

`write` 单点用 flag，`points batch` 用 `--file`。

**client 归属**（`channels.rs` 内两个 client 分工明确，照此归置）：

| 方法 | client | 归属命令 | 理由 |
|---|---|---|---|
| `write_point` | `ChannelClient` | `ChannelCommands::Write` | 通道级写入，与既有 `send_control` / `send_adjustment` 同类 |
| `points_batch` | `PointClient` | `PointCommands::Batch` | 点位 CRUD，与既有 `add_point` / `update_point` / `remove_point` 同类 |
| `point_mapping` | `PointClient` | `PointCommands::Mapping` | 单点映射查询，点位级 |

`channels.rs:104` 已有嵌套子命令组 `ChannelCommands::Points { command: PointCommands }`，其 handler 在 `channels.rs:334` 构造 `let pc = PointClient::new(base_url)?;`。两个点位级新命令进 `PointCommands`，最终 CLI 形态为：

```
aether channels write <ch> --type A --id 5 --value 50.0
aether channels points batch <ch> --file batch.json
aether channels points mapping <ch> T 5
```

**Files:**
- Modify: `tools/aether/src/channels.rs`

- [ ] **Step 1: 写失败的测试**

追加到 `channels.rs` 的 `mod tests`。注意 `use` 需同时引入两个 client：

```rust
    // 在 mod tests 顶部的 use 中补上 PointClient：
    //   use super::{ChannelClient, PointClient};

    #[tokio::test]
    async fn write_point_posts_flattened_single_point_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/channels/1001/write"))
            .and(body_json(serde_json::json!({ "type": "A", "id": "5", "value": 50.0 })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let client = ChannelClient::new(&server.uri()).unwrap();
        client.write_point(1001, "A", "5", 50.0).await.unwrap();
    }

    #[tokio::test]
    async fn points_batch_posts_body_verbatim() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/channels/1001/points/batch"))
            .and(body_json(serde_json::json!({ "delete": [{ "point_id": 3 }] })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let client = PointClient::new(&server.uri()).unwrap();
        let body = serde_json::json!({ "delete": [{ "point_id": 3 }] });
        client.points_batch(1001, &body).await.unwrap();
    }

    #[tokio::test]
    async fn point_mapping_uses_type_in_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/channels/1001/T/points/5/mapping"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "instance_id": 3 })))
            .expect(1)
            .mount(&server)
            .await;

        let client = PointClient::new(&server.uri()).unwrap();
        let v = client.point_mapping(1001, "T", 5).await.unwrap();

        assert_eq!(v["instance_id"], 3);
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p aether channels::tests`
Expected: FAIL — `no method named 'write_point'`

- [ ] **Step 3a: 给 `ChannelCommands` 加 `Write`**

```rust
    /// Write a value to a point on a channel
    #[command(about = "Write a value to a single point on a channel")]
    Write {
        /// Channel ID
        channel_id: u32,
        /// Point type: T | S | C | A
        #[arg(long = "type", value_parser = ["T", "S", "C", "A"])]
        point_type: String,
        /// Point ID (numeric or semantic)
        #[arg(long)]
        id: String,
        /// Value to write
        #[arg(long)]
        value: f64,
    },
```

- [ ] **Step 3b: 给 `PointCommands`（`channels.rs:111`）加 `Batch` 与 `Mapping`**

```rust
    /// Apply a batch of point create/update/delete operations from a JSON file
    #[command(about = "Batch create/update/delete points from a JSON file")]
    Batch {
        /// Channel ID
        channel_id: u32,
        /// Path to a JSON file: {"create":[],"update":[],"delete":[]}
        #[arg(long)]
        file: String,
    },

    /// Show the instance mapping for one point
    #[command(about = "Show the instance mapping for a single point")]
    Mapping {
        /// Channel ID
        channel_id: u32,
        /// Point type: T | S | C | A
        #[arg(value_parser = ["T", "S", "C", "A"])]
        point_type: String,
        /// Point ID
        point_id: u32,
    },
```

- [ ] **Step 4a: 在 `impl ChannelClient` 中实现 `write_point`**

```rust
    /// comsrv's `WritePointRequest` flattens the point payload into the top level,
    /// so a single-point write is `{"type":..,"id":..,"value":..}`, not a nested object.
    async fn write_point(&self, channel_id: u32, point_type: &str, id: &str, value: f64) -> Result<Value> {
        let body = serde_json::json!({ "type": point_type, "id": id, "value": value });
        let resp = self
            .client
            .post(format!("{}/api/channels/{}/write", self.base_url, channel_id))
            .json(&body)
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(crate::output::parse_error_body("Failed to write point", resp).await)
        }
    }
```

- [ ] **Step 4b: 在 `impl PointClient` 中实现 `points_batch` 与 `point_mapping`**

`PointClient` 已有 `list_points` / `add_point` / `update_point` / `remove_point`，这两个方法与之同属点位级：

```rust
    async fn points_batch(&self, channel_id: u32, body: &Value) -> Result<Value> {
        let resp = self
            .client
            .post(format!("{}/api/channels/{}/points/batch", self.base_url, channel_id))
            .json(body)
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(crate::output::parse_error_body("Failed to apply point batch", resp).await)
        }
    }

    async fn point_mapping(&self, channel_id: u32, point_type: &str, point_id: u32) -> Result<Value> {
        let resp = self
            .client
            .get(format!(
                "{}/api/channels/{}/{}/points/{}/mapping",
                self.base_url, channel_id, point_type, point_id
            ))
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(crate::output::parse_error_body("Failed to get point mapping", resp).await)
        }
    }
```

- [ ] **Step 5: 扩展 `handle_command`**

```rust
        ChannelCommands::Write { channel_id, point_type, id, value } => {
            let data = client.write_point(channel_id, &point_type, &id, value).await?;
            if json {
                crate::output::print_success(&data);
            } else {
                println!("Wrote {value} to channel {channel_id} point {point_type}/{id}");
            }
        },
另外两个进 `ChannelCommands::Points { command }` 分支内部的 `match command` —— 那里已有 `let pc = PointClient::new(base_url)?;`（`channels.rs:334`），直接复用 `pc`：

```rust
                PointCommands::Batch { channel_id, file } => {
                    let raw = std::fs::read_to_string(&file)?;
                    let body: Value = serde_json::from_str(&raw)?;
                    let data = pc.points_batch(channel_id, &body).await?;
                    if json {
                        crate::output::print_success(&data);
                    } else if let Ok(s) = serde_json::to_string_pretty(&data) {
                        println!("{s}");
                    }
                },
                PointCommands::Mapping { channel_id, point_type, point_id } => {
                    let data = pc.point_mapping(channel_id, &point_type, point_id).await?;
                    if json {
                        crate::output::print_success(&data);
                    } else if let Ok(s) = serde_json::to_string_pretty(&data) {
                        println!("{s}");
                    }
                },
```

- [ ] **Step 6: 跑测试确认通过**

Run: `cargo test -p aether channels::tests`
Expected: PASS，6 个测试

- [ ] **Step 7: 提交**

```bash
git add tools/aether/src/channels.rs
git commit -m "feat: add 'aether channels write/points-batch/point-mapping'"
```

---

## Task 10: `models instances action`

端点（`services/modsrv/src/routes.rs:157-158`）：
- `POST /api/instances/{id}/action`，body `ActionRequest`（`api/dto.rs:184`）：`{"point_id":"1","value":4500.0}`（`point_id` 是 **String**，可为数字或语义名）

`ModelClient` 位于 `tools/aether/src/models/client.rs`，且**已经是 `pub`**（与其余 7 个 client 不同）。

**Files:**
- Modify: `tools/aether/src/models/client.rs`
- Modify: `tools/aether/src/models.rs`

- [ ] **Step 1: 写失败的测试**

`tools/aether/src/models/client.rs` 末尾新增：

```rust
#[cfg(test)]
mod tests {
    use super::ModelClient;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn execute_action_posts_string_point_id() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/instances/3/action"))
            .and(body_json(serde_json::json!({ "point_id": "power_setpoint", "value": 4500.0 })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let client = ModelClient::new(&server.uri()).unwrap();
        client.execute_action(3, "power_setpoint", 4500.0).await.unwrap();
    }

    #[tokio::test]
    async fn execute_action_surfaces_modsrv_typed_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/instances/3/action"))
            .respond_with(ResponseTemplate::new(503).set_body_json(serde_json::json!({
                "success": false,
                "error": { "code": "CHANNEL_OFFLINE", "message": "channel 1001 offline" }
            })))
            .mount(&server)
            .await;

        let client = ModelClient::new(&server.uri()).unwrap();
        let err = client.execute_action(3, "1", 1.0).await.unwrap_err().to_string();

        assert!(err.contains("channel 1001 offline"), "{err}");
    }
}
```

第三个测试对应 CLAUDE.md 的约定：控制写入失败经**返回值**透传（HTTP 503 + reason），不写回 instance。

`ModelClient::new(base_url: &str) -> Result<Self>`（`models/client.rs:14`），故测试中的 `.unwrap()` 成立。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p aether models::client::tests`
Expected: FAIL — `no method named 'execute_action'`

- [ ] **Step 3: 实现 action client 方法**

在 `models/client.rs` 的 `impl ModelClient` 追加。该 client 方法是 `pub`，与该文件既有风格一致：

```rust
    /// modsrv's ActionRequest takes point_id as a String: it may be numeric
    /// ("1") or a semantic name ("power_setpoint").
    pub async fn execute_action(&self, instance_id: u32, point_id: &str, value: f64) -> Result<Value> {
        let body = serde_json::json!({ "point_id": point_id, "value": value });
        let resp = self
            .client
            .post(format!("{}/api/instances/{}/action", self.base_url, instance_id))
            .json(&body)
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(crate::output::parse_error_body("Failed to execute instance action", resp).await)
        }
    }
```

- [ ] **Step 4: 加子命令**

`tools/aether/src/models.rs` 的结构已确认为 `ModelCommands { Products{ ProductCommands }, Instances{ InstanceCommands } }`，`InstanceCommands`（`models.rs:48`）现有 `List` / `Create` / `Get` / `Update` / `Delete` / `Data`。新变体加进 **`InstanceCommands`**：

```rust
    /// Execute a control action on an instance
    #[command(about = "Execute a control action on an instance (writes to the device)")]
    Action {
        /// Instance ID
        instance_id: u32,
        /// Point ID: numeric ("1") or semantic ("power_setpoint")
        #[arg(long)]
        point_id: String,
        /// Value to write
        #[arg(long)]
        value: f64,
    },
```

- [ ] **Step 5: 在 `models.rs::handle_command` 的 `Instances` 分支接线**

```rust
        // …在 Instances 对应的 match 分支内
        InstanceCommands::Action { instance_id, point_id, value } => {
            let data = client.execute_action(instance_id, &point_id, value).await?;
            if json {
                crate::output::print_success(&data);
            } else {
                println!("Action sent to instance {instance_id} point {point_id} = {value}");
            }
        },
```

- [ ] **Step 6: 跑测试确认通过**

Run: `cargo test -p aether models::client::tests`
Expected: PASS，2 个测试

- [ ] **Step 7: 提交**

```bash
git add tools/aether/src/models.rs tools/aether/src/models/client.rs
git commit -m "feat: add 'aether models instances action'"
```

Task 8–10 合起来完成 Spec 的 Step 3。

---

## Task 11: 全量校验

- [ ] **Step 0: 修正 `quick-check.sh` 的测试覆盖面（对齐 CI）**

`scripts/quick-check.sh` 的单元测试步骤当前是：

```bash
"${TEST_RUNNER[@]}" --workspace --lib
```

这不带 `--bins`，因此 `aether`（bin-only crate）的全部单元测试——包括本计划新增的——**在本地从不执行**。CI 的 `.github/workflows/rust-check.yml:117` 用的是 `cargo nextest run --workspace --lib --bins`，覆盖面更宽。改为：

```bash
"${TEST_RUNNER[@]}" --workspace --lib --bins
```

这会让 `aether` 既有的 73 个测试也开始在本地运行。它们在 CI 中一直是通过的，所以此改动应当无副作用；若有失败，属于本次暴露出来的既有问题，需单独处理，不要为了让检查变绿而回退这个修正。

- [ ] **Step 1: 跑完整检查**

Run: `./scripts/quick-check.sh`

Expected: `All checks passed!`，且输出中能看到 `output::tests`、`net::tests`、`alarms::tests` 等本计划新增的测试

这一步会依次跑：`mod.rs` 拦截 → `cargo check --workspace` → `cargo fmt --check` → `cargo clippy --all-targets --all-features -- -D warnings` → `cargo clippy --workspace --lib --bins -- -D clippy::unwrap_used -D clippy::expect_used` → 单元测试。

**若第二遍 clippy 报 `unwrap_used`**：检查是否在运行时代码（非 `#[cfg(test)]`）里用了 `unwrap`。测试里的 unwrap 不会被这遍扫到；若被扫到，说明测试没放在 `#[cfg(test)] mod tests` 内。

- [ ] **Step 2: 验证 hakari 一致性**

Run: `cargo hakari verify`
Expected: exit 0，无输出

- [ ] **Step 3: 人工确认新命令齐全**

Run:
```bash
cargo run -p aether -- net --help
cargo run -p aether -- alarms --help
cargo run -p aether -- channels --help
cargo run -p aether -- models instances --help
```

Expected: 新增子命令全部出现；`net` 下有 `mqtt` 与 `cert` 两组。

- [ ] **Step 4: 验证 `--json` 全局生效**

Run: `cargo run -p aether -- --json net mqtt status`
Expected: 无 banner、无颜色；输出为 `{"success":…}` 信封（netsrv 未运行时应为 `{"success":false,…}` 或非零退出并打印错误，取决于连接失败路径）。

- [ ] **Step 5: 最终提交**

```bash
git add -A
git commit -m "chore: verify CLI/Web parity implementation passes quick-check"
```

---

## Spec 覆盖对照

| Spec 要求 | 实现于 |
|---|---|
| Step 1 · netsrv MQTT + 证书 | Task 3, 4, 5, 6 |
| Step 1 · reqwest multipart feature + hakari | Task 2 |
| Step 2 · 告警规则写操作（5 个） | Task 7 |
| Step 3 · channels enabled/mappings/unmapped-points | Task 8 |
| Step 3 · channels write/points batch/point-mapping | Task 9 |
| Step 3 · instances action/measurement | Task 10 |
| 错误处理 · `parse_error_body`（两种错误形状） | Task 1 |
| 测试 · wiremock dev-dep + TDD | Task 1 起，贯穿全程 |
| 验收 · quick-check + hakari verify + `--json` | Task 11 |
| 排除 · 四个幽灵接口 | 全程未实现，见 Spec |
| 排除 · 用户/鉴权管理 | 全程未实现，见 Spec |

## 执行期间发现的既有 bug（已记录，未修）

这些都不是本计划引入的，且都超出范围。它们能长期存活是同一个原因：**`tools/aether` 的 HTTP 层与 clap 层此前从无任何测试**。本计划给这两层都装上了守卫（wiremock 客户端测试、`Cli::command().debug_assert()`），错配随即开始浮现。

| 问题 | 位置 | 后果 |
|---|---|---|
| `quick-check.sh` 用 `--workspace --lib`，缺 `--bins` | `scripts/quick-check.sh` | aether 的 73 个既有单测本地从不执行；CI 用 `--lib --bins`，跑得到。**Task 11 修** |
| `services clean` 的 `volumes` 用 `#[arg(short, long)]`，推断出 `-v`，与全局 `-v/--verbose` 冲突 | `services.rs` | debug panic / release 歧义。**已顺手修**（`debug_assert` 测试不修它就过不去） |
| `models instances get/update/delete` 把实例**名字**传给 `/api/instances/{id}`，而 modsrv 用 `Path<u32>` 提取 | `models/client.rs:76` 起，自初始提交 | 传非数字必 400。modsrv 无按名查询路由（只有 `/search`、`/list`）。三个命令的参数从设计上就对不上 |
| `tempfile` 在 `[dependencies]` 而非 `[dev-dependencies]`，只被 `#[cfg(test)]` 使用 | `tools/aether/Cargo.toml:49` | 白白进 release 依赖树 |
| `aether rules test` 打向不存在的 `POST /api/rules/{id}/test` | `rules.rs:364` | 必然 404。见 Spec「CLI 侧的第五个幽灵」 |

## 明确不做

- 不回改现有 8 个 client 的错误处理（它们继续只显示状态码）。`parse_error_body` 只供新代码使用。
- 不修 `aether rules test`（打向不存在的 `POST /api/rules/{id}/test`）。见 Spec「CLI 侧的第五个幽灵」。
- 不修 `models instances get/update/delete` 的 name/id 错配。属独立 bug，需先决定是给 modsrv 加按名查询路由，还是把 CLI 改成收 id。
- 不实现 `control/batch`、`adjustment/batch`、`instances/{id}/mappings`、`/api/operation-logs`——后端均不存在。
- 不抽取公共 `ServiceClient`。第 9 个 client 继续复制既有形状。

## 下一步

本计划完成后，Spec 3（`aether mcp`）方可开始——它依赖这里新增的 `net` 命令与 client 方法作为 MCP 工具的实现基础。届时另出一份计划。
