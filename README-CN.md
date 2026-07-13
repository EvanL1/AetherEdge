# AetherIot

[![代码检查](https://github.com/EvanL1/AetherIot/actions/workflows/rust-check.yml/badge.svg)](https://github.com/EvanL1/AetherIot/actions/workflows/rust-check.yml)
[![许可证](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.90%2B-orange.svg)](https://www.rust-lang.org/)
[![版本](https://img.shields.io/badge/version-0.5.0-yellow.svg)](CHANGELOG.md)
[![状态](https://img.shields.io/badge/status-beta-orange.svg)](CHANGELOG.md)

[English](README.md) | [文档](https://docs.aetheriot.workers.dev/) | [变更记录](CHANGELOG.md) | [llms.txt](https://docs.aetheriot.workers.dev/llms.txt)

**面向 Linux 网关的 AI-native、行业中立 IoT Edge Kernel、Runtime 与 Rust SDK。**

AetherIot 连接现场设备，以共享内存保存权威实时状态，在本地确定性地执行规则与告警，并保存
嵌入式历史。默认运行时可完全离线工作，不需要 LLM、Redis、PostgreSQL、云服务或浏览器。

> **Beta：** AetherIot 是行业中立的 Kernel、Runtime 与 SDK。能源管理实现和发行版已拆分到
> 独立的 [AetherEMS](https://github.com/EvanL1/AetherEMS) 仓库。为保持 API 兼容，现有 Rust
> crate、二进制与 CLI 仍使用 `aether-*` / `aether` 名称。剩余发布工作由
> [ADR-0007](docs/adr/0007-aether-core-and-ems-distribution.md) 跟踪。

AetherIot 明确采用无头发行：不包含产品前端、前端镜像或前端系统服务。EMS 运维控制台由
AetherEMS 独立维护和发布，并与其他客户端一样通过经过认证的 application API 访问内核。

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

已经可用：beta 版、版本化的 domain/ports/application/data-plane SDK，Pack v1，六服务
二进制，无需外部服务的 SHM/SQLite/local-outbox 路径，SDK 示例、可选适配器和 OpenAPI
契约检查。Point 与 health 两个 SHM 平面发布同一个已提交物理 epoch，History 与 Uplink
把同一份 SQLite topology 快照绑定到该 epoch。SQLite 是已投运 topology、协议 mapping、
逻辑 route、规则与 instance 的唯一期望状态权威，并通过带 revision 的命令自动协调运行时。

仍在迁移：受支持客户端必须完成显式 channel/rule revision 的发送，之后才能移除剩余的
无 revision 兼容路径；仅测试使用的 instance/routing 直接修改 helper 已删除。本地发布流程
会按依赖顺序校验公共 crate，在 clean-room consumer 中编译其精确归档，检查既有 API 的
SemVer 兼容性，并对独立且带证明的 Kernel、CLI、crate 与 Pack 产物设门禁。物理拆仓与
AetherEMS 下游消费 CI 已落地，但尚无 tag 建立首次独立 registry/GitHub 发行，也尚未以
签名产物替换下游 bootstrap Git pin。原 EMS 前端已经迁入 AetherEMS，作为独立测试的
Console。当前事实见[架构说明](ARCHITECTURE.md)。

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
