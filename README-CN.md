# Aether

[![代码检查](https://github.com/EvanL1/Aether/actions/workflows/rust-check.yml/badge.svg)](https://github.com/EvanL1/Aether/actions/workflows/rust-check.yml)
[![许可证](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.90%2B-orange.svg)](https://www.rust-lang.org/)
[![版本](https://img.shields.io/badge/version-0.5.0-yellow.svg)](CHANGELOG.md)
[![状态](https://img.shields.io/badge/status-beta-orange.svg)](CHANGELOG.md)

[English](README.md) | [文档](https://docs.aetheriot.workers.dev/) | [变更记录](CHANGELOG.md) | [llms.txt](https://docs.aetheriot.workers.dev/llms.txt)

**面向 Linux 网关的 AI-native、行业中立 IoT Edge Kernel、Runtime 与 Rust SDK。**

Aether 连接现场设备，以共享内存保存权威实时状态，在本地确定性地执行规则与告警，并保存
嵌入式历史。默认运行时可完全离线工作，不需要 LLM、Redis、PostgreSQL、云服务或浏览器。

> **Beta：** 当前仓库是 Aether Kernel 与可选 AetherEMS 能源发行版的集成工作区。行业中立
> Kernel 已可使用；剩余兼容迁移由
> [ADR-0007](docs/adr/0007-aether-core-and-ems-distribution.md)跟踪。

## 体验 SDK

以下组合不需要外部服务，也不会投运任何硬件：

```bash
cargo run -p aether-example-minimal-gateway
cargo run -p aether-example-energy-gateway
```

前者是行业中立的空网关；后者叠加可选的 [Energy Pack](packs/energy)。它们是 SDK 冒烟
测试，不是受监管的生产运行时。

## Edge Runtime

| 进程 | 职责 |
|---|---|
| `aether-io` | 协议采集；唯一的遥测/状态写入者 |
| `aether-automation` | 实例、规则与经审计的控制分发 |
| `aether-alarm` | 告警计算与生命周期 |
| `aether-history` | 嵌入式历史与可选历史适配器 |
| `aether-api` | 认证管理 API 与 WebSocket |
| `aether-uplink` | 通过本地持久 outbox 交付云端/MQTT 数据 |

请从[快速开始](docs/guides/getting-started.md)中的安全空配置开始，最终用 `aether doctor`
验收。浏览器客户端、外部数据库和云连接均为可选项。

## Swagger UI

内置接口文档由各服务的 Rust OpenAPI 契约生成，并受 feature 控制。构建 Edge 安装包时可
一次为六个服务启用：

```bash
./scripts/build-installer.sh v0.5.0 arm64 -s rust --enable-swagger
```

| 服务 | Swagger UI | OpenAPI JSON |
|---|---|---|
| `aether-io` | `http://127.0.0.1:6001/docs` | `http://127.0.0.1:6001/openapi.json` |
| `aether-automation` | `http://127.0.0.1:6002/docs` | `http://127.0.0.1:6002/openapi.json` |
| `aether-history` | `http://127.0.0.1:6004/docs` | `http://127.0.0.1:6004/openapi.json` |
| `aether-api` | `http://<edge-host>:6005/docs` | `http://<edge-host>:6005/openapi.json` |
| `aether-uplink` | `http://127.0.0.1:6006/docs` | `http://127.0.0.1:6006/openapi.json` |
| `aether-alarm` | `http://127.0.0.1:6007/docs` | `http://127.0.0.1:6007/openapi.json` |

只有 `aether-api` 设计为可远程访问，其余五个服务必须保留在 loopback。文档路由本身公开，
也不会绕过操作鉴权。已治理的 channel、automation、alarm 与 Data Processing 操作会在
Swagger 中声明认证、确认、关联 ID、已接受/降级结果与审计契约；其余服务本地管理接口仍
属于迁移范围。只应在受信的投运网络中启用 Swagger。

## 架构与安全

```text
设备 -> aether-io -> 权威 SHM
                    |-> 自动化与告警
                    |-> API 与嵌入式历史
                    `-> 持久 outbox -> 可选云端

domain <- ports <- application <- runtime/interfaces
             ^
             `---- extensions
```

- SHM 是实时点状态权威；外部存储只能镜像它。
- 只有采集侧能写遥测/状态；应用接口只能读取。
- 设备控制默认拒绝，必须经过权限、确认、校验与审计。
- HTTP、CLI 与 MCP 的 channel 投运、外部设备动作、手动规则执行和物理 action-routing
  变更共享 application command 边界；MCP 写操作还需显式 `--allow-write`。
- AI 不进入协议轮询或硬实时安全闭环。

## 成熟度

已经可用：带类型的 domain/ports/application/data-plane 与 Pack v1 crate，六服务二进制，
无需外部服务的 SHM/SQLite/local-outbox 路径，SDK 示例、可选适配器和 OpenAPI 契约检查。

仍在迁移：敏感 channel 配置查询、instance、point/template/provisioning、
measurement-routing、history、uplink 与其他 config 操作需要完整的 application
command/query 边界；仅开发/测试使用的兼容层仍需删除；Aether/AetherEMS 还需要独立签名
发布、下游消费 CI 与实际拆仓。当前事实见
[架构说明](ARCHITECTURE.md)。

## 文档

- [快速开始](docs/guides/getting-started.md)
- [连接设备](docs/guides/connect-devices.md)
- [HTTP API 与 Swagger](docs/reference/http-api.md)
- [连接 AI 助手](docs/guides/ai-assistants.md)
- [部署指南](docs/guides/deployment.md)
- [架构说明](ARCHITECTURE.md)与 [ADR 索引](docs/adr)

## 开发验证

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --lib --bins
./scripts/check-openapi-contracts.sh
./scripts/check-architecture.sh
```

依赖外部服务的测试不属于默认验证路径。

## 许可证

可任选 MIT 或 Apache-2.0。详见 [LICENSE](LICENSE)。
