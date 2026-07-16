---
title: "写作规则"
description: "通过 HTTP API 或下游产品控制台创作规则"
updated: 2026-07-10
---

# 写作规则

规则是 Aether 的控制逻辑：读取测量点 (M)、评估条件和写入操作点 (A) 的流程。它们执行内部自动化（端口 6002），并通过应用程序 API 直接或通过下游产品控制台编写。本指南涵盖了创作机制；有关引擎如何调度和执行规则的信息，请参阅[规则引擎](/concepts/rule-engine)，有关有效的控制策略，请参阅[控制策略](https://github.com/EvanL1/AetherEdge/blob/main/docs/domain/control-strategies.md)。

## 规则剖析

SQLite `rules` 表中的规则行包含：

- **`id`** — 自动分配的整数；你永远不会选择它。
- **`name`** 和 **`description`** - 对于人类。
- **`enabled`** — 新规则开始禁用；调度程序完全跳过禁用的规则。
- **`priority`** — 当多个规则到期时订单评估；请参阅[控制策略](https://github.com/EvanL1/AetherEdge/blob/main/docs/domain/control-strategies.md)，了解优先级如何与互斥条件相结合，以在写入同一执行器的规则之间进行仲裁。
- **`cooldown_ms`** — 成功执行至少一个操作后的最小间隙，抑制重新执行直至其结束。
- **`trigger_config`** — 规则运行时。两种变体，通过 `"type"` 进行区分：
  - `{"type": "interval", "interval_ms": 1000}` — 对调度程序刻度进行定期评估。没有 `trigger_config` 的规则默认为 1000 毫秒间隔（或者将其 `cooldown_ms` 作为周期，如果已设置）。
  - `{"type": "on_change", "point_refs": [{"instance": 1, "point_type": "measurement", "point": 0}], "time_deadband_ms": 200, "value_deadband": null}` — 订阅点更改时的事件驱动评估，通过时间死区（触发器之间的最小间隙）和可选值死区（绝对或百分比变化阈值）进行过滤。
- **流程** — 逻辑本身：起始节点扇出到输入节点（读取测量点或加载配置参数），通过决策节点（条件分支），到达操作节点（写入操作点），最后到达结束节点。

该流程存储两次 - `flow_json`（完整的可视化编辑器文档）和 `nodes_json`（引擎执行的紧凑拓扑） - 并且这两列始终通过一个函数从编辑器文档一起派生。 [规则引擎](/concepts/rule-engine) 解释了原因。

## 通过下游应用程序

独立的 [AetherEMS](https://github.com/EvanL1/AetherEMS) 控制台是一个可选的能量域参考应用程序，带有 Vue Flow 规则编辑器。它编辑完整的视觉文档 - 具有画布位置、标签、边缘和视口的节点 - 并通过相同的经过身份验证的规则命令 API 提交该文档。 AetherEdge 不会捆绑控制台或授予其直接 SQLite/SHM 访问权限。服务器一起导出两个存储的表示，因此 `flow_json` 和执行拓扑不会漂移（有关不变量，请参阅[规则引擎](/concepts/rule-engine)）。

## 通过 HTTP API

自动化服务于规则 API (`services/automation/src/rule_routes.rs`)：`http://localhost:6002/docs` 处的 Swagger UI 是每个操作的合约源。下面的每个变更仅接受承载管理员/工程师参与者，需要 `confirmed: true`，并在更改 SQLite 或重新加载调度程序之前写入强制审核记录。

| 方法 | 路径 | 用途 |
|--------|------|---------|
| GET | `/api/rules` | 分页列表（摘要字段： id、名称、启用、描述） |
| POST | `/api/rules` | 创建仅元数据存根 |
| GET | `/api/rules/{id}` | 完整规则，包括两个流程columns |
| PUT | `/api/rules/{id}` | 部分更新；流程和触发器位于此处 |
| DELETE | `/api/rules/{id}` | 删除规则 |
| POST | `/api/rules/{id}/enable` | 设置已启用 |
| POST | `/api/rules/{id}/disable` | 设置已禁用 |
| POST | `/api/rules/{id}/execute` | 立即执行（实际执行 - 见下文） |
| GET | `/api/rules/{id}/variables` | 规则读取的变量，例如监控 |
| GET | `/api/scheduler/status` | 调度程序运行标志、规则计数、刻度间隔 |
| POST | `/api/scheduler/reload` | 强制从 SQLite 重新读取所有规则 |

创建规则是**两步现实**： `POST /api/rules` 仅接受名称和描述，并插入一个存根 - 空 `{}` 拓扑，无编辑器文档，已禁用。流内容仅通过 `PUT /api/rules/{id}` 到达：
```bash
# 1. Create the stub; the response carries the assigned id
curl -X POST http://localhost:6002/api/rules \
  -H "Authorization: Bearer $AETHER_ACCESS_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"name": "Battery SOC Protection", "description": "Protect battery when SOC is too low", "confirmed": true}'
# → {"success": true, "data": {"id": 3, "name": "Battery SOC Protection", "status": "created"}}

# 2. Write the flow and trigger
curl -X PUT http://localhost:6002/api/rules/3 \
  -H "Authorization: Bearer $AETHER_ACCESS_TOKEN" \
  -H 'Content-Type: application/json' \
  -d @rule.json

# 3. Enable it
curl -X POST http://localhost:6002/api/rules/3/enable \
  -H "Authorization: Bearer $AETHER_ACCESS_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"confirmed": true}'
```

其中 `rule.json` 提供编辑器文档和触发器：
```json
{
  "flow_json": {
    "nodes": [
      {"id": "start", "type": "start", "position": {"x": 0, "y": 0},
       "data": {"config": {"wires": {"default": ["end"]}}}},
      {"id": "end", "type": "end", "position": {"x": 100, "y": 0}}
    ],
    "edges": []
  },
  "trigger_config": {"type": "interval", "interval_ms": 1000},
  "confirmed": true
}
```

该流程是最小的有效文档（它什么也不做）；有关包含输入、决策和操作节点的完整策略，请参阅附带的模板 `packs/energy/rules/battery_soc_management.json`，[控制策略](https://github.com/EvanL1/AetherEdge/blob/main/docs/domain/control-strategies.md) 逐个节点地遍历该模板。格式错误的流无法作为一个单元进行 PUT — 不会存储任何内容 — 并且格式错误的 `trigger_config` 在同一边界处被拒绝。

`aether` CLI 包装相同的端点。设置`AETHER_ACCESS_TOKEN`并将`--confirmed`传递给`rules create`、`update`、`enable`、`disable`和`delete`； `delete --force` 仅跳过交互式提示。 `rules list` 保持只读状态。

## 测试规则

**不存在空运行。** `POST /api/rules/{id}/execute` — 以及 `rules_execute` MCP 工具和 `aether rules execute <id> --confirmed` — 通过经过身份验证、确认和审核的应用程序命令执行真正的执行：根据实时值评估流程，触发的任何操作都会提交到本地指挥飞机。接受并不能证明物理设备执行了命令或达到了目标值。

因此针对尚不存在的硬件进行测试。虚拟协议没有精确的功能门，因此它始终可用于此：

1. 创建一个虚拟协议通道，其中的控制点和调整点与规则将写入的内容相匹配，并将临时实例的操作点路由到该通道（请参阅[连接设备](/guides/connect-devices)）。
2. 将规则的操作指向临时实例并执行：

```bash
   AETHER_ACCESS_TOKEN='<signed access JWT>' \
     aether rules execute 3 --confirmed
   ```

3. 检查结果。命令响应报告 `actions_attempted` 和 `actions_succeeded`，其中成功意味着本地命令平面接受。读回相应的测量值以验证物理行为。详细的执行路径和操作结果保留在本地 SQLite `rule_history` 中，供 API 和 WebSocket 读取器使用。

4. 一旦分支选择和写入值看起来正确，就可以在生产实例上重新定位规则的操作并启用它。

## 重新加载

您通常不会考虑重新加载：所有五个规则 CRUD 端点 — 创建、更新、删除、启用、禁用 — 触发调度程序在数据库写入后重新加载，因此通过 API 或编辑器所做的更改会立即生效，无需重新启动服务。

对于带外写入存在显式 `POST /api/scheduler/reload`：批量导入或 `aether sync` 在调度程序背后将规则文件推送到 SQLite。在这样的写入之后，点击端点一次，调度程序就会重新读取每个启用的规则并自动重建其变化订阅。 `GET /api/scheduler/status` 确认结果 — `running`、总计和启用的规则计数以及刻度间隔。

## 相关页面

- [规则引擎](/concepts/rule-engine) — 双列存储、调度、执行、热重载
- [控制策略作为规则](https://github.com/EvanL1/AetherEdge/blob/main/docs/domain/control-strategies.md) — 表达 SOC 管理和峰值按流剃须
- [连接设备](/guides/connect-devices) — 通道、虚拟协议、点映射
