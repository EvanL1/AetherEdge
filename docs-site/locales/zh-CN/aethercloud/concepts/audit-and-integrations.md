---
title: "审核、订阅、Webhook 交付和数据导出"
description: "公开租户范围的证据和持久的集成工作流程，而无需绕过应用程序用例或泄露目标机密"
updated: 2026-07-15
status: mixed
---

# 审核、订阅、Webhook 传送和数据导出

> 权威来源：[AetherCloud](https://github.com/EvanL1/AetherCloud/blob/main/docs/concepts/audit-and-integrations.md)。此页面镜像到统一的 AetherIoT 文档中。

此上下文将应用程序证据转化为可查询的审核历史记录和受管控的出站工作。它不会使 OpenTelemetry 成为业务分类帐，也不会让 HTTP、SSE、Webhook、导出或未来的 MCP 接口直接写入有界上下文存储。

## 权限和事务边界

拥有的命令事务必须一起提交其聚合更改、所需的审核事件和发件箱记录。集成工作进程稍后会使用提交的发件箱事实。失败的 Webhook 或断开连接的 SSE 客户端可能会延迟外部投影，但无法回滚或更改原始业务结果。

审核是仅附加证据，范围为 `TenantId` 和 `ProjectId`。操作跟踪可以通过跟踪标识符进行关联，但采样的 OpenTelemetry 数据既不是授权证据，也不是审计的替代品。

## 已实现的层

存在以下可执行基础：

- 具有规范无损序列、主题、资源、结果、治理、关联和可选证据摘要的不可变 `AuditEvent` 域值；
- 授权`SearchAuditEvents` 应用程序查询和租户/项目范围内存仓库；
- 经过身份验证的 `GET /api/v1/audit/events` 和有限可恢复 `GET /api/v1/audit/events/stream` SSE 快照，两者均调用同一查询；
- `WebhookSubscription` 使用稳定目标引用和有界事件白名单创建、禁用和获取用例；
- a `WebhookDelivery` 生命周期，具有持久的运行中意图、稳定的交付幂等密钥、有界重试、尝试证据、可见死信状态和显式确认重新驱动；
- `DataExport` 生命周期，用于有界审计、警报或遥测历史记录导出，具有高风险显式确认请求、工作进程结果报告、不可变对象引用、摘要和无损字节长度；
- 内存一致性适配器，自动保留应用程序测试的聚合、幂等性、审计和发件箱证据。

SSE 端点特意是一个有限快照。 `Last-Event-ID` 通过相同的应用程序查询从审核序列恢复，但尚不存在持久的实时通知进程。

## 状态机
```text
Webhook subscription: active -> disabled

Webhook delivery:
pending -> delivering -> delivered
                     \-> retrying -> delivering
                     \-> dead-lettered -> pending (explicit redrive)

Data export:
queued -> running -> ready
                  \-> failed
queued/ready/failed -> expired
```

在调用外部发送者之前，尝试将被写入 `delivering`。因此，坠机可能会导致飞行中的尝试需要工作进程协调；绝不能将其视为从未尝试过而默默地对待。传递身份是重试过程中面向接收者的幂等性密钥。冲突的排队或重新驱动失败关闭。

导出响应从不包含原始导出字节。 `ready` 仅公开有界对象引用、内容摘要和十进制 `uint64` 字节长度。在客户端检索内容之前，仍然需要单独的授权下载功能和生产对象存储适配器。

## 目标和 SSRF 边界

应用程序和域记录包含 `WebhookDestinationId`，而不是任意请求提供的 URL 或纯文本签名密钥。生产发送方必须解析来自租户范围内的秘密支持的目标注册表的引用，要求 HTTPS，在 DNS 解析和重定向后拒绝私有/链接本地/保留地址目标，绑定响应字节和时间，对规范传送进行签名，并防止凭证转发。发送者和注册表已计划好；当前内存发送方仅供测试。

## 缺少生产表面

PostgreSQL仅附加审核、事务性发件箱消耗、目标注册表、秘密轮换、生产HTTP发送方、签名、DNS/重定向 SSRF 防御、重试租赁、实时 SSE 通知程序、WebSocket、对象存储、导出工作进程、保留/配额强制执行和公共 Webhook/导出API仍在计划中。当前的 API 使用内存审计仓库和配置的承载身份，因此它是用于本地组合和合约测试的可执行接口，而不是生产身份或持久性边界。

阅读 [ADR-0012](https://github.com/EvanL1/AetherCloud/blob/main/docs/adr/0012-durable-audit-and-outbound-integrations.md)、[HTTP API 参考](/aethercloud/reference/http-api) 和[应用程序合约添加外部集成表面之前的目录](/aethercloud/reference/application-contracts)。
