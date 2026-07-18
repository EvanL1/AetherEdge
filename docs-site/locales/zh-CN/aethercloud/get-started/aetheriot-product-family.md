---
title: "AetherIoT 产品系列"
description: "将 AetherCloud 放置在 AetherEdge 和 AetherContracts 旁边，不要模糊边缘、云、提供商或协议权限"
updated: 2026-07-16
status: normative
---

# AetherIoT 产品系列

> 权威来源：[AetherCloud](https://github.com/EvanL1/AetherCloud/blob/main/docs/get-started/aetheriot-product-family.md)。此页面镜像到统一的 AetherIoT 文档中。

AetherIoT 是伞式项目和公共平台标识。它不是额外的运行时、控制平面或协议。
```text
AetherIoT
├── AetherEdge       edge runtime, Kernel, CLI, and SDK
├── AetherCloud      cloud fusion and governed control plane
└── AetherContracts  public specifications, Schemas, fixtures, and TCK

AetherEMS            industry solution built on the platform
```

以前名为 `EvanL1/AetherIot` 的边缘仓库移至 `EvanL1/AetherEdge`。现有的 `aether-*` 包和二进制文件、`aether`、CLI、`aether-edge-sdk`、配置标识符、安装程序名称和 CloudLink 合约标识符保持稳定。

## 权限不会随名称而变化

- AetherEdge 仍然对实时点状态、获取、确定性规则具有权威性，安全联锁、本地策略和最终物理执行。
- AetherCloud 对于所需的放置和受管理的云作业仍然具有权威。
- 提供商对于其资源的实际存在和本机状态仍然具有权威。
- AetherContracts 仍然是唯一的共享互操作性权威。

已发布的标签、证据、出处记录和摘要固定AetherContracts 版本是不可变的。 alpha.3 版本和导入的消费者关闭可能会保留历史上的 AetherIot 名称。仓库重命名不会更改其一致性状态。

统一文档入口是 [docs.aetheriot.workers.dev](https://docs.aetheriot.workers.dev/)。内容按智能体任务、部署与运维、安全与治理、故障恢复、平台参考以及兼容性与状态组织。产品仓库仍是实现细节的权威来源。

仓库地址变化和保持稳定的软件标识，请参阅公开的
[AetherIot 到 AetherEdge 迁移指南](https://docs.aetheriot.workers.dev/migration/aetheriot-to-aetheredge/)。
