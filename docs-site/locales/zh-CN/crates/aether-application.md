---
title: "aether-application"
description: "Aether 边缘内核的传输中立命令和查询用例。"
updated: 2026-07-12
---

# aether-application

Aether 边缘内核的传输中立命令和查询用例。

`EdgeApplication` 由 CLI、MCP 和可选网络传输共享。它授权点读取、验证设备命令、需要审核接收器并通过功能端口分派控制。基础设施选择位于此包之外，因此其默认图表不包含 Redis、PostgreSQL、SQLx、MQTT 客户端或 Web 框架。

控制、手动规则执行、规则/警报策略更改、警报解析、I/O 通道调试和物理操作路由突变在调用非幂等端口之前会保留 `Attempted` 审核事件。通道审核详细信息标识更改的字段，但从不包括协议参数或每个通道的日志记录值。如果无法存储预执行事件，则执行失败关闭。一旦端口接受操作，未能附加终端 `Succeeded` 事件将作为包含 `CompletionAuditStatus::Incomplete`、请求和命令/规则相关 ID 以及 `is_retryable() == false` 的 `AcceptedOutcome` 返回；它永远不会变成可以执行操作两次的可重试错误。审核详细信息包括特定于操作的命令目标、规则标识符、操作计数或路由键。

`DataProcessingApplication` 是 Aether 数据处理的传输中立查询外观。组合根注册一个声明性任务、一个 `DataProcessingBinding` 和一个 `DataProcessor` 路由。该绑定将任务本地测量名称解析为只读 `PointAddress` 值，并可以固定静态功能和工件选择器； API 调用者永远不会选择处理器路由。

着陆的外部绑定是 `aether-api` 中经过身份验证的 `/api/v1/data-processing/*` HTTP 表面。数据处理 CLI 和 MCP 绑定未在版本 1 中实现。

对于应用程序在读取数据之前授权的每个处理请求，查询有界历史记录和协变量，可选择合并仅针对 `Last` 聚合功能的完全对齐的只读实时尾部，组装一个完整的框架，其中每个功能有一个出处条目，计算共享规范摘要，应用精确的帧和负载限制，以及将处理器响应视为不可信。只有相关的、政策兼容的结果才会成为`DerivedData`。外观没有 SHM 编写器、历史接收器或命令调度程序，并且空路由集仍然是有效的默认配置。传输请求和结果 ID 是稳定的 UUIDv5 派生，而调用者的原始请求 ID 保留在审核记录中。查询是非幂等的：重复的内容可以保留稳定的相关 ID，但仍会执行处理器并为每次调用进行所需的审核。

应用程序限制源事件时间；它无法制造适配器不提供的时间顺序。使用当前的 SQLite 架构和工件选择器，旧的 `as_of` 并不能证明摄取时间/源纪元或模型可用性削减。历史评估必须提供冻结的投入品或港口，其契约包含并验证这些削减。
```bash
cargo test -p aether-application
```

您可以选择根据 MIT 或 Apache-2.0 获得许可。
