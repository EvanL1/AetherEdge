# AetherEdge

[![代码检查](https://github.com/EvanL1/AetherEdge/actions/workflows/rust-check.yml/badge.svg)](https://github.com/EvanL1/AetherEdge/actions/workflows/rust-check.yml)
[![许可证](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.90%2B-orange.svg)](https://www.rust-lang.org/)
[![版本](https://img.shields.io/badge/version-0.5.0-yellow.svg)](CHANGELOG.md)
[![状态](https://img.shields.io/badge/status-beta-orange.svg)](CHANGELOG.md)

**文档网站：** [docs.aetheriot.workers.dev](https://docs.aetheriot.workers.dev/)

[快速开始](docs/guides/getting-started.md) · [在线文档](https://docs.aetheriot.workers.dev/) · [Agent Skill](skills/aether-iot/SKILL.md) · [MCP](docs/guides/ai-assistants.md) · [English](README.md)

**用 AI 构建可靠的边缘 IoT 应用。**

AetherEdge 是开源、行业中立的 Linux 网关 IoT Edge Kernel、Runtime 与 Rust SDK。它连接
现场设备，以共享内存保存权威实时状态，在本地确定性地执行规则与告警，并保存嵌入式历史；
默认运行不依赖 Redis、PostgreSQL、云服务、浏览器或 LLM。

AetherEdge 是 [AetherIoT 平台](docs/overview/platform.md)的边缘产品，与
[AetherCloud](https://github.com/EvanL1/AetherCloud) 和
[AetherContracts](https://github.com/EvanL1/AetherContracts)共同组成核心产品族。本仓库原名
`EvanL1/AetherIot`；迁移期间软件标识保持稳定，详见
[迁移说明](docs/migration/aetheriot-to-aetheredge.md)。

AI 是 AetherEdge 的客户端，不进入硬实时闭环。Agent、CLI、生成式应用和操作界面都通过同一
套类型化 command/query 边界访问系统；设备控制始终默认拒绝，并要求明确确认和完整审计。

> **Beta：** AetherEdge 是行业中立的 Kernel、Runtime 与 SDK。现有 crate、二进制、CLI 和
> 部分兼容产物仍使用 `aether-*` / `aether` 名称。官方能源管理实现位于独立的
> [AetherEMS](https://github.com/EvanL1/AetherEMS) 仓库。

## 安装 AetherEdge

AetherEdge 不是 npm 或 Bun 包；`npx` 和 `bunx` 都不能安装 Kernel、Runtime、CLI 或 Rust SDK。

在基于 Docker 的 Linux Edge 主机上，从
[GitHub Releases](https://github.com/EvanL1/AetherEdge/releases)下载与目标架构匹配的
`AetherEdge-<arch>-<version>.run` 及其 `.sha256` 文件，然后在目标主机上校验并运行这个仅支持
全新部署的安装包：

```bash
sha256sum -c AetherEdge-<arch>-<version>.run.sha256
chmod +x AetherEdge-<arch>-<version>.run
sudo ./AetherEdge-<arch>-<version>.run
```

`.run` 安装包会安装六服务 Edge Runtime 和 `aether` CLI。Release 还包含独立 CLI 压缩包，
但它们不会安装 Runtime。源码检出或 SDK 开发见[快速开始](docs/guides/getting-started.md)；
`cargo install --path tools/aether --locked` 也只安装 CLI。Docker 与裸机安装包契约见
[部署指南](docs/guides/deployment.md)。

## 可选：连接 AI Agent

仓库里的 Agent Skill 只是可选的开发指导，不是 AetherEdge 软件包。需要时按
[使用 AI 构建应用](docs/guides/build-applications-with-ai.md)添加到兼容的编码助手。

把正在运行的 Edge 系统作为默认只读的 MCP tools 接入：

```bash
claude mcp add aether -- aether mcp
```

然后直接告诉助手：

```text
从 AetherEdge 开始。检查这个仓库，并根据当前 Edge Runtime 暴露的能力，
生成一个只读运维应用。
```

可选 Skill 提供开发方法，并按需读取在线 Markdown 文档；MCP 提供实时、结构化的系统能力。
只有操作员显式启动写入会话时，写工具才会注册；每次写入仍必须通过服务端权限、确认、校验
和审计边界。

完整客户端契约见[使用 AI 构建应用](docs/guides/build-applications-with-ai.md)，安全空运行时的
完整安装流程见[Agent Quickstart](https://docs.aetheriot.workers.dev/agent-quickstart/)。

## AetherEdge 提供什么

- **确定性 Edge Runtime**：没有 AI 客户端时，六个隔离 Rust 服务仍持续采集、执行规则、
  处理告警、保存历史并完成 uplink。
- **Local-first 数据平面**：SHM 是实时点状态权威；SQLite 提供嵌入式期望状态、历史、审计和
  持久 outbox。
- **机器可读契约**：Runtime Manifest、OpenAPI、capability metadata、Pack Manifest、MCP
  tools、实验性 CloudLink schema 和 Markdown 文档为 Agent 提供可验证事实。
- **唯一应用边界**：HTTP、CLI、MCP 和生成式客户端共享受治理的 query/command，不直接写
  SHM 或存储。
- **Domain Pack**：行业知识、模型、mapping、规则和处理声明可以叠加在 Kernel 之上，而不
  成为核心依赖。

## 默认无头

AetherEdge 不交付一个固定的通用 Web Console。固定 Dashboard 无法表达所有行业 Pack，浏览器
也不能成为第二套配置权威。AetherEdge 交付的是生成或维护专用应用所需的契约、Agent Skill 和
开发规范。

UI 是下游客户端和参考实现：它只消费公开 application API，可被替换，不能直接访问 SHM、
SQLite 或内部服务。可选的 AetherEMS Console 是这种模式下的一个能源领域实现。

## 体验 SDK

以下组合不需要外部服务，也不会投运任何硬件：

```bash
cargo run -p aether-example-minimal-gateway
cargo run -p aether-example-energy-gateway
```

`aether-edge-sdk`（导入名为 `aether_sdk`）是唯一受支持的 Rust 应用门面。Workspace
实现 crate 仅随源码提供，不能独立发布。下游构建固定到签名源码发行标签对应的精确 commit，
并通过 SDK 的 `local-runtime` feature 选择本地 adapter。

前者是行业中立的空网关；后者验证默认禁用的 Energy Pack 组合。它们是 SDK 冒烟测试，不是
受监管的生产运行时。

## Edge Runtime

| 进程 | 职责 |
|---|---|
| `aether-io` | 协议采集；唯一的遥测/状态写入者 |
| `aether-automation` | Instance、规则与经审计的控制分发 |
| `aether-alarm` | 告警计算与生命周期 |
| `aether-history` | 嵌入式历史与可选历史适配器 |
| `aether-api` | 经认证的远程 application API 与 WebSocket |
| `aether-uplink` | 通过本地持久 outbox 完成 legacy 云/MQTT 交付，并提供实验性 CloudLink 基础 |

```text
设备 -> aether-io -> 权威 SHM
                    |-> 自动化与告警
                    |-> API 与嵌入式历史
                    `-> 持久 outbox -> 可选云端

domain <- ports <- application <- runtime/interfaces
             ^
             `---- extensions
```

只有 `aether-api` 是远程应用边界，其他进程 API 必须保留在 loopback。生成式客户端只能使用
公开 application capability，不能暴露或代理这些内部端口。

## 项目状态

AetherEdge 当前为 beta。版本化 SDK、Pack v1、六服务 Runtime、point/health 一致 SHM epoch、
嵌入式本地运行、受治理命令、MCP 接口和 OpenAPI 契约检查已经可用。签名的 `v0.5.0`
源码、Runtime 与 CLI 发行已经发布；下游 bootstrap pin 替换和剩余无 revision 兼容路径的
清理仍未完成。精确边界见[架构说明](ARCHITECTURE.md)、
[ADR-0007](docs/adr/0007-aether-core-and-ems-distribution.md)与
[ADR-0012](docs/adr/0012-agent-first-application-surface.md)、
[ADR-0013](docs/adr/0013-single-sdk-source-release.md)、
[ADR-0014](docs/adr/0014-coordinated-shm-topology-publication.md)与
[ADR-0015](docs/adr/0015-configuration-authority-and-reconciliation.md)。

Point 与 health 两个 SHM 平面发布同一个已提交物理 epoch，History 与 Uplink 把同一份
SQLite topology 快照绑定到该 epoch。SQLite 是已投运 topology、协议 mapping、逻辑 route、
规则与 instance 的期望状态权威，并通过带 revision 的命令自动协调运行时。本地发布门禁会
拒绝 registry 发布，确认所有 workspace package 仅随源码提供，并签名 Kernel 源码、Runtime、
manifest 与 CLI 产物。AetherEMS 物理拆仓及其下游 bootstrap CI 已落地，但尚未使用签名发行
证据替换 bootstrap Git pin。

Broker-neutral CloudLink MQTT v1 的**边端基础**已经以实验性、显式 opt-in 方式落地：严格
JSON schema/codec、只由 application durable ACK 清理的独立 memory/file spool、用户自选
MQTT 3.1.1 Broker binding、session/heartbeat/manifest/真实 PointSample telemetry 与 replay
测试。Legacy MQTT adapter 仍是兼容默认值。AetherCloud 与 AetherEdge 现在消费同一份通过摘要锁定的
AetherContracts `v0.1.0-alpha.3`：Cloud 与 Edge 使用完全一致的 complete-consumer 锁，`pending_imports` 为空。该证据只证明分发完整性与公开 fixture 执行，不证明生产密钥生命周期、签名 ACK 或 Cloud 崩溃持久性。
这只证明分发完整性，不代表 Rust/TypeScript codec 或边云正式联调已经完成；认证、真实双进程
Broker harness、Cloud batch-position 应用模型和 durable Cloud store 仍未完成。边界见
[ADR-0017](docs/adr/0017-experimental-cloudlink-mqtt-edge-foundation.md)与
[CloudLink 参考](docs/reference/cloudlink-mqtt-v1.md)，公共发行权威见
[ADR-0018](docs/adr/0018-pinned-aethercontracts-consumption.md)。

## 文档

- [Agent Quickstart](https://docs.aetheriot.workers.dev/agent-quickstart/)
- [使用 AI 构建应用](docs/guides/build-applications-with-ai.md)
- [连接 AI 助手](docs/guides/ai-assistants.md)
- [连接设备](docs/guides/connect-devices.md)
- [HTTP API 与 Swagger](docs/reference/http-api.md)
- [部署指南](docs/guides/deployment.md)
- [llms.txt](https://docs.aetheriot.workers.dev/llms.txt) 与
  [llms-full.txt](https://docs.aetheriot.workers.dev/llms-full.txt)

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
