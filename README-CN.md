# AetherIot

[![代码检查](https://github.com/EvanL1/AetherIot/actions/workflows/rust-check.yml/badge.svg)](https://github.com/EvanL1/AetherIot/actions/workflows/rust-check.yml)
[![许可证](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.90%2B-orange.svg)](https://www.rust-lang.org/)
[![版本](https://img.shields.io/badge/version-0.5.0-yellow.svg)](CHANGELOG.md)
[![状态](https://img.shields.io/badge/status-beta-orange.svg)](CHANGELOG.md)

[快速开始](docs/guides/getting-started.md) · [在线文档](https://docs.aetheriot.workers.dev/) · [Agent Skill](skills/aether-iot/SKILL.md) · [MCP](docs/guides/ai-assistants.md) · [English](README.md)

**用 AI 构建可靠的边缘 IoT 应用。**

AetherIot 是开源、行业中立的 Linux 网关 IoT Edge Kernel、Runtime 与 Rust SDK。它连接
现场设备，以共享内存保存权威实时状态，在本地确定性地执行规则与告警，并保存嵌入式历史；
默认运行不依赖 Redis、PostgreSQL、云服务、浏览器或 LLM。

AI 是 AetherIot 的客户端，不进入硬实时闭环。Agent、CLI、生成式应用和操作界面都通过同一
套类型化 command/query 边界访问系统；设备控制始终默认拒绝，并要求明确确认和完整审计。

> **Beta：** AetherIot 是行业中立的 Kernel、Runtime 与 SDK。现有 crate、二进制、CLI 和
> 部分兼容产物仍使用 `aether-*` / `aether` 名称。官方能源管理实现位于独立的
> [AetherEMS](https://github.com/EvanL1/AetherEMS) 仓库。

## 从 AI Agent 开始

在支持 Agent Skills 的编码助手中安装本仓库的 Skill：

```bash
npx skills add EvanL1/AetherIot -s aether-iot
```

构建 CLI，并把正在运行的 Edge 系统作为默认只读的 MCP tools 接入：

```bash
cargo build --release -p aether
claude mcp add aether -- ./target/release/aether mcp
```

然后直接告诉助手：

```text
从 AetherIot 开始。检查这个仓库，并根据当前 Edge Runtime 暴露的能力，
生成一个只读运维应用。
```

Skill 提供开发方法，并按需读取在线 Markdown 文档；MCP 提供实时、结构化的系统能力。只有
操作员显式启动写入会话时，写工具才会注册；每次写入仍必须通过服务端权限、确认、校验和
审计边界。

完整客户端契约见[使用 AI 构建应用](docs/guides/build-applications-with-ai.md)，安全空运行时的
完整安装流程见[Agent Quickstart](https://docs.aetheriot.workers.dev/agent-quickstart/)。

## AetherIot 提供什么

- **确定性 Edge Runtime**：没有 AI 客户端时，六个隔离 Rust 服务仍持续采集、执行规则、
  处理告警、保存历史并完成 uplink。
- **Local-first 数据平面**：SHM 是实时点状态权威；SQLite 提供嵌入式期望状态、历史、审计和
  持久 outbox。
- **机器可读契约**：Runtime Manifest、OpenAPI、capability metadata、Pack Manifest、MCP
  tools 和 Markdown 文档为 Agent 提供可验证事实。
- **唯一应用边界**：HTTP、CLI、MCP 和生成式客户端共享受治理的 query/command，不直接写
  SHM 或存储。
- **Domain Pack**：行业知识、模型、mapping、规则和处理声明可以叠加在 Kernel 之上，而不
  成为核心依赖。

## 默认无头

AetherIot 不交付一个固定的通用 Web Console。固定 Dashboard 无法表达所有行业 Pack，浏览器
也不能成为第二套配置权威。AetherIot 交付的是生成或维护专用应用所需的契约、Agent Skill 和
开发规范。

UI 是下游客户端和参考实现：它只消费公开 application API，可被替换，不能直接访问 SHM、
SQLite 或内部服务。可选的 AetherEMS Console 是这种模式下的一个能源领域实现。

## 体验 SDK

以下组合不需要外部服务，也不会投运任何硬件：

```bash
cargo run -p aether-example-minimal-gateway
cargo run -p aether-example-energy-gateway
```

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
| `aether-uplink` | 通过本地持久 outbox 向云端/MQTT 交付数据 |

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

AetherIot 当前为 beta。版本化 SDK、Pack v1、六服务 Runtime、point/health 一致 SHM epoch、
嵌入式本地运行、受治理命令、MCP 接口和 OpenAPI 契约检查已经可用。首次独立签名公开发行和
剩余无 revision 兼容路径的清理仍未完成。精确边界见[架构说明](ARCHITECTURE.md)、
[ADR-0007](docs/adr/0007-aether-core-and-ems-distribution.md)与
[ADR-0012](docs/adr/0012-agent-first-application-surface.md)。

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
