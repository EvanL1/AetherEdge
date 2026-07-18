---
title: "AetherContracts 产品概览"
description: "AetherContracts 是 AetherCloud、AetherEdge 与独立实现共同使用的语言中立互操作权威。"
updated: 2026-07-17
status: experimental
version: 0.1.0-alpha.4
---

# AetherContracts 产品概览

> 本页是面向中文用户的说明。带标签版本中的英文规范、JSON Schema、测试夹具和 TCK 才是规范性依据。

AetherContracts 是 AetherCloud、AetherEdge 与独立实现共同使用的公开、语言中立互操作权威。英文规范定义语义，JSON Schema Draft 2020-12 定义结构，测试夹具固定示例，TCK 提供可执行证据。任何语言绑定都不能成为第二套事实来源。

最新发布版本是 `v0.1.0-alpha.3`。当前源码面向尚未发布的 `0.1.0-alpha.4` 开发目标；它仍处于实验阶段，不能作为生产 CloudLink 切换版本。

## 当前状态

| 能力                                   | 状态                     | 证据                                                                 |
| -------------------------------------- | ------------------------ | -------------------------------------------------------------------- |
| Thing Model v1 alpha 结构              | 已实现，实验性           | 结构定义、Voltage 迁移黄金夹具、TCK                                    |
| P/M/A 迁移词汇                         | 已实现，实验性           | `P -> properties`、`M -> points`、`A -> capabilities`                |
| Integration v1 alpha 拓扑与观测        | 已实现，实验性契约       | 封闭结构定义、Home Assistant 黄金夹具与配置档、上下文 TCK             |
| 通过 CloudLink 传输 Integration        | 已实现，实验性扩展       | 原样载荷封装、显式启用、流、代次、批次、重放和确认测试               |
| 受治理的 Integration Control           | 已实现，实验性且默认关闭 | 单一封闭电源设定动作、精确拓扑目标、确认、重放、回执与安全测试       |
| CloudLink alpha.4 开发目标、配置档和 TCK | 候选已冻结，尚未发布     | AetherContracts 是唯一权威；产品文件只是非规范实现叠加层             |
| 测试夹具与发布哈希检查                 | 已实现                   | `pnpm test:tck`                                                      |
| 摘要锁定的使用方分发                   | 已实现，实验性           | 封闭且锁定的结构定义、离线验证器、精确发布版本的复合操作                |
| 既有 CloudLink 线结构拒绝              | 已实现                   | JSON Schema TCK，以及四种语言绑定中的稳定公开夹具结果                |
| 最小上下文归并器                       | 已实现，实验性           | 重放、会话、摘要、数据丢失与游标场景；不是生产状态机                 |
| TypeScript、Rust、C、C++ 测试夹具绑定  | 已实现，实验性           | 每种绑定都执行相同的公开夹具清单；不声明完整生产编解码器一致性       |
| 共享消息代理认证交互                   | 提案                     | 两种来源模型与精确签名对象；生产密钥生命周期和验证方归属仍待实现     |
| 未签名的累计应用持久确认               | 已冻结的实验契约         | 连续前缀与已声明丢失的 TCK；不声明生产级崩溃持久性，签名确认仍在规划 |
| 使用方的真实消息代理测试与故障矩阵     | 使用方证据               | 不是发布一致性或生产持久性证据；旧传输仍为默认值                     |

语言绑定有意保持精简：

| 绑定       | 当前已实现                                                           | 仍在规划                            |
| ---------- | -------------------------------------------------------------------- | ----------------------------------- |
| TypeScript | 规范 `uint64`、兼容 RFC 8785 的 JSON 规范化、公开 CloudLink 夹具清单 | 完整的生产结构定义与传输编解码器    |
| Rust       | 全范围规范 `u64`、类型化失败、公开 CloudLink 夹具清单                | 完整的生产 JSON、模型与传输编解码器 |
| C99        | 有界规范 `uint64`、无分配的静态 P/M/A 查询、有界公开夹具配置档       | 完整的生产 JSON、模型与传输编解码器 |
| C++17      | C99 基础上的轻量视图与结果                                           | 明确禁止另建一套线协议语义          |

## 安全边界

边缘端对实时点位状态、确定性规则、安全联锁和物理执行拥有最终权威。Thing Model 能力只是声明，不是授权；它们默认拒绝，只能通过受治理作业运行，并始终服从边缘端的最终决定。

CloudLink 核心不是任意远程调用，不提供直接共享内存、寄存器或物理控制操作。独立的 Integration Control 扩展默认关闭，只公开受治理的 `device.power.set.v1` 语义动作。公开消息不能携带调用方选择的 Home Assistant 域、服务、服务参数、地址、令牌或任意 JSON。边缘策略保留最终执行权；提供方接受请求不能证明物理动作已经完成。MQTT PUBACK 只是传输证据，不能证明云端业务事实已持久提交。

Integration 拓扑和观测只携带规范化的提供方证据，不携带提供方凭据或直接服务调用。Home Assistant 地址和令牌留在边缘本地。CloudLink Integration 扩展通过外层会话认证网关，并原样承载公共载荷。提供方状态本身也不能独立证明物理执行。

## 仓库结构

- `spec/`：具有规范效力的英文语义和生命周期规则。
- `schemas/`：Thing Model、Integration、CloudLink、分发与 TCK 的封闭 JSON Schema。
- `profiles/`：传输和标准对齐配置档。
- `fixtures/`：有效、无效、上下文、迁移与黄金示例。
- `compatibility/`：失败分类和兼容性门槛。
- `tck/`：语言中立场景和仓库契约测试。
- `scripts/verify-consumer-lock.mjs`：默认离线的使用方发布验证器。
- `packages/`：实验性语言绑定，不具备规范效力。
- `contract-manifest.json`：发布身份和制品哈希。

运行自包含契约检查：

```sh
pnpm test:tck
```

运行全部语言绑定和打包检查：

```sh
pnpm check
```

C/C++ 使用方可以安装 CMake 项目，并链接 `AetherContracts::c` 或 `AetherContracts::cpp`。默认发布测试不需要消息代理、数据库、云账号或设备。真实消息代理与重启证据属于使用方，不能改变本版本的生产状态。

GitHub 标签、发布包和公开 SHA-256 校验值共同构成源码分发路径。语言包仓库可以在一致性验证后镜像生成的绑定；Cloudflare 可以缓存发布字节，但不能成为契约权威。使用方应提交封闭的使用方锁和精确发布清单，并默认在离线状态下验证已导入字节。只有需要完整源码检出的固件厂商才需要选择 Git 子模块。

历史 alpha.2 联合核心导入来自有本地改动、尚未提交的 AetherCloud 与 AetherEdge 工作区；当时边缘仓库仍名为 AetherIot。导入器拒绝任何不一致的源文件，最终字节也已锁定，但仅凭当时两个产品仓库的提交无法复现导入。该限制记录在 `compatibility/cloudlink-joint-core-provenance.json`。现在 AetherContracts 是唯一线协议权威；两个产品只消费带标签的字节，并在各自仓库保留实现、就绪状态和证据叠加层。

继续阅读 [AI 原生平台](https://docs.aetheriot.workers.dev/overview/ai-native-platform/)、[AetherContracts 快速开始](https://docs.aetheriot.workers.dev/aethercontracts/getting-started/)、[兼容性矩阵](https://docs.aetheriot.workers.dev/compatibility/version-matrix/)和[边缘端、公共协议与云端联动任务](https://docs.aetheriot.workers.dev/guides/edge-contracts-cloud/)。
