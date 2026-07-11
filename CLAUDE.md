# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

> **Canonical policy:** read [`AGENTS.md`](AGENTS.md) first. This file retains
> detailed notes for the v0.4 compatibility services during the incremental
> edge-kernel migration; it must not override the dependency and AI-safety
> boundaries in `AGENTS.md`.

## 核心约束

单人项目，YAGNI 原则。**禁止**: `mod.rs` | 硬编码 Redis 键 | 编译时 SQLx 宏 | 过度工程化

`mod.rs` 由 `scripts/quick-check.sh` 硬性拦截（find + exit 1），不是风格建议。

## 常用命令

```bash
# 日常开发
./scripts/quick-check.sh                  # 见下方「quick-check 实际做什么」
./scripts/quick-check.sh --with-integration   # 追加集成测试（需要 Redis）
cargo test -p aether-io --lib                # 单 crate 单元测试（最快反馈）
cargo test -p aether-automation --test test_shm_dispatch  # 单个集成测试

# 前端（quick-check 不跑这些，需手动）
cd apps && npm run lint:check && npm run type-check && npm run test:run

# 构建部署
./scripts/build-installer.sh -s rust      # 用法: [VERSION] [ARCH] [TARGET] [--services=…] [--enable-swagger]
scp release/AetherEdge-arm64-*.run root@192.168.30.21:/tmp/
ssh root@192.168.30.21 '/tmp/AetherEdge-arm64-*.run'

# 配置管理
aether init && aether sync              # 配置初始化并同步到 SQLite
aether services start/stop/refresh       # 服务管理
aether doctor                            # 系统健康检查

# Docker 本地部署
docker compose up -d && docker compose ps
```

**toolchain 固定** `1.90.0`（`rust-toolchain.toml`），交叉编译目标 `aarch64-unknown-linux-musl`。

### quick-check 实际做什么

按顺序：submodule 同步/新鲜度 → **`mod.rs` 拦截** → 新内核依赖边界检查 → `cargo check --workspace` → `cargo fmt --check` →
`cargo clippy --workspace --all-targets --all-features -D warnings` → **第二遍 clippy `--lib --bins -D clippy::unwrap_used -D clippy::expect_used`** → 单元测试（`--workspace --lib --bins`）。

两个容易踩的点：

- **两遍 clippy 语义不同**：`unwrap`/`expect` 只在 `--lib --bins`（运行时代码）被禁，**测试里可以随便用**。不要去"修"测试中的 unwrap。
- **集成测试默认跳过**（需 Redis），要加 `--with-integration`；覆盖率加 `--with-coverage`（需 `cargo-llvm-cov`）。
- **前端只检查文件是否存在**，不跑 lint/type-check/test。前端校验必须手动执行（见上）。

装了 `cargo-nextest` 会自动使用（快 2–3×）。

## 服务端口

| 服务 | 端口 | 服务 | 端口 |
|------|------|------|------|
| aether-apps | 8080 | aether-io | 6001 |
| aether-automation | 6002 | aether-history | 6004 |
| aether-api | 6005 | aether-uplink | 6006 |
| aether-alarm | 6007 | aether-redis | 6379 |

## 项目结构

```
libs/
  common          — 共享 bootstrap、logging、test_utils/schema、dependency（启动依赖检查）
  errors          — 统一 AetherError + ErrorCategory → HTTP status 映射
  aether-model   — PointType、KeySpaceConfig、产品常量（编译时）
  aether-routing — RoutingCache、set_action_point
  aether-rtdb    — Rtdb trait + RedisRtdb + MemoryRtdb
  aether-rtdb-shm — 统一 SHM（UnifiedWriter/Reader/ActionWriter）、UDS notifier、PointWatch、snapshot
  aether-rules   — 规则引擎：parser → scheduler → executor
  aether-calc    — 公式求值、CalcEngine
  aether-config  — 跨平台配置 schema（aether-io/aether-automation/aether 共用）
  aether-core    — no_std 核心类型（固件共用）
  aether-shm     — 固件 SHM（RawPtrShm 裸机 SRAM 用；与 aether-rtdb-shm 是两套独立格式，勿混淆）
  aether-infra   — Redis/SQLite 连接池封装
  aether-schema-macro — proc-macro，从 Rust struct 自动生成 SQL DDL
  aether-sim     — 波形生成库（simulator 用）
  aether-script-host — ⚠️ 不是 Rust crate，是 Python（main.py），见下方「Python 脚本宿主」
services/
  io(aether-io), automation(aether-automation), api(aether-api)
  history(aether-history), uplink(aether-uplink), alarm(aether-alarm)
tools/
  aether（CLI 管理工具）, simulator
apps/            — Vue 3 + Vite 前端（ECharts 看板、Vue Flow 规则编辑器），独立 npm 工程
firmware/        — ⚠️ workspace `exclude`，targets thumbv7em-none-eabihf，`cargo test --workspace` 覆盖不到
workspace-hack/ — cargo-hakari 生成，统一 feature flags（勿手动编辑）
```

## 服务间通信

| 路径 | 机制 | 延迟 |
|------|------|------|
| aether-io → all（数据） | SHM 直写 + 后台异步 Redis 同步 | <1ms（SHM），~100ms（Redis） |
| aether-io → aether-automation（读数） | SHM mmap 零拷贝 | <1ms |
| aether-io → aether-automation（点位事件） | SubscriptionBitmap + PointWatch UDS | sub-ms |
| aether-automation → aether-io（M2C 命令） | SHM write + UDS notify | ~1–2ms |
| aether-alarm → aether-api/aether-uplink | HTTP POST | ~5ms |
| aether-uplink → cloud | MQTT | network |
| aether-api → browsers | WebSocket | network |
| all ↔ SQLite | sqlx (in-process) | local |

**启动顺序**: aether-io 必须先于 aether-automation 启动（aether-io 创建 SHM，aether-automation 验证布局）。

## Rtdb trait 设计边界

> **遗留兼容层：** 以下规则只描述 `services/` 与 `libs/` 的 v0.4
> 实现。`crates/` 新代码禁止依赖 `Rtdb`；使用 `aether-ports` 中的
> `LiveState`、`HistorySink`、`StateMirror` 等能力接口。Redis 只允许在
> `extensions/redis-bridge` 中作为可选镜像。

`Rtdb` trait（`aether-rtdb/src/traits.rs`）使用 AFIT，**不是 object-safe**，只能泛型 `<R: Rtdb>` 使用。

- **aether-io/aether-automation** 的遗留兼容结构体仍以泛型 `Rtdb` 隔离 Redis
- **aether-api/aether-history/aether-uplink/aether-alarm** 从 SQLite 解析地址并直接读取 SHM，不持有 Redis RTDB
- **MemoryRtdb 是纯测试替身**，不是 SHM 的抽象。SHM（定长 PointSlot 数组 + seqlock）和 Rtdb（KV/Hash/List/Set）数据模型不兼容
- **不要尝试**将 Rtdb trait 向其他服务传播或用于 SHM 抽象

## Instance 是纯物模型（不要染色）

Instance 表示**设备的逻辑结构 + 当前测量值**，不持有任何聚合状态字段：

- ❌ 不加 `status` / `health` / `degraded` / `alarm_state` / `online` 字段到 `inst:{id}:*`
- ❌ 不要把"channel 离线" / "有未恢复告警" / "控制写失败"等事件反向回写到 instance
- ✅ 告警是**事件**，归 aether-alarm 的告警表（通过外键引用 instance）
- ✅ 通信链路状态在 `io:online` hash（channel 维度，跟 instance 正交）
- ✅ Instance 的"当前值"用 IEEE-754 NaN 表达"暂时拿不到"（见 SHM v3 NaN 哨兵），值本身就是数据，不需要额外状态标签

四份数据**正交**，谁也不染色谁：

| 数据 | 含义 | 写入方 |
|------|------|--------|
| `inst:{id}:M/A` | 物模型当前值（可能 NaN） | aether-io（M）/ aether-automation（A） |
| `io:online` | 通信链路状态（per-channel） | aether-io |
| 告警表 | 告警事件流 | aether-alarm |
| `route:m2c` 等 | 路由配置（静态） | aether sync |

**控制写入失败**（如 channel 离线导致 aether-automation `execute_action()` 拒写）应通过返回值透传，不持久化到 instance。

**前端要灰掉控制按钮**自己做 join：`instance.action_point → routing → channel_id → io:online`，不要求后端在 instance 里聚合在线状态。

## 协议扩展

aether-io 协议通过 `ChannelRuntime` trait（object-safe，`#[async_trait]`）+ 编译时 feature gates：
1. 在 `services/io/src/protocols/adapters/` 加适配器模块
2. 实现 `ChannelRuntime` trait
3. 在 `services/io/src/protocols/gateway/factory.rs` 加 `#[cfg(feature = "...")]` 分支
4. 在 `services/io/Cargo.toml` 声明 feature

当前 14 个协议：Modbus、IEC 104、**IEC 61850 (MMS)**、OPC UA、MQTT、HTTP、DL/T 645、CAN/J1939、GPIO、BLE、Zigbee、Matter、Aether-485、Virtual。

**feature gate 是理解"协议为什么不存在"的第一现场**：

- `default = ["modbus", "gpio", "aether_485", "openapi", "iec61850", "can"]` — 其余 8 个协议默认**不编译进二进制**
- `can` / `gpio` 额外受 `#[cfg(all(feature = "…", target_os = "linux"))]` 约束 → **macOS 上无论如何都不会出现在 factory 里**
- `virtual` **无 feature gate**，永远可用（测试/仿真依赖它）
- `j1939` 隐含 `can`；`mqtt` / `http` 隐含 `json-mapping`

排查"某协议 channel 创建失败"先看 feature，再看代码。

## Python 脚本宿主（aether-io 自定义 transform）

`libs/aether-script-host/main.py` 是一个**常驻 Python 子进程**，不是 Rust crate、不在 workspace members 里。

- Rust 侧：`services/io/src/protocols/core/script_runner.rs`（`ScriptRunner`），由 `json_mapper.rs` 在 channel 配置了自定义 script 时惰性启动
- 协议：**JSON-Lines over stdin/stdout**，`{id, payload}` → `{id, points[] | error}`
- 进程**每 channel 复用一次**，避免每条消息 fork（~100ms 解释器启动开销）
- 脚本路径查找顺序：`libs/aether-script-host/main.py`（开发）→ `/etc/aether/script-host/main.py` → `/usr/local/share/aether/script-host/main.py`（部署）

改 Python 侧协议时，`ScriptRequest` / `ScriptResponse` / `PluginDataPoint` 三个结构体必须同步。

## 关键模式

```rust
sqlx::query_as::<_, Row>("SELECT * FROM t WHERE id = ?").bind(id)     // SQLx（禁编译时宏）
```

## 数据流

```
上行: Device → aether-io → SHM(T/S slots) [唯一实时面]
                      → PointWatch/事件通知 → automation/history/alarm/api/uplink
下行: aether-automation → SHM(C/A) + UDS → aether-io ShmCommandListener → Device
     （路由配置来自 SQLite，运行时数据不经过外部数据库）
```

Redis/PostgreSQL 只能通过 `extensions/` 中的桥接器显式启用；它们不得成为上述进程的启动条件、实时中转或控制路径。默认历史落本地 SQLite。
`ReverseSlotIndex`（slot → channel/point 反向映射）用于把 SHM 变更还原为领域地址并分发事件。

## SHM 架构

**文件**: `AETHER_SHM_PATH` → `/shm/rtdb/aether-rtdb.shm`（Docker）→ `/dev/shm/...`（Linux）→ `/tmp/...`（macOS），`UnifiedHeader(64B) + PointSlot[N](32B each)`

**写者所有权（类型强制）**: aether-io 拥有 T/S 槽，aether-automation 拥有 C/A 槽；跨写在类型层面不可表达。

**关键 header 字段**:
- `routing_hash` — aether-io 和 aether-automation 必须匹配
- `writer_generation` — aether-io 发布新 generation，aether-automation 检测切换

**M2C 通知**: `ShmNotifier` → UDS(`/tmp/aether-m2c.sock`，bind 后 chmod 0600) → `ShmCommandListener`
- 48 字节 `ShmNotification`（bytemuck Pod），`producer_id + seq` 去重（wrapping 比较）
- UDS 失败自动重连（指数退避 1–5s），无轮询降级

**Seqlock**: `try_load_consistent()` 单次尝试（tokio worker 上的后台任务必须用它，如 ShmRedisSync）；`load_consistent()` 自旋重试最多 ~3–16ms（仅限专用线程），耗尽返回 None，不返回撕裂数据

**SHM 重建**: 走 `ShmHandle::rebuild_via_swap` 原子换页；per-generation 文件 + aether-automation inode watcher 实现跨进程感知。

**PointWatch 事件平面（v0.4.0）**: aether-io 每次 T/S 写后查 `SubscriptionBitmap`（独立 mmap 文件，aether-automation 按规则订阅写入），命中才发事件 → UDS(`/tmp/aether-point-watch-automation.sock`) → aether-automation `PointWatchListener` → `PointWatchDispatcher`（`(channel,point)→rule_ids` 索引）→ 有界通道(1024) `try_send` 进 scheduler，满载丢弃 + `dropped_count` 计数。事件只是提示，规则判定总会重新读取 SHM。

## 规则引擎

**双列存储（关键不变量）**:
- `flow_json` — 前端 Vue Flow 完整 JSON（含 UI 布局）
- `nodes_json` — 紧凑执行拓扑（`RuleFlow { start_node, nodes }`）
- **两列只能经 `aether_rules::flow_column_values()` 一起产出**（返回 `FlowColumns` 结构体）。现有三个写入点（`repository::upsert_rule`、`automation/src/rule_routes.rs` PUT、`tools/aether` `syncer.rs`）全走它；新增 rules 表写路径必须同样收口，不要自己 serialize 任一列

**执行流**: Scheduler（100ms tick + PointWatch 事件混合，`tokio::select!`；Interval 规则走 tick，OnChange 规则走事件+死区）→ Executor → RTDB write + SHM C/A write（经 `ActionDispatch`）+ UDS notify
**执行结果**: SQLite `rule_history` 是持久记录；Redis `rule:{id}:exec` 仅是可选最新结果镜像，aether-api 从本地历史读取诊断。
**热加载**: `RuleScheduler::reload_rules` 原子重建 SubscriptionBitmap + dispatcher 订阅索引 → `POST /api/scheduler/reload` 规则改动立即生效，无需重启服务

## 配置流

```
config/*.yaml → aether sync → SQLite(aether.db) → 服务启动时加载
```

服务不直接读 YAML，所有配置经 `aether sync` 写入 SQLite 后生效。

## 错误处理

`errors` crate: `AetherError`（42 variants）→ `ErrorCategory`（17 variants）→ HTTP status。
`AetherErrorTrait` 定义统一接口（`error_code()`、`is_retryable()`）。HTTP status 通过 `AetherError::status_code()` 映射。

- **aether-io**: `IoError`（15 variants）实现 `AetherErrorTrait`
- **aether-automation**: `AutomationError` 实现 `IntoResponse`
- **aether-history/aether-uplink/aether-alarm**: `anyhow::Result` + handler 错误映射
- **aether-api**: 管理 API 错误映射

**API 响应格式（事实约定,2026-06 审定）**:

- 成功统一为 `{ success: true, data, metadata? }`（`common::api_types::SuccessResponse`）
- 错误响应有三种形状共存（历史原因,前端已按各自路径解析,**不要批量迁移**）:
  - 类型化错误（aether-io 经 `AppError`、aether-automation 经 `AutomationError`）: `{ success: false, error: { code, message, details?, suggestion?, field_errors? } }`
  - handler 内联校验（四个外围服务）: `{ success: false, message }`
  - `AetherError` 的 `into_http_response` 形状: `{ error_code, message, category, retryable, retry_delay_ms }`
- **新代码**: 错误优先用所在服务的类型化错误（`AutomationError`/`IoError`/`AppError`,实现 `IntoResponse`）,不要新增内联 `json!({"success": false, "message"})`

## 测试

- `common::test_utils::schema` — 共享 DDL 常量（`init_io_schema()`、`init_automation_schema()`、`init_rules_schema()`）
- `create_test_rtdb()`（`aether-rtdb` crate）→ `Arc<MemoryRtdb>`（单元测试不需要 Redis）
- `NoopDispatch`（`automation/src/infra/shm_dispatch.rs`）→ 无 SHM 的 `InstanceManager` 测试；`noop_dispatch()` 包装函数在 `automation/src/instance_manager_tests.rs` 内（测试局部，未导出）
- 集成测试需 Redis，用 `tempfile::TempDir` 做 SQLite
- aether-history `StorageBackend` 支持本地 SQLite 及可选外部后端
- CI 会扫描 `#[ignore]` 测试并以 warning 暴露——加 `#[ignore]` 不会被静默接受

## workspace-hack (cargo-hakari)

`workspace-hack/` 由 cargo-hakari 生成，统一所有 crate 的 feature flags 以提升编译缓存命中率。

```bash
cargo hakari generate          # 依赖变更后重新生成
cargo hakari manage-deps       # 新增 crate 后同步依赖
cargo hakari verify            # CI 中验证一致性（已集成到 quality-check job）
```

**不要手动编辑** `workspace-hack/Cargo.toml`。
