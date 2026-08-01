# AetherEdge

[![代码检查](https://github.com/EvanL1/AetherEdge/actions/workflows/rust-check.yml/badge.svg)](https://github.com/EvanL1/AetherEdge/actions/workflows/rust-check.yml)
[![许可证](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#许可证)
[![Rust](https://img.shields.io/badge/rust-1.90%2B-orange.svg)](https://www.rust-lang.org/)
[![版本](https://img.shields.io/badge/version-0.0.1-yellow.svg)](https://github.com/EvanL1/AetherEdge/releases)
[![状态](https://img.shields.io/badge/status-beta-orange.svg)](https://github.com/EvanL1/AetherEdge/releases)

**产品站：** [aetheriot.ai](https://aetheriot.ai/) ·
**开发者站：** [aetheriot.dev](https://aetheriot.dev/)

**文档：** [docs.aetheriot.ai](https://docs.aetheriot.ai/) ·
[快速开始](docs/guides/getting-started.md) ·
[用户旅程](docs/overview/user-journeys.md) ·
[连接设备](docs/guides/connect-devices.md) ·
[连接 AI](docs/guides/ai-assistants.md) · [English](README.md)

**连接物理设备、证明数据链路、投运确定性行为——而不让云、浏览器或
AI 模型成为控制回路的一部分。**

AetherEdge 是面向 Linux 网关的开源、行业中立 IoT Edge Kernel、六服务
Runtime、CLI 与 Rust SDK。SHM 是实时点状态权威；嵌入式 SQLite 保存期望状态、
历史、审计和持久 outbox。默认发行版不需要 Redis、PostgreSQL、云服务、浏览器
或 LLM。

AI 是与其他客户端一样、位于类型化受治理 application boundary 后面的可替换
客户端。设备控制默认拒绝，必须明确确认并完整审计。即使断开所有外部客户端，
已经投运的采集、安全、规则和告警仍在 Edge 本地确定性运行。

## AetherEdge 是正确入口吗？

| 你的目标 | 从这里开始 |
|---|---|
| 在 Linux 网关连接现场设备并运行本地行为 | **AetherEdge** |
| 部署能源管理解决方案和操作员 Console | [**AetherEMS**](https://github.com/EvanL1/AetherEMS) |
| 协调 Edge Fleet 或云端任务 | [**AetherCloud**](https://github.com/EvanL1/AetherCloud) |
| 实现或验证共享协议 | [**AetherContracts**](https://github.com/EvanL1/AetherContracts) |

AetherEdge 的直接用户是设备厂商、系统集成商、解决方案开发者、应用开发者和
Edge 运维人员。它不会假装自己是适用于所有行业的完整终端应用。

## 从空白主机到可用 Edge

产品旅程是：

```text
安全空安装 -> 操作员身份 -> 默认禁用的设备 Channel
  -> 物理点/逻辑点映射 -> 只读数据证明
  -> 审核行为 -> 显式投运 -> 审计与持续运维
```

所有重要变更遵循：

```text
检查 -> 计划 -> 验证 -> 确认 -> 应用 -> 审计 -> 观察 -> 修订
```

创建配置绝不能静默启用硬件。

### 1. 安装安全空 Runtime

从 [GitHub Releases](https://github.com/EvanL1/AetherEdge/releases) 下载目标
Linux 主机对应的 `.run` 安装包及校验文件，然后校验并执行仅支持全新部署的安装包：

```bash
sha256sum -c AetherEdge-<arch>-<version>.run.sha256
chmod +x AetherEdge-<arch>-<version>.run
sudo ./AetherEdge-<arch>-<version>.run
```

安装器创建六个服务、`aether` CLI、私有 bootstrap 凭据、嵌入式数据库和空配置。
它不会添加设备、启用规则或安装行业解决方案。

### 2. 建立身份并证明空 Runtime

先执行本地健康门禁：

```bash
aether doctor
```

健康的首次启动应当有六个健康服务和有效 SHM。使用私有 bootstrap 凭据登录后
立即修改密码，为日常运维创建独立账号，并导出该账号的签名
`AETHER_ACCESS_TOKEN`。然后证明系统没有隐式投运任何对象：

```bash
aether channels list --json
aether models instances list --json
aether rules list --json
```

Channel、Instance 和 Rule 集合都应该为空。[快速开始](docs/guides/getting-started.md)
包含完整的 bootstrap 和 Token 流程。

### 3. 创建一个仍然禁用的 Channel

选择安装包中的 IO Runtime 已编译支持的协议。受治理的创建命令需要认证和确认，
但只要没有显式传入 `--enabled true`，新 Channel 就保持禁用：

```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' aether channels create \
  --name "PLC#1" \
  --protocol modbus_tcp \
  --params '{"host":"192.168.1.10","port":502}' \
  --confirmed
```

启用前，需要声明物理点、映射协议地址、将必要的物理点绑定到 Domain Pack
提供的逻辑 Instance，并检查未解决的映射。完整流程见
[连接设备](docs/guides/connect-devices.md)。

### 4. 先证明观测，再添加控制

```text
设备 -> aether-io -> 权威 SHM -> API 与嵌入式历史 -> 客户端
```

检查 Channel 健康状态、时间戳、质量、新鲜度、拓扑 generation、历史样本和
未映射点。Socket 已连接但没有新鲜数据不代表采集健康；缺失值也不等于零。

映射完成后，使用最新 Channel 查询返回的 desired-state revision 显式启用：

```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' aether channels enable <CHANNEL_ID> \
  --expected-revision <REVISION> \
  --confirmed
```

第一个有用里程碑是一条只读数据链路。不要为了证明采集而添加物理控制命令。

### 5. 添加并投运确定性行为

通过下游 Domain Pack 或 application composition 添加逻辑模型、计算、告警和
本地规则。规则草案和控制路径保持禁用，直到输入、目标、权限、失败行为和审计
链路都完成审核。

```text
审核禁用行为 -> 验证 -> 确认 -> 启用
  -> 检查审计证据 -> 观察物理结果
```

命令被系统接受不代表物理设备已经达到目标状态，必须独立观察实际结果。

### 6. 选择可替换客户端

所有客户端都通过经认证的 `aether-api:6005` 进入：

| 客户端 | 用途 |
|---|---|
| `aether` CLI | 安装、投运、诊断和运维 |
| HTTP/OpenAPI | 专用应用和生成式客户端 |
| 只读 MCP | AI 辅助检查和解释 |
| 临时写入 MCP | 一次边界明确、显式授权的维护任务 |
| `aether-edge-sdk` | 下游解决方案或嵌入式组合 |
| 下游 Console | AetherEMS 等行业专用操作体验 |

其他五个进程 API 保留在 loopback。客户端不能代理这些端口，也不能直接写 SHM
或 SQLite。AetherEdge 不提供通用 Web Console；UI 是可替换的 application client，
不能成为第二个状态权威。

将已有 Runtime 以默认只读模式接入 Claude：

```bash
claude mcp add aether -- aether mcp
```

为会话设置 `AETHER_ACCESS_TOKEN`。远程 Edge 应使用 SSH stdio 或 HTTPS 入口，
不能暴露内部服务端口。详见[连接 AI 助手](docs/guides/ai-assistants.md)。

## 没有现场硬件时开发

运行行业中立的 SDK 组合或协议验证模拟器；二者都不会投运物理设备：

```bash
cargo run -p aether-example-minimal-gateway
cargo run -p simulator -- \
  --scenario tools/simulator/scenarios/modbus_protocol_verification.yaml \
  --port 5020
```

源码检出是开发者路径，不是普通操作员的安装流程。详见
[快速开始](docs/guides/getting-started.md)。

## 构建下游解决方案

```bash
cargo add aether-edge-sdk --features local-runtime
```

`aether-edge-sdk` 的导入名是 `aether_sdk`，它是受支持的 Rust application facade。
下游产品在自己的仓库组合 SDK、Domain Pack 和专用应用或 Agent。领域 Processor、
模型和 Console 不会成为 AetherEdge Kernel 的依赖。AetherEMS 是这一模式的能源领域
参考实现。

## Runtime 模型

| 进程 | 职责 |
|---|---|
| `aether-io` | 协议采集；唯一的遥测/状态写入者 |
| `aether-automation` | Instance、规则与经审计的控制分发 |
| `aether-alarm` | 告警计算与生命周期 |
| `aether-history` | 嵌入式历史与可选历史适配器 |
| `aether-api` | 经认证的远程 application API 与 WebSocket |
| `aether-uplink` | 持久 legacy Cloud/MQTT 交付和实验性 CloudLink 基础 |

```text
设备 -> aether-io -> 权威 SHM
                    |-> 自动化与告警
                    |-> API 与嵌入式历史
                    `-> 持久 outbox -> 可选云端

         domain <- ports <- application <- runtime/interfaces
                   ^
                   `---- 下游静态 Rust 适配器（仓外）
```

AetherEdge 当前交付面向集成商的 Runtime、application contracts、受治理命令、
MCP 基础、Pack v1 和 SDK facade。完整的对话式意图编译、仿真、临时行为和持续
效果评估仍是产品方向。准确交付边界见[平台状态](docs/roadmap/status.md)。

## 参与开发

开发环境与验证流程见 [CONTRIBUTING.md](CONTRIBUTING.md)。面向 Agent 与贡献者的
仓库规则见 [AGENTS.md](AGENTS.md)。

## 许可证

可任选 [MIT](LICENSE-MIT) 或 [Apache-2.0](LICENSE-APACHE) 许可证。
