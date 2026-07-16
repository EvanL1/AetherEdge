---
title: "aether-domain"
description: "行业中立的 Aether 边缘内核的 `no_std` 域类型。"
updated: 2026-07-12
---

# aether-domain

行业中立，Aether 边缘内核的 `no_std` 域类型。

此包定义点地址和样本、强类型标识符、质量状态、时间戳、经过验证的控制命令和 Aether 数据处理合约。处理模型将应用程序端 `ProcessTaskRequest` 与完整的处理器端 `DataProcessingRequest` 分开，并将 `ProcessingResult` 视为不可信，直到作为 `DerivedData` 接受为止。

crate 仍为 `no_std`；拥有的处理框架使用 `alloc` 集合。它没有异步运行时、数据库、网络、服务、模型框架或硬件依赖性。

在实现需要交换稳定边缘域值的 Aether 主机、扩展、协议适配器或固件组件时使用它。
```bash
cargo test -p aether-domain
cargo tree -p aether-domain --edges normal
```

您可以选择根据 MIT 或 Apache-2.0 获得许可。
