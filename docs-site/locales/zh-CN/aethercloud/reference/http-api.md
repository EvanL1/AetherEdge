---
title: "HTTP API 参考"
description: "调用已实现的 HTTP 端点，并将它们与计划的版本化 API 区分开来"
updated: 2026-07-15
status: implemented
---

# HTTP API 参考

> 权威来源：[AetherCloud](https://github.com/EvanL1/AetherCloud/blob/main/docs/reference/http-api.md)。此页面已镜像到统一的 AetherIoT 文档中。

仓库基础里程碑公开了两个公共元数据路由和两个经过身份验证的审核查询路由。它尚未公开队列、遥测、提供商发现、部署、命令、Webhook/导出或 MCP 路由。实现的 `DiscoverProviderRegions` 应用程序查询故意不是 HTTP 合约，直到围绕它组成身份验证、租户上下文和运行时解码。

实现的 `PlanDeploymentStack` 应用程序命令也不是 HTTP 合约。没有端点接受模块、拓扑、计划工件或应用请求。其未来路线还需要持久审计、加密工件存储和生产状态锁定引擎适配器。

实现的网关注册、注册声明问题/消费和状态用例也只是应用程序合约。在组成生产租户身份边界、PostgreSQL 事务、持久审计和秘密支持的令牌服务之前，它们将保持不公开状态。此页面上描述的端点均不接受注册令牌。

## `GET /health`

返回进程活跃度，而不读取 PostgreSQL 或其他外部服务。

响应状态：`200 OK`
```json
{
  "status": "ok",
  "service": "aether-cloud-api",
  "version": "0.1.0"
}
```

此响应仅证明 API 进程可以处理请求。未来对已配置适配器的准备情况检查将使用单独的端点，因此数据库中断不会重写活动语义。

## `GET /api/v1/platform`

为客户端和编码代理返回稳定的产品角色和权限元数据。它不包含租户数据，也不执行 I/O。

响应状态：`200 OK`
```json
{
  "name": "AetherCloud",
  "role": "ai-native-multi-cloud-iot-control-plane",
  "authority": {
    "livePointState": "edge",
    "physicalControl": "edge",
    "tenantIdentity": "aether-cloud",
    "desiredRevision": "aether-cloud",
    "placementPolicy": "aether-cloud",
    "actualInfrastructure": "provider"
  },
  "multiCloud": {
    "providerModel": "capability-driven-adapters",
    "executionEngines": ["opentofu", "terraform"],
    "stateIsolation": "deployment-stack"
  }
}
```

这是有关稳定产品边界的元数据，而不是提供商连接、库存路由或基础设施执行端点已可用的证据。

## `GET /api/v1/audit/events`

仅搜索从经过身份验证的主题解析的租户和项目。该请求无法提供租户范围。它需要 `audit.event.read`。

支持的可选查询字段包括 `action`、`cursor`、`from`、`limit`、`resourceId`、`resourceKind`、`subjectId` 和 `to`。 `limit` 默认为 50，并受应用程序查询限制。游标是规范的十进制审计序列；协议 `int64` 值保留字符串。

 响应状态为 `200 OK`，带有 `{ "items": [...], "nextCursor": string |
null }`。每个项目都包含不可变的审计主题、操作、资源、结果、风险、确认、关联和可选的跟踪/证据摘要字段。

## `GET /api/v1/audit/events/stream`

返回编码为 `text/event-stream` 的相同授权查询结果。 `Last-Event-ID` 标头在审核序列后恢复，并且在两者都存在时必须与查询 `cursor` 一致。每个事件 ID 都是审核序列。

此路由是一个有限可恢复快照，以 `snapshot-complete` 注释结尾。它不是持久的实时订阅，也不能证明通知工作线程或代理的存在。

## 错误形状

版本化应用程序API使用包含代码、人类可读消息和相关标识的稳定信封：
```json
{
  "error": {
    "code": "permission-denied",
    "message": "permission audit.event.read is required",
    "correlationId": "request-correlation-id"
  }
}
```

审核路由根据适用情况返回 `400 invalid-input`、`401 unauthenticated` 或 `403 permission-denied`，并发出 `x-correlation-id`。

## 身份验证

`/health` 和 `/api/v1/platform` 有意公开，不包含租户或基础设施实例。审核路由需要`授权：承载
<token>`. The local server resolves the configured subject from
`AETHER_CLOUD_API_BEARER_TOKEN`、`AETHER_CLOUD_API_TENANT_ID`、`AETHER_CLOUD_API_PROJECT_ID`、`AETHER_CLOUD_API_SUBJECT_ID`和逗号分隔的`AETHER_CLOUD_API_PERMISSIONS`；缺少配置会拒绝每个受保护的请求。在恒定时间内比较精确的令牌。

此配置的承载适配器和内存审计存储适用于本地组合和合约测试，而不是生产身份或持久性。在调用应用程序用例之前，生产路由仍会从经过身份验证的身份解析租户上下文。正文、路径或查询租户身份绝不是访问证明。
