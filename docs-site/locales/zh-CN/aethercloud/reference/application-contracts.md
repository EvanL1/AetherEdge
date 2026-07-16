---
title: "应用契约目录"
description: "从机器可读目录中发现能力治理、错误、事件、HTTP 操作和实现层级"
updated: 2026-07-16
status: mixed
---

# 应用契约目录

> 权威来源：[AetherCloud](https://github.com/EvanL1/AetherCloud/blob/main/docs/reference/application-contracts.md)。此页面镜像到统一的 AetherIoT 文档中。

[`ai/application-contracts.json`](https://github.com/EvanL1/AetherCloud/blob/main/ai/application-contracts.json) 是应用程序功能、权限、命令治理、键入错误、集成事件、HTTP 操作和实现状态的机器可读索引。

`mcp_exposures` 单独报告是否实现了资源/工具适配器以及是否存在可连接的协议传输。这可以防止在组合根可用之前将可执行的传输中立接口描述为 MCP 服务器。

目录是索引，而不是备用执行路径。 TypeScript 域和应用程序代码对于可执行行为仍然具有权威性，而传输架构对于其编码输入和输出仍然具有权威性。

## 状态含义

- `implemented` 表示指定的传输及其生产所需的行为是可执行的。
- `partial` 在列出缺失的生产或传输时标识确切的可执行层
- `planned` 是经过审核的名称或契约，没有可执行的产品表面。
- `deprecated` 仅保留用于迁移的列表。

仅由内存适配器支持的应用程序用例是 `partial`，即使其域和应用程序测试通过也是如此。事件在业务事务将其写入真正的发件箱之前一直是`planned`。直到线路解码和所属应用程序行为都可执行后，才会实现记录的 CloudLink 消息。

## 命令治理

每个命令条目都声明权限、风险、确认、幂等性、过期和审核。缺少元数据意味着该命令无法公开。传输可以添加更严格的策略，但不能削弱目录治理。

查询声明许可，并且永远不会改变产品状态。改变投影的源自边缘的观察即使在描述事实而不是请求物理工作时也是命令。

## 兼容性

`schema_version`在不兼容的目录结构更改之前发生更改。功能名称、错误代码、事件名称和操作标识符在一个版本内保持稳定。仅当老读者可以安全地忽略它时，添加字段才是向后兼容的。

该目录当前记录了两个已实现的公共 HTTP 操作和两个经过身份验证的审核查询操作（JSON 和有限 SSE 快照）；部分提供程序、计划、注册、CloudLink 会话、运行时清单、遥测、警报、工件注册表、部署、受管作业、审计、Webhook 和数据导出应用程序切片；并规划了后期的物联网产品能力。网关注册和登记事件现在为 `partial`，因为 PostgreSQL 适配器将它们写入与聚合和审核状态相同的事务中。该目录现在将实验 CloudLink MQTT 层与生产成分和联合 AetherEdge 一致性分开命名。它不声明存在生产实时事件流、外部 Webhook 发送器、导出工作​​器/下载接口、MCP 有线服务器、生产 CloudLink 进程、迁移运行程序或已部署的生产数据库。
