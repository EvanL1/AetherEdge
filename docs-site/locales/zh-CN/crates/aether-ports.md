---
title: "aether-ports"
description: "用于Aether边缘扩展的小型、对象安全功能接口。"
updated: 2026-07-16
---

# aether-ports

用于 Aether 边缘扩展的小型对象安全功能接口。

该包将权威实时读取、采集拥有的写入、设备命令调度、审计、历史记录、镜像、持久发件箱、上行链路发布、I/O 通道调试和请求驱动的数据处理分开。 `ChannelMutator` 保持持久的所需配置权威性，并报告可重建的运行时投影、结果修订和协调状态，而无需选择有线编码。 `HistoryQuery` 和 `CovariateSource` 接受有界逻辑窗口并返回源出处； `DataProcessor` 接收完整的 `DataProcessingRequest` 并且没有回调到 Aether 数据源。它故意不公开通用数据库、缓存、模型或脚本运行程序API。主机在组合边界选择具体适配器。

`CloudLinkSpool` 有意与 `DurableOutbox` 分开：它拥有流纪元/位置、稳定的批次身份/摘要、重放和显式丢失证据，并且仅在经过验证的云应用程序接收后才删除记录。 `CloudLinkTransport` 承载有界逻辑路由，而不会将 MQTT 主题字符串暴露给核心调用者。没有 CloudLink 命令或任意 RPC 路由。

`HistoryQuery` 限制事件时间，但不隐式承诺双时态或源纪元历史；实现必须显式声明更强的时间点语义。同样，工件时间顺序不是历史端口的责任。

错误携带恢复语义，因此调用者可以区分不可用、瞬态、拒绝、无效数据和永久故障。
```bash
cargo test -p aether-ports
```

您可以选择根据 MIT 或 Apache-2.0 获得许可。
