# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.5.0] - 2026-07-10 — AI-native SHM Edge Kernel

This release turns Aether from an energy-shaped application repository into an
industry-neutral IoT edge kernel and SDK. The production runtime keeps six
independently restartable processes while making shared memory the sole
authoritative live-state plane.

### Added

- Public `aether-domain`, `aether-ports`, `aether-application`, `aether-sdk`,
  `aether-dataplane`, and `aether-testkit` crates with explicit capability
  boundaries and package-ready metadata.
- Zero-external-service local adapters and a runnable `minimal-gateway`
  composition.
- Optional `redis-bridge` and `postgres-history` extensions; neither is in the
  default SDK or edge composition.
- AI-facing CLI/MCP documentation, architecture invariants, ADRs, conformance
  checks, and SHM-only dependency gates.

### Changed

- Renamed the six process roles to `aether-io`, `aether-automation`,
  `aether-history`, `aether-api`, `aether-uplink`, and `aether-alarm`.
- I/O owns authoritative telemetry/status SHM writes; automation dispatches
  control/action through SHM plus UDS event hints. Other services consume SHM
  through read-only capability adapters.
- SQLite is the embedded configuration, metadata, audit, rule, and history
  store. Redis and PostgreSQL are opt-in extension profiles only.
- Minimum supported Rust version is 1.90 and is pinned by
  `rust-toolchain.toml` for local and CI builds.

### Breaking

- Removed all compatibility aliases for the former service and binary names.
- Removed Redis-backed live-state APIs and fallback behavior from the core
  runtime. A missing or invalid SHM layout now fails closed.
- The workspace and public crate version is now `0.5.0`; downstream users must
  select infrastructure adapters explicitly through `aether-ports`.

## [0.4.0] - 2026-05-29 — Sub-millisecond Event Plane (亚毫秒事件平面)

事件驱动 SHM 路径上线，并离网切换关键路径压到 ~1.5 ms（之前 Redis tick 模型 50–150 ms）。生产硬件（Cortex-A55 @ 1.4 GHz / EdgeLinux 22.04）端到端实测 P50 = 206 µs。

### Added
- **rtdb-shm**: **PointWatch** 事件驱动通知子系统 — 反向 M2C：comsrv `set_direct` → SubscriptionBitmap 检查 → 56 B PointWatchEvent → UDS → modsrv PointWatchListener → RuleScheduler 即时唤醒。彻底消除 100 ms tick 等待 + Redis HMGET 拉取。
- **rtdb-shm**: `SubscriptionBitmap` (mmap 共享) + `PointWatchSignaler` + `PointWatchListener` + 独立 UDS socket `/tmp/aether-point-watch.sock`。
- **rules**: `PointWatchDispatcher` 订阅索引 `(channel_id, point_id) → Vec<rule_id>`，事件→规则路由。
- **rules**: `RuleScheduler::reload_rules` 现在原子重建 SubscriptionBitmap + dispatcher 订阅索引——`POST /api/scheduler/reload` 规则改动立即生效，无需 service restart。
- **routing**: `c2m_reverse` `FxHashMap` 反向索引（O(1) instance→channel 查找），消除 O(N²) routing scan bug。
- **rtdb-shm**: `ShmHandle::rebuild_via_swap` 原子换页式 SHM 重建（修复 reconfigure_existing 的撕裂窗口）。
- **rtdb-shm**: per-generation file 文件机制 + modsrv inode watcher，跨进程感知 SHM 重建。
- **shm core**: 提取 `SlotReader` / `SlotWriter` / `SlotIo` trait — 纯基础设施层，与业务路由解耦。
- **bench**: 控制链路 baseline 套件（`control_chain.rs`，含 SHM 热路径 + M2C UDS + Phase 0 HMGET vs SHM-direct）。
- **bench**: PointWatch 端到端延迟 bench（`pointwatch_e2e.rs`，跨 Tokio runtime UDS round-trip）。
- **docs**: `docs/plans/2026-05-28-point-watch-design.md`（946 行设计文档），`BASELINE.md` 含 Apple M3 Pro + ECU-1170 双硬件实测。

### Changed
- **rules**: OnChange Phase 0 `fetch_point_snapshot` 优先走 SHM 直读（取代 Redis HMGET）。N=1000 订阅点时延迟 5.25 ms → 1.44 ms（A55 实测，3.65× 加速）。
- **rtdb-shm**: 移除 legacy `reconfigure_existing`、orphan 文件清理，统一走 atomic-swap rebuild。
- **comsrv/modsrv**: PointWatch bootstrap 集成入 main.rs（`SubscriptionBitmap::create`/`open` + signaler/listener + bridge task）。
- **modsrv**: `PointWatchDispatcher` 包成 `Arc<std::sync::Mutex<>>` 在 bridge task 与 scheduler reload 间共享（std mutex 选型理由：dispatch hot path 不 .await）。
- **build**: cargo-hakari workspace-hack 重新生成（含 SHM core/ 重构后的依赖图）。

### Fixed
- **routing**: 反向 M2C 查找原本 O(routes) 线性扫描，导致 N=1000 时比 Redis HMGET 还慢 9×。加 `c2m_reverse` 索引后 O(1)。
- **rules,modsrv**: PointWatchDispatcher mutex 改用 `unwrap_or_else(|p| p.into_inner())` 替代 `expect()`，满足 CI clippy panic-boundary check + 标准 poison-recovery 语义。
- **comsrv**: `routes` 测试更新匹配 SHM-aware health + fail-closed C/A writes。
- **ci**: aether `export` 子命令语法修正。

### Performance (Cortex-A55 @ 1.4 GHz, ECU-1170 production hardware)
| 路径 | 旧 (Redis tick) | 新 (PointWatch event) | 加速 |
|---|---|---|---|
| OnChange Phase 0 (N=1000) | 5.25 ms | 1.44 ms | 3.65× |
| Tick alignment wait | 0–100 ms | 0 | ∞ |
| **端到端 P50** | **50–150 ms** | **206 µs** | **~500×** |
| 端到端 P99 | (same) | 526 µs | — |
| 端到端 P99.9 | (same) | 1.4–2.2 ms | — |

→ 20 ms 并离网切换 SLA 中位数仅消耗 **1.03 % 预算**，P99 消耗 **2.63 %**。

### Compatibility
- **API**: `POST /api/scheduler/reload` 行为增强（兼容）——规则改动后 SubscriptionBitmap + PointWatchDispatcher 也会重建。
- **SHM 协议**: PointWatchEvent (56 B 定长) 与 ShmNotification (48 B) 兼容并存，两个独立 UDS socket。
- **依赖关系**: comsrv 必须先于 modsrv 启动以创建 SubscriptionBitmap mmap 文件。

---

## [0.3.0] - 2026-04-21 — First Beta (首个内测版本)

首个内测版本。功能完备度与稳定性已达到小范围试用标准，欢迎内部测试反馈。

### Added
- **comsrv**: IEC 61850 协议支持（MMS），至此支持 14 种协议
- **comsrv**: 每通道独立日志配置（create/update flow 贯通）
- **comsrv**: `:ts` 时间戳 sidecar 写入 `inst:{id}:{M,A}` Redis 哈希
- **alarmsrv**: 告警规则 / 事件支持关键字模糊搜索（rule_name / description / channel_id / point_id）(#87)
- **apigateway**: WebSocket 原生 Ping 心跳（替代应用层 JSON heartbeat，浏览器自动 Pong）(#87)
- **apigateway**: 新增 nginx `/hisApi/` 路由接入历史数据服务
- **hissrv**: TimescaleDB 数据库压缩策略
- **aether**: channels / points CRUD 接口
- **aether**: 日志文件查看器（list / view / tail / ui 子命令）
- **aether**: 远程日志查看（HTTP API）
- **aether**: sync 默认 upsert 模式（`--force` 触发全量覆盖）
- **modsrv**: SHM 自动重建 + dispatch degraded 错误码 + UDS 健康状态
- **build**: Swagger UI 改为可选 cargo feature（生产构建瘦身）
- **frontend**: 组件定义增强，字体资源更新

### Changed
- **comsrv**: SHM → Redis 异步刷写，热路径移除 DashMap（性能优化）
- **rules**: RPN 执行器替换为 evalexpr 中缀引擎
- **rules**: 点位字段统一为 `point_id`（从 Vue Flow variables 读取）
- **rtdb-shm**: 通过 `ChannelPointCounts` 解耦 SHM 布局与路由
- **comsrv/modsrv**: 解耦路由刷新与 SHM 重建
- **aether-model**: 产品 JSON 由 `build.rs` 自动发现（无需手动注册）
- **CI**: 采用 cargo-nextest（测试提速 2–3x），引入 cargo-hakari + resolver v3
- **CI**: 统一 release pipeline，清理 Python 时期遗留

### Fixed
- **SHM seqlock**: 重试耗尽返回 `None`，杜绝撕裂读
- **SHM**: `writer_generation` 计数器检测 comsrv 重启，避免 TOCTOU
- **SHM**: UDS 最大退避降至 5s；新增 dropped command 计数器
- **modsrv**: dispatch 降级 → HTTP 502（区分 UDS 降级 vs 内部错误）
- **aether**: 迁移 v4 修复 `trigger_config`，跳过非 Vue Flow 规则
- **aether**: sync 通过 `extract_rule_flow()` 同步 `nodes_json`（避免 flow_json / nodes_json 错位）
- **aether**: 导出补齐 points / mappings（per-type tables）、templates、per-rule JSON
- **resilience**: SIGTERM 优雅停机，health 503，warning 统计，Redis 重试
- **install**: `gunzip` 管道替代 unpigz（避开 zlib 错误），收紧权限，构建全 6 服务镜像
- **timescaledb**: 固定 `2.25.2-pg17` 兼容性
- **websocket**: 补齐无 `ts` 时间戳与首页订阅问题
- **i18n**: 部分消息响应统一改英文

### Infra / Housekeeping
- 打包脚本调整，适配单服务测试更新
- 抑制 `aether` 启动 banner（init 命令除外）

## [0.2.0] - 2026-03-18

### Breaking Changes — Full Rust Migration
- **All services now Rust** — hissrv, apigateway, netsrv, alarmsrv migrated from Python/FastAPI to Rust/Axum
- **Python services removed** — `services/python-services/` directory deleted; all 6 services live under `services/`
- **Pluggable storage replaces InfluxDB** — hissrv now uses a runtime-configurable backend (PostgreSQL / TimescaleDB) via `PUT /hisApi/storage`
- **Unified Docker image** — all Rust services share a single `aetherems:latest` Alpine-based image

### Added
- **aether**: Remote management CLI with `--host` flag for all commands
- **aether**: Interactive TUI dashboard (`aether top`) with local and remote monitoring
- **aether**: JSON output mode (`--json`) for AI agent and script integration
- **aether**: Channel template API — snapshot, apply, list templates
- **aether**: Cross-platform release pipeline (Linux ARM64/AMD64, macOS, Windows)
- **apigateway**: JWT authentication, WebSocket proxy, unified REST API (Rust rewrite)
- **hissrv**: Pluggable storage backend (PostgreSQL / TimescaleDB) with runtime configuration via REST API
- **netsrv**: MQTT client with TLS support, device telemetry forwarding
- **alarmsrv**: Alarm rule evaluation, notification management, CSV export

### Refactored
- Net reduction of ~7,000 lines of code despite adding 4 new Rust services
- Purge 23 unused Cargo dependencies + dead functions/types across workspace
- Remove tombstone comments, ghost imports, zombie Redis writes
- Clean up dead VecRtdb, RingBuffer, snapshot_info, health_check code

### Fixed
- CI: multi-arch build fixes, NPM_TOKEN handling, aether tag filtering
- Tests: fix trailing comma in calculations DDL causing 16 integration test failures
- Remove dead CanMappingConfig exposed by Linux-only can feature gate

## [0.1.11] - 2026-03-12

### Performance
- **shm**: Zero-cost seqlock on x86 — eliminate mfence/lock instructions, use compiler fences only

## [0.1.10] - 2026-03-11

### Fixed
- **modsrv**: Harden M2C dispatch safety — error propagation, stale writer clear, refresh lock
- **modsrv**: Correct SHM refresh ordering, remove dead code
- **modsrv**: Propagate comsrv reload error, remove phantom TODO queue references
- **modsrv**: Address code review — DispatchOutcome observability, SHM consistency
- **modsrv**: Eliminate TOCTOU races, harden error handling and API safety
- **comsrv**: Filter by point_type in Modbus write_control/write_adjustment
- **comsrv**: Use OS-assigned port in integration tests to eliminate flaky port conflicts
- **rtdb-shm**: Fix unsafe soundness in ring buffer and snapshot restore
- **shm**: Harden shared memory safety for ARM64 weak memory model
- **e2e**: Harden Phase 6 routing and Phase 9 reset verification; fix Redis socket permission

### Refactored
- **modsrv**: Replace DispatchOutcome 3-bool struct with enum
- **modsrv**: Extract infra/runtime layers, wire ShmDispatch + ComsrvCoordinator
- **modsrv**: Delete unwired skeletons, wire DynamicSlotRuntime
- **modsrv**: Delete dead ShmCommandPoller; purge all stale M2C fallback/polling references
- **modsrv**: Deduplicate Redis sync via Acquire trait generalization
- **comsrv**: Extract ChannelPollContext to reduce function parameter count

### Added
- **simulator**: Device state machine with Modbus write hooks; CAN/J1939 E2E scenarios
- **simulator**: HTTP state API, CAN LYNK sender, J1939 sender; Modbus protocol readback
- **comsrv/modsrv**: Refactor rtdb-shm notification API; enable Redis Unix socket hot path
- **comsrv**: Watchdog with auto-recovery, heartbeat liveness, and health endpoint
- **comsrv**: JSON mapper with JSONPath extraction and script fallback
- **comsrv**: Add protocol field to GET /api/channels/list response

## [0.1.9] - 2026-02-26

### Added
- **comsrv**: BLE (Bluetooth Low Energy) protocol adapter
- **comsrv**: Zigbee protocol adapter via TCP gateway
- **comsrv**: Matter protocol adapter with UDP transport
- **comsrv**: Channel template API for point-table snapshot and apply
- **modsrv**: Auto-reload services after aether sync
- **modsrv**: Enforce instance topology hierarchy with cascade delete and topology API
- **comsrv**: OpenAPI/Swagger docs for template API

### Fixed
- Stabilize flaky seqlock concurrent test for CI coverage runs
- Soften topology hierarchy validation to warn-only for flexible topologies
- CI: install libdbus-1-dev for BLE adapter compilation
- **comsrv**: Remove panics, unwraps, and reconnect backoff blocking
- **aether-rtdb-shm**: Seqlock fallback torn read + ringbuffer push guard
- CI: ARM64 native runner; dependency-aware service restart; clippy threshold unification

### Refactored
- **comsrv**: Extract state mapping, cleanup deprecated code, convert macros to functions
- Large-scale simplification — deduplicate, extract, remove dead code
- Security, stability, and performance audit remediation; translate remaining Chinese log messages

## [0.1.7] - 2026-02-11

### Added
- **comsrv**: Channel online status tracking (real-time heartbeat in Redis)
- **aether-model**: Runtime ProductLibrary with external JSON overrides
- **aether-model**: PVInverter product; align all product names
- **modsrv**: Propagate rule_name in execution results; normalize legacy product names
- **modsrv**: Batch_direct and routing_cache integration tests

### Fixed
- **comsrv**: Prevent CAN client deadlock; clarify protocol safety docs
- **modsrv**: Parameterize SQL queries to prevent injection
- **logging**: Enforce log retention and prevent disk overflow
- Pre-existing test failures resolved

### Refactored
- **modsrv**: Replace ghost table SQL with in-memory product lookups
- **comsrv**: Split point_handlers.rs into module directory
- **aether-rtdb**: Extract SHM zero-dep modules to aether-rtdb-shm crate
- **aether-rtdb**: Remove unused Rtdb trait methods
- Move routing_cache to aether-routing; complete SHM extraction
- **aether-rtdb-shm**: Simplify ChannelToSlotIndex to slot indices

## [0.1.6] - 2026-01-13

### Refactored
- **comsrv**: Replace u32/4 internal ID encoding with explicit point_type field
- **comsrv**: Implement lock-free diagnostics and Arc\<DataBatch\> optimization
- **comsrv**: Implement lock-free polling with channel-based architecture
- **rules**: Optimize scheduler with Arc\<Rule\> and parallel execution
- Simplify modsrv errors and eliminate syncer.rs duplication

### Fixed
- **gpio**: Use MockGpioDriver in tests for hardware-independent testing
- Clippy: use is_multiple_of() instead of manual modulo check

## [0.1.5] - 2026-01-09

### Added
- **aether**: shm command with TUI dashboard and SHM iteration API
- **install.sh**: Auto-start and cleanup on install
- **frontend**: Vue.js build and checks integration

### Fixed
- **install**: Improve architecture detection and Docker Compose compatibility
- **build**: Use musl for amd64 to fix Alpine compatibility
- **ci**: Support multi-arch Docker builds with TARGET_TRIPLE ARG
- **test**: Update assertions to match ryu float formatting
- **aether**: Cleanup redundant code and unused dependencies

## [0.1.4] - 2026-01-05

### Added
- DI/DO channel: internal_id conversion and improved logging

### Performance
- Multi-round performance optimization for core libraries (aether-core, aether-model, aether-routing)

### Fixed
- **comsrv**: Make start_flush_task async to avoid blocking_write panic
- **comsrv**: Add defensive Drop impl for IgwChannelWrapper; abort background tasks on hot-reload
- Improve type safety and error handling across concurrency paths
- Add safety bounds for Redis retry and integer conversions
- Improve task lifecycle management robustness
- Update igw to 0.2.16 (GPIO startup init)

## [0.1.2] - 2026-01-04

### Added
- Multi-arch installer support (ARM64 + AMD64)
- GPIO: use sysfs driver for simpler global numbering

### Fixed
- **comsrv**: Register point types for correct Redis key mapping
- **build**: Fail fast when aether binary is missing; add --platform linux/arm64 flag
- **ci**: Replace non-existent aether validate with sync --dry-run
- **rules**: Fail condition evaluation when variable is missing
- Remove dangerous Default implementations and validate critical IDs
- Correct volume mount path for data directory
- Update igw to 0.2.14-0.2.13; remove voltage_modbus and tokio-serial dependencies

### Refactored
- **comsrv**: Replace Chinese log messages with English
- **apigateway**: Unify WebSocket rule message format
- **install**: Change default path to /opt/AetherEdge; enable auto mode by default
- Unify clippy lints to workspace level; optimize Cargo.toml dependencies

## [0.1.1] - 2025-12-25

### Refactored
- **comsrv**: Simplify point handlers with wrapper pattern

---

## [0.1.0] - 2025-12-24

### First Stable Release

First stable release of AetherEMS, an Industrial Energy Management System designed for edge computing environments.

### Core Services (Rust)

#### comsrv (Communication Service) - Port 6001
- **10 protocol adapters**: Modbus TCP/RTU, IEC 60870-5-104, OPC UA, MQTT, HTTP, DL/T 645-2007, CAN bus, J1939, GPIO, Virtual
- **Four-remote point types**: Telemetry (T), Signal (S), Control (C), Adjustment (A)
- Batch data upload to Redis with configurable intervals
- Hot-reload configuration via REST API
- Swagger UI documentation

#### modsrv (Model Service) - Port 6002
- Product and instance management with hierarchical definitions
- Routing engine: C2M, M2C, C2C data flow mapping
- Rule engine with cron and condition-based triggers (Vue Flow compatible)
- Virtual points with expression-based calculations (evalexpr)
- Full REST API with Swagger UI

#### aether (CLI Tool)
- Configuration management: init, sync, status, validate
- Service management: start, stop, restart, refresh, logs, reload
- Routing commands: set-action, routing-show

### Auxiliary Services (Python, migrated to Rust in v0.2.0)
- **hissrv** (6004): Historical data storage with InfluxDB 3.x
- **apigateway** (6005): Unified API gateway, WebSocket proxy, authentication
- **netsrv** (6006): MQTT cloud connectivity, HTTP forwarding
- **alarmsrv** (6007): Alarm rule evaluation, notification management

### Frontend
- **apps** (8080): Vue.js 3 + TypeScript, real-time dashboard, configuration management UI

### Infrastructure
- **Redis 8**: Real-time data store with Unix socket support
- **InfluxDB 3**: Time-series database (replaced by TimescaleDB in v0.2.0)
- **Docker Compose**: Host network mode, health checks, volume mounts

### Libraries

| Library | Description |
|---------|-------------|
| aether-core | Core types and codecs (no_std compatible) |
| aether-model | Product definitions, calculations, instance management |
| aether-routing | Data flow routing between comsrv and modsrv |
| aether-rtdb | Real-time database abstraction (Redis and in-memory) |
| aether-rtdb-shm | Shared memory RTDB via /dev/shm |
| aether-shm | Platform-agnostic shared memory readers/writers |
| aether-infra | Redis and SQLite integration |
| aether-calc | Expression evaluation engine |
| aether-rules | Rule engine with Vue Flow parsing and scheduling |
| aether-sim | Waveform generator for device simulation |
| aether-schema-macro | Proc macro for SQL DDL generation |
| common | Service bootstrap, config management, shared utilities |
| errors | Unified error types |
