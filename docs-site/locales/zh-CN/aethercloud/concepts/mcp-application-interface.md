---
title: "MCP应用程序接口"
description: "通过相同的应用程序用例公开功能元数据、租户资源和受管工具，而无需创建仅代理的数据路径"
updated: 2026-07-15
status: mixed
---

# MCP 应用程序接口

> 权威来源：[AetherCloud](https://github.com/EvanL1/AetherCloud/blob/main/docs/concepts/mcp-application-interface.md)。此页面镜像到统一的 AetherIoT 文档中。

MCP 接口是 AetherCloud 应用程序命令和查询的适配器。它不拥有业务状态、放松授权、制造计划的功能或直接连接到 PostgreSQL、发件箱、CloudLink 或边缘运行时。

## 已实现的接口基础

`apps/mcp` 实现具有运行时解码和类型化结果的传输中性 `AetherCloudMcpInterface`：

- `listResources()`通告 `aethercloud://capabilities` 和 `aethercloud://audit/events{?cursor,limit}` 资源模板；
- 能力资源与 MCP 暴露状态分开报告应用程序状态，包括计划的 MCP 暴露；
- 审核资源注入经过身份验证的租户、项目、主题和权限，然后调用由HTTP；
- `listTools()` 仅公开可执行的 MCP 适配器，并包括权限、风险、确认、幂等性、到期、审核和输入架构元数据；
- `data.export.request` 调用 `RequestDataExport`；其高风险显式确认仍由应用程序命令强制执行；
- `edge.job.create` 调用 `CreateGovernedJob`；功能声明和边缘端决策对于所请求的工作仍然具有权威性；
- 在调用任何用例之前，未知或仅限应用程序的功能会失败并返回 `mcp-tool-not-implemented`。

经过身份验证的范围由未来的 MCP 组合根提供，而不是由工具的业务 `input` 提供。命令信封包含确认、幂等密钥、发布时间和到期时间；该接口将它们原封不动地转发给应用程序解码器。它不能削弱能力元数据。

## 已实施与计划

资源/工具注册、应用程序委托、精确外部解码、能力状态资源和行为测试已实施。目前还没有 MCP SDK 传输、stdio 服务器、可流式 HTTP 端点、OAuth 流、会话组合根、速率限制器或生产身份/审核持久性。因此，该软件包是可执行的接口基础，而不是 MCP 客户端现在可以连接到的网络服务器。

添加有线传输必须是围绕该接口的精简组合根。它必须翻译 MCP 协议错误和内容，而不更改底层命令/查询上下文或触及用例。命令工具默认保持拒绝；在其应用程序行为和显式 MCP 适配器可执行之前，该工具不存在。

## 安全性和内容边界

资源和工具返回有界的 JSON 内容。原始凭据、注册令牌、工件字节、Webhook 机密、导出字节、SHM 地址和设备寄存器值不是 MCP 内容。数据导出仅返回其受控资源状态和不可变对象元数据；下载仍然是一个单独的计划授权边界。

在公开其他资源或工具之前，请阅读[应用程序契约目录](/aethercloud/reference/application-contracts)、[审核和集成](/aethercloud/concepts/audit-and-integrations) 和[受管理的功能作业](/aethercloud/concepts/governed-capability-jobs)。
