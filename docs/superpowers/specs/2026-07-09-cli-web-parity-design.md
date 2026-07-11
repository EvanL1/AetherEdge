# CLI / Web 功能对等 — 设计文档

- **日期**: 2026-07-09
- **状态**: 已定稿，待实施
- **范围**: Spec 1（共 3 份，见文末「关联 Spec」）

## 目标

补齐 `aether` CLI 相对 Vue 前端缺失的能力，使两者功能对等。**不删除前端，不改动服务架构，不新增 service/crate。**

## 背景：现状调查结论

调查 `tools/aether`（14k LOC，17 个顶层命令）与 `apps/`（Vue 3）后确认：

1. **CLI 不是 apigateway 的客户端，而是它的对等物。** CLI 直连 Redis/SQLite 做读取与配置同步（`rtdb.rs`、`top.rs`、`core/syncer.rs`），并对各服务直接发 HTTP 做变更。`tools/aether/` 中不存在对 apigateway 的引用。
2. **前端同样绕过 apigateway。** `apps/nginx.conf` 将 `/comApi` → comsrv:6001、`/modApi` 与 `/ruleApi` → modsrv:6002、`/alarmApi` → alarmsrv:6007、`/netApi` → netsrv:6006。apigateway 仅服务 `/api/v1/auth/*`、`/api/v1/homepage`、`/ws`。
3. **鉴权在 nginx 层。** `nginx.conf` 通过 `auth_request` 子请求打到 apigateway 的 `/api/v1/auth/validate` 来守卫各 backend location。服务本身在 localhost 上不鉴权。这是 CLI 无需 token 即可直连 `127.0.0.1:6001` 的原因，也是本 Spec **不实现 CLI 登录/用户管理**的依据。

因此「CLI/Web 对等」是一个逐服务的问题，与 apigateway 无关。

### 既有代码约定（新代码必须遵循）

每个 CLI 模块的形状一致：

```rust
pub enum XCommands { … }                      // clap Subcommand
pub async fn handle_command(cmd: XCommands, base_url: &str, json: bool) -> Result<()>
struct XClient { client: reqwest::Client, base_url: String }
impl XClient { fn new(base_url: &str) -> Result<Self> }
```

已有 8 个：`AlarmClient`、`ChannelClient`、`PointClient`、`HistoryClient`、`RuleClient`、`TemplateClient`、`RoutingClient`（均为私有），以及 `ModelClient`（`models/client.rs`，已是 `pub`，方法亦 `pub`）。

- `base_url` 由 `main.rs::service_url(env_var, scheme, port, host)` 解析，遵循 `AETHER_<SERVICE>_URL` + `aether_model::service_ports::<SERVICE>_PORT`，并受全局 `--host` 覆盖。
- 输出经 `output::print_success` / `print_error` / `print_ok`，受全局 `--json` 控制。
- 错误经 `anyhow::Result` 上抛，`main.rs:279-285` 打印后 `exit(1)`。

## 决策记录

| 决策 | 选择 | 理由 |
|---|---|---|
| 架构 | 不变，CLI 与 Web 并存 | 用户明确选择；符合 YAGNI |
| 实现方式 | 就地扩展，照抄现有 `XClient` 模式 | CLAUDE.md 禁止为加功能而引入抽象层 |
| 不做公共 `ServiceClient` 抽取 | 是 | 需动 7 个正常工作的模块，扩大爆炸半径 |
| 不做 OpenAPI 代码生成 | 是 | 引入构建步骤与第二套 API 风格，收益不抵复杂度 |
| 用户/鉴权管理 | 不做 | 鉴权在 nginx 层；CLI 运行在可信主机 |
| 操作日志 | 移出本 Spec | 后端不存在，属新功能（见 Spec 2） |
| 测试 | 新增 `wiremock` dev-dep，HTTP 级单测，TDD | 覆盖 URL/method/错误映射/multipart——CLI 客户端 bug 的实际聚集地 |

## 幽灵接口（明确排除）

前端调用了四个后端不存在的接口。它们的前端单测 mock 掉了 `Request` 层，因此恒绿，无法发现 404。**不得为它们编写 CLI 命令**——那等于让 CLI 复现前端的 bug。

| 前端调用 | 后端状态 | 证据 |
|---|---|---|
| `/api/operation-logs` | 不存在 | `apps/src/views/Statistics/OperationLog.vue:37` 已注释 |
| `/comApi/api/channels/{id}/control/batch` | comsrv 中字面量不存在 | `postControlBatch` 仅被 `apps/src/api/__tests__/channelsManagement.test.ts:108` 引用 |
| `/comApi/api/channels/{id}/adjustment/batch` | comsrv 中字面量不存在 | `postAdjustmentBatch` 同上 |
| `/modApi/api/instances/{id}/mappings` | modsrv 无任何 mappings 路由 | `getInstanceMappings` 仅被自身测试引用 |

修复它们（补后端或删前端死代码）是独立工作，不属本 Spec。

### CLI 侧的第五个幽灵：`aether rules test`

`tools/aether/src/rules.rs:364` 发起 `POST /api/rules/{id}/test`。modsrv 的两个 router 文件（`routes.rs`、`rule_routes.rs`）中**均无此路由**（已用字面量全量搜索确认）。因此 `aether rules test <id>` 今天必然返回 `HTTP 404`。

`rule_routes.rs:50-63` 实际提供的是：`/api/rules`（GET/POST）、`/api/rules/{id}`、`/api/rules/{id}/enable`（POST）、`/api/rules/{id}/disable`（POST）、`/api/rules/{id}/execute`（POST）、`/api/rules/{id}/variables`（GET）、`/api/scheduler/status`、`/api/scheduler/reload`。

**不属本 Spec。** 修复路径有二，需单独决策：

1. 删除 `RuleCommands::Test` 子命令（承认这个能力不存在）
2. 在 modsrv 实现 `/api/rules/{id}/test`（条件求值但不派发动作的 dry-run）

选 2 才能让 Spec 3 把 `rules_test` 作为只读 MCP 工具暴露。在此之前 `rules_test` 不得进入 MCP 工具集。

### 各服务的方法约定不一致

同一语义在不同服务用了不同 HTTP 方法，新代码必须逐个照抄，不可套用：

| 语义 | modsrv | alarmsrv |
|---|---|---|
| 启用规则 | `POST /api/rules/{id}/enable` | `PATCH /alarmApi/rules/{id}/enable` |
| 停用规则 | `POST /api/rules/{id}/disable` | `PATCH /alarmApi/rules/{id}/disable` |

## 实施步骤

### Step 1 — `net.rs`：netsrv 覆盖（:6006）

新建 `tools/aether/src/net.rs`，含 `NetCommands` + `NetClient`，在 `main.rs` 注册 `Commands::Net`，`base_url` 用 `service_url("AETHER_NETSRV_URL", "http", service_ports::NETSRV_PORT, host)`。

全部端点已验证存在于 `services/netsrv/src/routes.rs`：

| 命令 | 端点 | 方法 |
|---|---|---|
| `aether net mqtt status` | `/netApi/mqtt/status` | GET |
| `aether net mqtt config` | `/netApi/mqtt/config` | GET |
| `aether net mqtt config set` | `/netApi/mqtt/config` | POST |
| `aether net mqtt reconnect` | `/netApi/mqtt/reconnect` | POST |
| `aether net mqtt disconnect` | `/netApi/mqtt/disconnect` | POST |
| `aether net cert info` | `/netApi/certificate/info` | GET |
| `aether net cert upload --type <t> <file>` | `/netApi/certificate/upload` | POST (multipart/form-data) |
| `aether net cert delete <type>` | `/netApi/certificate/{cert_type}` | DELETE |

**依赖变更**：workspace `reqwest` 当前为 `default-features = false, features = ["json", "rustls-tls"]`，需追加 `"multipart"`。改后必须运行 `cargo hakari generate`（`workspace-hack` 会因 feature 统一而变化）。

证书上传是 `multipart/form-data`，表单形状见 `services/netsrv/src/models.rs:336`：每请求上传一个文件，`cert_type` 指定角色，原始文件名忽略。

### Step 2 — `alarms.rs`：告警规则写操作（:6007）

`alarms.rs` 当前模块注释为 `//! read-only access to alarmsrv`，需一并更新。

`AlarmCommands` 新增 5 个子命令，`AlarmClient` 新增对应方法。全部端点已验证存在于 `services/alarmsrv/src/routes.rs:38-47`：

| 命令 | 端点 | 方法 |
|---|---|---|
| `aether alarms rule create` | `/alarmApi/rules` | POST |
| `aether alarms rule update <id>` | `/alarmApi/rules/{id}` | PUT |
| `aether alarms rule delete <id>` | `/alarmApi/rules/{id}` | DELETE |
| `aether alarms rule enable <id>` | `/alarmApi/rules/{id}/enable` | PATCH |
| `aether alarms rule disable <id>` | `/alarmApi/rules/{id}/disable` | PATCH |

注意 `/alarmApi/rules/{id}` 的 GET/PUT/DELETE 是一个跨行 `.route(…)` 注册（`routes.rs:40-45`），单行 grep 看不到，但确实存在。

### Step 3 — `channels.rs` + `models.rs`：零碎缺口

仅包装**已验证存在**的端点。

comsrv（`services/comsrv/src/api/routes.rs`）：

| 命令 | 端点 | 方法 |
|---|---|---|
| `aether channels enabled <id> <bool>` | `/api/channels/{id}/enabled` | PUT |
| `aether channels mappings <id>` | `/api/channels/{id}/mappings` | GET |
| `aether channels unmapped-points <id>` | `/api/channels/{id}/unmapped-points` | GET |
| `aether channels write <ch>` | `/api/channels/{channel_id}/write` | POST |
| `aether channels points batch <ch>` | `/api/channels/{channel_id}/points/batch` | POST |
| `aether channels point-mapping <ch> <type> <pid>` | `/api/channels/{channel_id}/{type}/points/{point_id}/mapping` | GET |

modsrv（`services/modsrv/src/routes.rs:157-158`）：

| 命令 | 端点 | 方法 |
|---|---|---|
| `aether models instances action <id>` | `/api/instances/{id}/action` | POST |
| `aether models instances measurement <id>` | `/api/instances/{id}/measurement` | POST |

## 错误处理

现有 7 个 client 一律写作：

```rust
if resp.status().is_success() { Ok(resp.json().await?) }
else { Err(anyhow::anyhow!("… {}", resp.status())) }
```

这**丢弃了服务端的错误体**。按 CLAUDE.md，comsrv/modsrv/alarmsrv 经 `common::api_types::AppError` 返回 `{success:false, error:{code, message, details?, suggestion?, field_errors?}}`，其中 `suggestion` 对用户有实际价值，而 CLI 只显示了状态码。

**本 Spec 的新代码**统一使用一个新 helper：

```rust
// tools/aether/src/output.rs
pub async fn parse_error_body(resp: reqwest::Response) -> anyhow::Error
```

它读取响应体，若能解析出 `error.message` 则拼入错误信息（有 `suggestion` 时一并附上），否则退化为状态码。

**不回改现有 7 个 client**——那是与本目标无关的重构。新 helper 为将来的回改留出样板。

## 测试

`tools/aether/Cargo.toml` 目前**没有 `[dev-dependencies]`**，8 个 client 全无测试。本 Spec 首次引入：

```toml
[dev-dependencies]
wiremock = "0.6.5"
tokio = { workspace = true }
```

按 TDD（RED → GREEN → REFACTOR）执行，每个新增 client 方法至少一个测试：

1. **路径与方法**：`Mock::given(method("PATCH")).and(path("/alarmApi/rules/7/enable"))`
2. **请求体**：写操作断言 JSON body 形状
3. **错误映射**：mock 返回 `400` + `{"success":false,"error":{"message":"…","suggestion":"…"}}`，断言 `parse_error_body` 产出的错误信息**同时包含** message 与 suggestion
4. **multipart**：`net cert upload` 断言 `Content-Type: multipart/form-data` 与 `cert_type` 字段

覆盖率目标 80%（`~/.claude/rules/testing.md`），范围限于本 Spec 新增代码。

## 验收标准

- `./scripts/quick-check.sh` 全绿（含 `mod.rs` 拦截、两遍 clippy——注意第二遍 `--lib --bins` 禁 `unwrap`/`expect`，新代码不得在运行时路径使用）
- `cargo hakari verify` 通过（Step 1 改了 reqwest feature 后必须先 `cargo hakari generate`）
- 前端能做到的 netsrv / 告警规则 / channels / instances 操作，CLI 均能做到
- 所有新命令支持全局 `--json` 与 `--host`

## 关联 Spec

| Spec | 内容 | 状态 |
|---|---|---|
| **1（本文）** | CLI / Web 功能对等 | 已定稿 |
| 2 | 审计日志后端 + CLI（`operation_log` 表、跨服务采集点、查询 API、保留期） | **未 brainstorm** |
| 3 | `aether mcp` MCP server | 已落地，见 `../../reference/mcp-tools.md` |

Spec 3 依赖本 Spec 先落地：否则 MCP 会暴露一套残缺工具。Spec 2 与两者均独立。
