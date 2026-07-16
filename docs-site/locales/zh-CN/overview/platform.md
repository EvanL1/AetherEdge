---
title: "AetherIoT 平台概述"
description: "AetherIoT 是一系列可互操作、边缘优先的 IoT 产品的开源项目标识。它不是第四个运行时，也不拥有……"
updated: 2026-07-16
---

# AetherIoT 平台概述

AetherIoT 是一系列可互操作、边缘优先的 IoT 产品的开源项目标识。它不是第四个运行时，也不拥有单独的有线协议。
```text
AetherIoT
├── AetherEdge       edge runtime, Kernel, CLI, and SDK
├── AetherCloud      cloud fusion and governed control plane
└── AetherContracts  public specifications, Schemas, fixtures, and TCK

AetherEMS            energy-management solution built on the platform
```

## 产品边界

| 产品 | 拥有 | 不拥有 |
| --- | --- | --- |
| AetherEdge | 实时点状态、获取、确定性规则、安全联锁、本地历史记录和最终物理执行 | 云放置、提供商资源或公共协议权威 |
| AetherCloud | 所需放置、受控云作业、租户控制平面状态和多云协调 | 边缘实时状态权威或提供商本机实际状态 |
| AetherContracts | 语言中立协议语义、封闭Schema、测试夹具、稳定故障类和可执行一致性证据 | 产品运行时行为、凭证、云持久性或部署策略 |
| AetherEMS | 能源领域模型、工作流程和解决方案体验 | 行业中立的平台核心 |

每个基础设施提供商对其资源的实际存在和提供商本机状态保持权威。云故障不得停止已委托的 AetherEdge 运行时。

## 命名规则

- 对项目、社区、网站和完整平台使用 **AetherIoT**。
- 对以前名为 AetherIot 的仓库和产品使用 **AetherEdge**。
- 保留现有的 `aether-*` 包名称， `aether` CLI、`aether-edge-sdk` 软件包、安装程序名称和协议标识符保持稳定。
- 逐字节保留历史版本工件和摘要固定的 AetherContracts 捆绑包。新的显示名称永远不会重写旧的证据。

阅读[AetherIot 到 AetherEdge 迁移指南](/migration/aetheriot-to-aetheredge) 以了解仓库和自动化更改。

## 文档所有权

此站点是公共入口点。每个产品仓库仍然是其实现细节的真实来源，而 AetherContracts 仍然是共享协议行为的唯一权威。统一页面链接到这些来源，而不是将规范内容复制到第二个权威机构。

继续[部署拓扑](/overview/deployment-topologies)、[用户旅程](/overview/user-journeys)或[边缘到契约到云教程](/tutorials/edge-contracts-cloud)。
