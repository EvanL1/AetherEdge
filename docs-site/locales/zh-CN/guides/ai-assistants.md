---
title: "连接AI助手"
description: "将 Claude 或任何 MCP 客户端指向 aether mcp，然后在只读和写入访问之间进行选择"
updated: 2026-07-12
---

# 连接 AI 助手

`aether` CLI 兼作 MCP（模型上下文协议）服务器：`aether mcp` 在 stdio 上运行，并将系统功能作为工具公开，因此 Claude — 或任何 MCP 客户端 — 可以检查通道、查询历史记录、读取警报以及（在明确允许的情况下）操作系统。本页介绍客户端设置、将服务器指向远程安装以及只读/写入访问模型。

## 您将获得什么

生产 MCP 目录有两层 45 个工具：

- **23 个只读工具**，始终注册 — 列出和检查通道及其点映射（`channels_list`、`channels_status`、 `channels_points`）、警报和警报规则（`alarms_list`、`alarms_stats`）、控制规则（`rules_list`、`rules_get`）、路由、历史数据（`history_query`、`history_latest`）、产品型号和设备实例（`models_products`、`models_instances`）、通道模板和云链接状态（`net_mqtt_status`、`net_cert_info`）。
- **22 种受控写入工具**，仅在服务器随 `--allow-write` 启动时注册：`channels_create`、`channels_update`、`channels_delete`、`channels_enable`、`channels_disable` 和 `channels_reconcile`；`models_instances_action`；`rules_execute`；`rules_create`、`rules_update`、`rules_delete`、`rules_enable` 和 `rules_disable`；`alarms_rule_create`、`alarms_rule_update`、`alarms_rule_delete`、`alarms_rule_enable` 和 `alarms_rule_disable`；`alarms_resolve`；以及 `routing_action_upsert`、`routing_action_delete` 和 `routing_action_set_enabled`。它们分别映射到受管控的 `io.channel.manage`、`io.channel.reconcile`、`device.write_point`、`automation.rule.execute`、`automation.rule.manage`、`alarm.rule.manage`、`alarm.alert.resolve` 和 `automation.routing.manage` 应用能力。每次写入都需要签名身份、`confirmed: true`、应用授权和强制审计。请参阅下文“只读与写入访问”。

每个工具都会针对 `aether` 命令行使用的同一服务 HTTP API 封装一个 CLI 客户端调用。结果以结构化内容形式返回；失败或无法访问的服务会以可读错误文本的形式返回，而不是不透明的协议错误。

服务器还会以 MCP 资源的形式提供你正在阅读的文档，因此助手无需离开会话即可理解领域概念，例如 PCS 的含义以及哪些操作会写入真实硬件。

一个标志说明：CLI 的全局`mcp` 会忽略 `--json` 标志（服务器始终采用 MCP 自己的 JSON-RPC 协议），如果通过，则会打印警告。

## Claude Desktop

添加到 `claude_desktop_config.json`（`aether` 二进制文件必须位于 `PATH` 上，或使用绝对路径）：
```json
{
  "mcpServers": {
    "aether": {
      "command": "aether",
      "args": ["mcp"]
    }
  }
}
```

## 克劳德·科德
```bash
claude mcp add aether -- aether mcp
```

对于需要写访问权限的会话（请参阅下面的访问模型）：
```bash
claude mcp add aether -- aether mcp --allow-write
```

## 指向远程系统

MCP 服务器不必在边缘设备上运行。每个工具都与 Aether 服务 API 通信，因此笔记本电脑上的 `aether mcp` 可以检查远程安装。服务器启动时解决了两种机制：

- **`--host <hostname>`** 重写所有五个服务 URL 的主机，同时保留纯文本 HTTP 和默认端口。当所有服务都在一台受信任的网络主机上运行时，这是只读工具的快速路径：

```bash
  aether mcp --host 192.168.1.50
  ```

- **五个环境变量** 独立设置每个服务 URL，当每个服务的方案、端口或主机不同时很有用：

  | 环境变量 | 服务 | 提供的工具 | 默认 |
  |----------------------|---------|--------------|---------|
  | `AETHER_IO_URL` | io | 通道、点、模板 | `http://localhost:6001` |
  | `AETHER_AUTOMATION_URL` | 自动化 | 规则、路由、模型/实例 | `http://localhost:6002` |
  | `AETHER_ALARM_URL` | 警报 | 警报 | `http://localhost:6007` |
  | `AETHER_UPLINK_URL` | 上行链路 | MQTT，证书 | `http://localhost:6006` |
  | `AETHER_HISTORY_URL` | 历史记录 | 历史记录 | `http://localhost:6004` |

优先级：`--host` 获胜 — 当它通过时，不参考环境变量。如果两者均未设置，则一切默认为 `localhost`。

允许通过环回 HTTP 进行携带 `AETHER_ACCESS_TOKEN` 的受保护写入，以进行设备上操作。对于任何支持远程写入的 MCP 服务器，请省略 `--host` 并将服务变量指向经过证书验证的 HTTPS 入口。在为请求选择或附加承载令牌之前，传输防护会拒绝非环回明文 HTTP。

在 Claude Desktop 配置中，启用远程写入的服务器可以使用 `env` 块，如下所示（将示例主机名替换为您的入口端点）：
```json
{
  "mcpServers": {
    "aether-site-a": {
      "command": "aether",
      "args": ["mcp", "--allow-write"],
      "env": {
        "AETHER_IO_URL": "https://io.edge.example.test",
        "AETHER_AUTOMATION_URL": "https://automation.edge.example.test",
        "AETHER_ALARM_URL": "https://alarm.edge.example.test",
        "AETHER_UPLINK_URL": "https://uplink.edge.example.test",
        "AETHER_HISTORY_URL": "https://history.edge.example.test",
        "AETHER_ACCESS_TOKEN": "<SIGNED_ADMIN_OR_ENGINEER_TOKEN>"
      }
    }
  }
}
```

## 只读与写入访问

默认情况下，`aether mcp` 是只读的。这不是建议性注释：如果没有 `--allow-write`，22 个写入工具永远不会注册，也根本不会出现在 `tools/list` 响应中。客户端无法调用（甚至无法查看）未注册的内容，因此无论客户端如何配置或模型如何行为，保证都有效。

使用 `--allow-write` 启动服务器是有意为之的行为，但该标志只是一个注册门。它不是对任何命令的确认。 MCP 调用者仍必须在每次调用时传递 `confirmed: true`，并且应用程序会在分派之前拒绝未经授权或无法审核的请求。 MCP 网桥读取 `AETHER_ACCESS_TOKEN`，将其作为 `Authorization: Bearer` 凭证发送到服务，并为每个受管请求生成一个 `X-Request-ID`。它拒绝将该凭证附加到非环回明文HTTP；远程写入需要经过证书验证的 HTTPS 入口。保留返回的 `request_id` 和任何 `command_id`：超时和不完整的审核或发布响应不是安全的自动重试信号。成功的设备命令响应意味着本地命令平面接受了该命令；它并不能证明物理设备执行了它。路由响应意味着物理目标已被持久化并发布；它不执行设备命令。 **在启用写入之前，请阅读[应用程序和代理的安全操作](/guides/safe-operations)。**如果命令响应报告 `audit.status="incomplete"`，则该命令已被接受：保留其 `request_id`/`command_id` 并且不要重试。通道突变成功还可以报告降级的运行时投影。保留其`request_id`和`resulting_revision`，检查`reconciliation_required`，并且不自动重试非幂等调试命令。

信道模拟/点批量和上行链路配置/证书操作保留在MCP之外。通道 CRUD/生命周期、规则 CRUD/生命周期、警报规则 CRUD/生命周期和警报解决方案之所以存在，只是因为它们的架构和应用程序功能明确映射在 22 工具写入允许列表中。现有的包装器不会仅仅因为 `--allow-write` 的存在而被提升为 AI 工具。

一行规则：**授予助理对任务的写入权限，而不是默认设置。** 为需要它的会话注册可写入的服务器，然后返回到只读。

## 资源

除了工具之外，服务器还以只读方式提供此文档的精选子集两种模式下的 MCP 资源。嵌入内核页面；仅当经过验证的 Pack 在 `global.yaml` 中处于活动状态时，才会显示 Pack 知识。支持 MCP 资源的客户端可以直接提取上下文，而无需依赖模型的先验知识：

- `aether://packs/energy/knowledge/ess-primer` — Energy Pack 处于活动状态时的能量存储概念
- `aether://packs/energy/knowledge/safe-operations` — Energy Pack 安全合约
- `aether://docs/concepts/architecture` — 七项服务及其对话方式
- `aether://docs/concepts/data-model` — 实例、通道、点
- `aether://docs/reference/mcp-tools` — 完整工具参考

## 相关页面

- [应用程序和代理的安全操作](/guides/safe-operations) — 请先阅读此内容`--allow-write`
- [系统架构](/concepts/architecture) — 工具背后的服务
- [MCP 工具参考](/reference/mcp-tools) — 每个工具及其参数
- [入门](/guides/getting-started) — 构建、初始化和启动工具与之对话的堆栈
