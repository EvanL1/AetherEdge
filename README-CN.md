# Aether

[![代码检查](https://github.com/EvanL1/Aether/actions/workflows/rust-check.yml/badge.svg)](https://github.com/EvanL1/Aether/actions/workflows/rust-check.yml)
[![许可证](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.90%2B-orange.svg)](https://www.rust-lang.org/)
[![版本](https://img.shields.io/badge/version-0.5.0-yellow.svg)](CHANGELOG.md)
[![状态](https://img.shields.io/badge/status-beta-orange.svg)](CHANGELOG.md)

[English](README.md) | [文档](https://docs.aether-edge.workers.dev/) | [变更记录](CHANGELOG.md) | [llms.txt](https://docs.aether-edge.workers.dev/llms.txt)

**一套 AI-native Edge Runtime、Kernel 与 SDK——它源于一个判断：IoT 工作天然适合
Agent。**

Aether 将工业设备、实时点状态、历史、告警、规则和控制动作转换为带类型的能力，
让 AI Agent 与操作者通过同一套 command/query 应用 API 发现和使用这些能力。

它面向 Linux IoT 网关，可完全离线运行；即使没有任何 AI 客户端连接，采集与安全
行为仍保持确定性。默认运行时不需要 LLM、云连接、Redis、PostgreSQL 或浏览器。

## 为什么会有 Aether

IoT 工程中有大量工作特别适合 Agent：阅读协议和设备文档、发现能力、映射点位、结合
实时上下文理解现场状态、诊断故障、生成规则、检查配置，以及通过受约束的动作操作
工具。这些工作上下文密集、持续迭代，而且天然以工具为中心。

传统网关却让这类工作变得困难。知识散落在仪表盘、数据库、脚本、厂商 API 和只面向
人类编写的文档中。在这样的系统旁边外挂一个聊天机器人，并不会改变底层边界。

Aether 建立在另一个前提上：让 Edge 系统本身能够被 Agent 理解和操作。能力、状态、
策略、文档与验证都以带类型、机器可读的契约和代码放在一起。确定性运行时仍负责采集
与安全；Agent 只能通过受控的应用边界工作，不会进入实时闭环。

**AetherEMS 是这套理念的第一个官方行业参考案例。** 它将行业中立的 Aether Kernel
与可选的 Energy Pack 组合起来，展示 Agent-native IoT 基础如何成为能源网关，同时
不把核心限制成一个只服务 EMS 的产品。

## 从一个设备开始，无需换底座地持续生长

Aether 提供一条渐进式采用路径。团队可以从一台 Linux 主机、一个协议开始，逐步加入
上下文、自动化、AI 接入、安全控制与行业知识，而不需要更换实时状态权威来源，也不
需要改变应用边界。

| 阶段 | 获得的能力 | Aether 构件 |
|---|---|---|
| **连接** | 可靠采集一个设备 | 协议适配器 + `aether-io` + 权威 SHM |
| **理解** | 让原始点位拥有稳定语义与历史 | 实例模型 + 告警 + 嵌入式历史 |
| **自动化** | 离线运行确定性行为 | 本地规则 + `aether-automation` |
| **辅助** | 让 Agent 检查状态并获取运行知识 | 只读 MCP tools/resources + `llms.txt` |
| **行动** | 允许有边界的物理设备动作 | 类型化命令 + 认证 + 确认 + 审计 |
| **扩展** | 面向其他行业或基础设施构建产品 | Domain packs + ports + 可选 adapters |

每个阶段不依赖下一阶段也能独立产生价值。AI 永远不会成为采集、本地自动化或安全
行为的运行依赖。

因此，Aether 适合构建自有 Edge 产品的团队、连接存量设备的系统集成商、需要安全
物理世界工具面的 Agent 开发者，以及希望沉淀可复用行业知识的领域团队。

## 在本地验证这套基础

两个示例都处于未投运状态，不会连接现场设备，也不需要外部服务：

```bash
# 行业中立 Aether SDK composition
cargo run -p aether-example-minimal-gateway

# Aether + 可选 energy domain pack
cargo run -p aether-example-energy-gateway
```

预期输出：

```text
Aether minimal gateway ready: 2 capabilities, no external services
AetherEMS ready: pack=energy, capabilities=7, example_channels=8, commissioned=0
```

第一个组合证明公共 SDK 不依赖能源领域；第二个组合证明 AetherEMS 由同一内核加入
Energy Manifest、示例模型与 fail-closed 投运策略构成。

## Aether 所说的 AI-native 是什么

AI-native 是运行时契约，不是在传统网关旁边外挂一个聊天机器人：

- **能力可发现。** MCP tools、MCP resources、`llms.txt` 与仓库自带的 AI catalog
  共同描述边缘节点能做什么。
- **核心操作有类型。** Query 和 command 都有明确输入、权限、风险、幂等性、确认与
  审计策略。
- **设备控制共享同一个应用边界。** 来自 AI、CLI 和 HTTP 的外部动作不能通过直接
  写 SHM、数据库或协议驱动绕过策略。
- **控制默认拒绝。** 真实设备动作必须具备经过认证的权限、明确确认、参数校验与
  持久审计记录。
- **实时面不依赖 AI。** 即使 Agent、模型、网络或云消失，协议采集、本地规则、
  重连和安全行为仍确定运行。
- **行业知识可组合。** Domain pack 可以教会 Agent 和运行时理解一个行业，而不向
  内核加入行业依赖。

这些契约与代码一起版本化：

| 契约 | 用途 |
|---|---|
| [`llms.txt`](llms.txt) | AI 可读文档索引 |
| [`ai/catalog.yaml`](ai/catalog.yaml) | 机器可读组件与验证目录 |
| [`ai/invariants.md`](ai/invariants.md) | 不可违反的运行时与安全不变量 |
| [`ai/safety-policy.yaml`](ai/safety-policy.yaml) | 能力风险、权限、确认和审计策略 |
| [`AGENTS.md`](AGENTS.md) | 仓库内编码 Agent 的规范来源 |
| [`ai/evals`](ai/evals) | Agent 行为与架构一致性评测入口 |

## 从 Agent 到设备的信任链

```text
AI Agent 或操作者
        │
        ▼
MCP / CLI / 经过认证的 HTTP
        │
        ▼
类型化能力 + RequestContext
        │
        ├── query 或 command
        ├── 风险等级
        ├── 所需权限
        ├── 确认策略
        ├── 幂等契约
        └── 强制审计策略
        │
        ▼
应用 command/query API
        │
        ├── 拒绝 ──► 持久审计
        │
        └── 允许 ─► 路由与安全校验 ─► SHM/UDS ─► 设备驱动
```

例如，机器可读策略将真实设备控制声明为高风险 command：

```yaml
device.write_point:
  kind: command
  risk: high
  permission: device.control
  idempotent: false
  confirmation: always
  audit: required
```

对于外部设备动作，这些元数据由应用边界真实执行，而不是贴在未检查驱动调用旁边的
说明文字。

## 连接 AI 客户端

在已经安装并运行 Aether Edge Runtime 的机器上，构建 CLI/MCP 适配器：

```bash
cargo build --release -p aether
./target/release/aether mcp
```

默认 MCP 能力面只读。兼容 MCP 的桌面 Agent 可以通过 stdio 启动它：

```json
{
  "mcpServers": {
    "aether": {
      "command": "/absolute/path/to/aether",
      "args": ["mcp"]
    }
  }
}
```

只读工具可以检查通道、实例、SHM 实时值、告警、历史、规则和运行状态。运行知识也会
作为 MCP resources 暴露，让 Agent 获取仓库自带的操作指导，而不是猜测设备语义。

只有操作者显式启动 `aether mcp --allow-write` 时，写工具才会出现。真实设备动作还
必须提供来自已认证 Admin 或 Engineer 会话的 `AETHER_ACCESS_TOKEN`：

```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' aether mcp --allow-write
```

不要把 access token 明文写入会提交到仓库的 MCP 配置，应使用客户端 secret store
或进程环境。启用写操作前，请阅读 [AI 安全操作守则](docs/domain/safe-operations.md)
和 [MCP 工具参考](docs/reference/mcp-tools.md)。

## 面向物理系统的安全属性

- SHM 是当前点状态的权威来源，并具有明确的 writer ownership。
- 只有采集面拥有 Telemetry/Signal 实时写权限；应用接口只能读取实时状态。
- 外部 C/A 设备命令必须进入 automation 的认证、确认和审计应用用例。
- 伪造 actor 或 role 请求头不能获得设备控制权限。
- Uplink 命令使用单独生成的 service credential，并映射为服务端固定身份。
- T/S 仿真写入默认关闭，因为伪造测量值可能触发真实自动化规则。
- AI 永远不会进入协议轮询或硬实时安全循环。
- 外部服务或 AI 故障不能停止本地采集与安全行为。

详细信任边界记录在
[ADR-0008](docs/adr/0008-application-control-boundary.md)。

## Edge Runtime

Aether 负责设备采集、亚毫秒共享内存实时状态、本地自动化与告警、嵌入式历史，并通过
可崩溃恢复的本地 outbox 向云端交付数据。

生产环境明确保留六个独立监督的 Rust 进程：

| 进程 | 职责 |
|---|---|
| `aether-io` | 协议采集，Telemetry/Signal 唯一写入者 |
| `aether-automation` | 实例、规则、Control/Action 下发 |
| `aether-alarm` | 告警计算与生命周期 |
| `aether-history` | 嵌入式历史与可选历史适配器 |
| `aether-api` | 带鉴权的管理 API 和 WebSocket |
| `aether-uplink` | MQTT/云交付与本地持久 outbox |

阻塞驱动、云连接失败或外围进程崩溃都不能拖垮采集与其他服务。只有 `aether-api`
用于远程访问；发行版中的内部服务 API 绑定回环地址。

```text
设备 ─► aether-io ─► 权威 SHM
          │             ├─ 事件提示 ─► aether-automation
          │             ├─ 事件提示 ─► aether-alarm
          │             ├─ 事件提示 ─► aether-api
          │             ├─ 对账读取 ─► aether-history ─► SQLite
          │             └─ 对账读取 ─► aether-uplink ─► FileOutbox ─► 云
          │
          └─ 可选适配器 ─► Redis 镜像 / PostgreSQL 历史
```

## Kernel 与 SDK 契约

依赖方向保持单向：

```text
domain <- ports <- application <- runtime/interfaces
             ^
             +---- extensions
```

| 层 | 职责 |
|---|---|
| `aether-domain` | 行业中立的点、身份、质量和命令类型 |
| `aether-dataplane` | 无数据库 SHM 布局、原子点槽、mmap I/O 和快照 |
| `aether-ports` | `LiveState`、`HistorySink`、`StateMirror` 等能力接口 |
| `aether-application` | 命令、查询、权限、确认和审计 |
| `aether-edge-sdk`（`aether_sdk`） | 稳定 builder 与公共 facade |
| `extensions/*` | Local、SHM、Redis、PostgreSQL 和平台适配器 |
| `packs/*` | 声明式行业知识；energy 是第一个官方 pack |

默认 Cargo members 与 Edge composition 不要求 Redis 或 PostgreSQL。嵌入式 SQLite
和本地持久 outbox 覆盖独立运行需求；外部存储只是可选 port 实现，永远不是实时状态
权威来源。

## 协议与扩展

标准边缘构建包含 Modbus TCP/RTU、IEC 61850 MMS、CAN、GPIO、Aether-485 和
Virtual。可选 Cargo features 提供 IEC 104、OPC UA、MQTT、HTTP、DL/T 645、
J1939、BLE、Zigbee 和 Matter。

“支持协议”表示适配器可编译并通过 conformance tests。真实部署仍必须针对目标硬件
验证映射、超时、命令边界、重连语义和硬件行为。

```text
extensions/store-local       默认嵌入式状态、审计和 outbox
extensions/redis-bridge      可选、非权威实时状态镜像
extensions/postgres-history  可选外部历史存储
```

## Domain packs 与 AetherEMS

Aether 保持行业中立。Domain pack 可以提供模型、映射、规则、Agent 指导、能力策略
引用和默认禁用的投运示例，而不修改内核。

[`packs/energy`](packs/energy) 是第一个官方 pack，它构成 AetherEMS 能源发行版；
内核本身仍可用于工业自动化、楼宇、农业、环境监测和其他 IoT 行业。

## 安装与部署

从源码构建 CLI，并生成可审阅的 setup 计划：

```bash
cargo build --release -p aether
./target/release/aether --json setup
```

应用计划只初始化空站点，默认绝不启用硬件。独立 CLI 安装器只安装 CLI；Edge `.run`
包安装完整六进程运行时，并生成不会打印到终端的本地控制凭据。

- [快速开始](docs/guides/getting-started.md)
- [连接 AI 助手](docs/guides/ai-assistants.md)
- [部署指南](docs/guides/deployment.md)
- [CLI 参考](docs/reference/cli.md)
- [配置参考](docs/reference/configuration.md)

浏览器应用只是可选客户端；SDK、运行时、MCP 服务和两个 composition example 都不
依赖它。

## 仓库结构

```text
crates/       稳定、行业中立的内核与 SDK
extensions/   可选存储、SHM 和平台适配器
services/     六个生产进程隔离边界
tools/        Aether CLI/MCP launcher 与模拟器
examples/     可运行的 Aether 与 AetherEMS 组合
packs/        声明式行业知识
ai/           机器可读不变量、能力策略、runbook 和 eval
docs/         架构、概念、指南与参考
libs/         共享运行时与配置基础库
apps/         可选旧浏览器客户端
```

## 开发验证

```bash
cargo test
cargo test -p aether-example-minimal-gateway --test composition_contract
cargo test -p aether-example-energy-gateway --test composition_contract
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
./scripts/check-architecture.sh
```

需要外部服务的测试不属于默认路径。

## 当前状态

0.5 版本为 beta。行业中立 Kernel 边界、SHM 权威、本地 outbox、标准服务身份、
fail-safe 安装和外部设备动作认证链均由 CI 强制检查。

默认 AI 能力面只读。只有操作者明确决定使用 `--allow-write` 后，运维写工具才会出现；
每项能力的权限与安全要求记录在 MCP 参考中。真实设备动作必须经过身份认证、确认、
参数校验与持久审计。

精确边界参见 [ARCHITECTURE.md](ARCHITECTURE.md)、[ADR 索引](docs/adr) 和
[变更记录](CHANGELOG.md)。

## 许可证

可任选 MIT 或 Apache-2.0。详见 [LICENSE](LICENSE)。
