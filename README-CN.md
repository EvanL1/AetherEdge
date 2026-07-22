# AetherEdge

[![代码检查](https://github.com/EvanL1/AetherEdge/actions/workflows/rust-check.yml/badge.svg)](https://github.com/EvanL1/AetherEdge/actions/workflows/rust-check.yml)
[![许可证](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#许可证)
[![Rust](https://img.shields.io/badge/rust-1.90%2B-orange.svg)](https://www.rust-lang.org/)
[![版本](https://img.shields.io/badge/version-0.5.0-yellow.svg)](CHANGELOG.md)
[![状态](https://img.shields.io/badge/status-beta-orange.svg)](CHANGELOG.md)

**文档网站：** [docs.aetheriot.workers.dev](https://docs.aetheriot.workers.dev/)

[AI 原生平台](https://docs.aetheriot.workers.dev/overview/ai-native-platform/) · [快速开始](docs/guides/getting-started.md) · [在线文档](https://docs.aetheriot.workers.dev/) · [Agent Skill](skills/aether-iot/SKILL.md) · [MCP](docs/guides/ai-assistants.md) · [English](README.md)

**让 AI 智能体安全、本地、确定性地运行你的物理空间。**

AetherEdge 是开源、行业中立的 Linux 网关 IoT Edge Kernel、Runtime 与 Rust SDK。它连接
现场设备，以共享内存保存权威实时状态，在本地确定性地执行规则与告警，并保存嵌入式历史——
不需要 Redis、不需要 PostgreSQL、不需要云服务、不需要浏览器、不需要 LLM。

它的不同之处在于：AI 是位于类型化受治理边界之后的一等*客户端*。智能体通过 MCP 和 OpenAPI
发现真实能力、提出变更、完成行为投运——而设备控制始终默认拒绝、要求明确确认并完整审计；
即使没有任何 AI 接入，边缘也持续确定性地执行。

AetherEdge 是 [AetherIoT 平台](docs/overview/platform.md)的边缘产品，与
[AetherCloud](https://github.com/EvanL1/AetherCloud) 和
[AetherContracts](https://github.com/EvanL1/AetherContracts)共同组成核心产品族。官方能源管理
发行版是 [AetherEMS](https://github.com/EvanL1/AetherEMS)。

## 五分钟体验

**已有运行中的 Edge 系统？** 把它作为默认只读的 MCP tools 接入你的 AI 助手：

```bash
claude mcp add aether -- aether mcp
```

然后直接告诉助手：

```text
检查我的 Edge Runtime，并根据它暴露的能力生成一个只读运维应用。
```

**没有硬件？** SDK 组合可在任何环境运行，不需要外部服务，也不会投运任何硬件：

```bash
cargo run -p aether-example-minimal-gateway   # 行业中立的空网关
cargo run -p aether-example-energy-gateway    # 默认禁用的 Energy Pack 组合验证
```

还可以在总线上仿真真实设备——Modbus TCP/RTU、CAN、J1939——像真实站点一样采集：

```bash
cargo run -p simulator -- --scenario tools/simulator/scenarios/pv_daily.yaml --port 5020
```

完整的安全空运行时安装见[快速开始](docs/guides/getting-started.md)，通道接线见
[连接设备](docs/guides/connect-devices.md)，助手驱动的完整安装流程见
[Agent Quickstart](https://docs.aetheriot.workers.dev/agent-quickstart/)。

## 安装 AetherEdge

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
[部署指南](docs/guides/deployment.md)。AetherEdge 不是 npm 或 Bun 包；`npx` 和 `bunx`
都不能安装它。

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

## AI 原生产品方向

AetherIoT 的产品方向是让用户通过对话描述想要的结果，不必在固定配置页面中编排设备编号、
触发器、条件和动作。智能体发现类型化能力、生成受治理方案并完成行为投运；AetherEdge 在本地
执行已经接受的行为，执行过程不依赖模型。

完整的最终用户对话智能体尚未作为当前测试版交付。AetherEdge 已经提供它所需要的基础：
运行时与 Pack 发现、面向智能体的文档、OpenAPI、MCP 工具与资源、受治理命令、带版本配置、
审计证据和确定性本地执行。

```text
人的意图 -> 智能体方案 -> 类型化契约 -> 策略检查与必要确认
         -> 行为投运 -> 边缘确定性执行
         -> 观察、解释与受治理修订
```

未来的意图、方案、仿真、有效期和持续调整能力必须通过明确的应用接口与 AetherContracts 契约
提供。智能体不能编造设备能力、直接写入 SHM 或 SQLite、绕过确认，或者成为隐藏的第二配置
权威。具体边界见 [AI 原生平台](https://docs.aetheriot.workers.dev/overview/ai-native-platform/)和
[平台状态](https://docs.aetheriot.workers.dev/roadmap/status/)。

> **Beta：** AetherEdge 是行业中立的 Kernel、Runtime 与 SDK。现有 crate、二进制、CLI 和
> 部分兼容产物仍使用 `aether-*` / `aether` 名称。本仓库原名 `EvanL1/AetherIot`；迁移期间
> 软件标识保持稳定，详见[迁移说明](docs/migration/aetheriot-to-aetheredge.md)。

## 智能体访问如何受治理

仓库里的 Agent Skill 只是可选的开发指导，不是 AetherEdge 软件包。需要时按
[使用 AI 构建应用](docs/guides/build-applications-with-ai.md)添加到兼容的编码助手。

可选 Skill 提供开发方法，并按需读取在线 Markdown 文档；MCP 提供实时、结构化的系统能力。
只有操作员显式启动写入会话时，写工具才会注册；每次写入仍必须通过服务端权限、确认、校验
和审计边界。

完整客户端契约见[使用 AI 构建应用](docs/guides/build-applications-with-ai.md)，安全空运行时的
完整安装流程见[Agent Quickstart](https://docs.aetheriot.workers.dev/agent-quickstart/)。

## 对话优先，默认无头

AetherEdge 不交付一个固定的通用 Web Console。固定 Dashboard 无法表达所有行业 Pack，浏览器
也不能成为第二套配置权威。AetherEdge 交付的是生成或维护专用应用所需的契约、Agent Skill 和
开发规范。

长期配置体验以对话为主：用户描述期望结果，智能体生成可检查、带版本的变更。重要变更仍可以
按需生成摘要、风险说明、仿真或确认内容；这些内容只负责解释变更，不拥有配置权威。

UI 是下游客户端和参考实现：它只消费公开 application API，可被替换，不能直接访问 SHM、
SQLite 或内部服务。可选的 AetherEMS Console 是这种模式下的一个能源领域实现。

## Rust SDK

`aether-edge-sdk`（导入名为 `aether_sdk`）是唯一受支持的 Rust 应用门面。Workspace
实现 crate 仅随源码提供，不能独立发布。下游构建固定到签名源码发行标签对应的精确 commit，
并通过 SDK 的 `local-runtime` feature 选择本地 adapter。上文的示例组合是 SDK 冒烟测试，
不是受监管的生产运行时。

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

## 参与开发

开发环境与验证流程见 [CONTRIBUTING.md](CONTRIBUTING.md)。面向智能体与贡献者的仓库规则见 [AGENTS.md](AGENTS.md)。

## 许可证

可任选 MIT 或 Apache-2.0。详见 [MIT 许可证](LICENSE-MIT)和
[Apache 2.0 许可证](LICENSE-APACHE)。
