# Aether 文档

这里是仓库内文档的导航页。Aether 目前处于 Beta：行业中立 Kernel、SDK 与六服务
Runtime 已可使用，但部分生产路径仍在从兼容层迁移，AetherEMS Energy Pack 也尚未
完成独立发行。项目边界与拆分条件见
[ADR-0007](./adr/0007-aether-core-and-ems-distribution.md)。
[ADR-0013](./adr/0013-single-sdk-source-release.md) 规定了单一 SDK façade 与签名源码发行边界。
[ADR-0014](./adr/0014-coordinated-shm-topology-publication.md) 规定了 point/health SHM 协调发布协议；
[ADR-0015](./adr/0015-configuration-authority-and-reconciliation.md) 规定了配置权威与运行时协调边界；
[ADR-0017](./adr/0017-experimental-cloudlink-mqtt-edge-foundation.md) 规定了实验性 CloudLink
MQTT 边端基础、application ACK 与 legacy 迁移边界。

## 开始使用

- [快速开始](./guides/getting-started.md) — 安全空配置、首次启动与验收
- [连接设备](./guides/connect-devices.md) — 协议、通道、点位与投运流程
- [部署指南](./guides/deployment.md) — Compose、安装包与生产检查
- [配置参考](./reference/configuration.md) — 配置文件与环境变量

## 架构与安全

- [系统架构](./concepts/architecture.md) — 六个进程、依赖方向和故障边界
- [数据通路](./concepts/data-flow.md) — SHM 上行、命令下行与派生结果
- [共享内存](./concepts/shared-memory.md) — 实时状态权威与读写边界
- [数据模型](./concepts/data-model.md) — 实例、通道与点位
- [规则引擎](./concepts/rule-engine.md) — 确定性本地自动化
- [安全操作](./guides/safe-operations.md) — 权限、确认、审计与设备控制
- [Architecture Decision Records](./adr) — 核心架构决策

## 接口

- [HTTP API 与 Swagger UI](./reference/http-api.md) — 六服务端口、JWT、响应格式与路由概览
- [CLI 参考](./reference/cli.md) — 本地运维与部署命令
- [MCP 工具参考](./reference/mcp-tools.md) — 默认只读的 AI 能力面
- [连接 AI 助手](./guides/ai-assistants.md) — MCP 接入与写操作门槛
- [旧版 API 汇编](./API_REFERENCE.md) — 仅供迁移查阅，不是当前契约

Swagger UI 是可选构建能力。安装包使用 `--enable-swagger` 后，六个服务都在
`/docs` 提供 UI、在 `/openapi.json` 提供规范；只有 `aether-api:6005` 可以作为
远程管理入口，其余服务端口必须保留在主机 loopback。Gateway 的 Swagger 路由
本身不经过 JWT middleware，只应在受信的投运网络中启用。

## 可选 Data Processing

- [概念与边界](./concepts/data-processing.md)
- [数据通路](./concepts/data-processing-flow.md)
- [Processor 接入指南](./guides/data-processors.md)
- [传输契约](./reference/data-processing-contracts.md)
- [AetherEMS 能源预测映射](https://github.com/EvanL1/AetherEMS/blob/main/packs/energy/knowledge/power-forecasting.md)

Data Processing 是可选能力，不属于采集或硬实时安全闭环。

## 实验性 CloudLink MQTT

- [Edge contract 与兼容性边界](./reference/cloudlink-mqtt-v1.md)
- [ADR-0017](./adr/0017-experimental-cloudlink-mqtt-edge-foundation.md)
- [严格 candidate schemas 与 fixtures](../contracts/cloudlink/README.md)

该能力目前只代表可独立验证的 AetherEdge edge foundation；默认仍运行 legacy MQTT。
AetherCloud 尚未接受 wire candidate，因此不得表述为正式协议或已经完成生产联调。
