# AetherEMS API 参考文档

本文档提供 AetherEMS 所有 REST API 端点的完整参考。

## 服务概览

| 服务 | 端口 | 说明 | Swagger UI |
|------|------|------|------------|
| **aether-io** | 6001 | 通信服务 - 设备协议、通道管理、点位数据 | `http://localhost:6001/swagger-ui/` |
| **aether-automation** | 6002 | 模型服务 - 产品定义、设备实例、路由、规则 | `http://localhost:6002/swagger-ui/` |
| **aether-history** | 6004 | 历史数据服务 - TimescaleDB 时序数据持久化 | - |
| **aether-api** | 6005 | API 网关 - 统一 REST API、WebSocket、JWT 认证 | - |
| **aether-uplink** | 6006 | 网络服务 - MQTT 代理集成 | - |
| **aether-alarm** | 6007 | 告警服务 - 告警规则与通知 | - |
| **apps** | 8080 | 前端 - Vue.js 3 + ECharts 仪表盘 | - |

---

## 目录

- [通用约定](#通用约定)
- [Io API (端口 6001)](#io-api-端口-6001)
  - [健康检查](#健康检查)
  - [通道管理](#通道管理)
  - [点位管理](#点位管理)
  - [控制操作](#控制操作)
  - [映射管理](#映射管理)
- [Automation API (端口 6002)](#automation-api-端口-6002)
  - [产品管理](#产品管理)
  - [实例管理](#实例管理)
  - [路由管理](#路由管理)
  - [规则管理](#规则管理)
  - [调度器控制](#调度器控制)
- [管理员 API](#管理员-api)
- [WebSocket API](#websocket-api)
- [错误处理](#错误处理)

---

## 通用约定

### 基础 URL

```
# 开发环境
http://localhost:6001  # io
http://localhost:6002  # automation

# 生产环境（通过环境变量配置）
${AETHER_IO_URL}  # 默认 http://127.0.0.1:6001
${AETHER_AUTOMATION_URL}  # 默认 http://127.0.0.1:6002
```

### 响应格式

**成功响应：**
```json
{
  "success": true,
  "data": { ... }
}
```

**分页响应：**
```json
{
  "success": true,
  "data": {
    "list": [...],
    "total": 100,
    "page": 1,
    "page_size": 20,
    "total_pages": 5,
    "has_next": true,
    "has_previous": false
  }
}
```

**错误响应：**
```json
{
  "success": false,
  "error": {
    "code": 404,
    "message": "Channel 1001 not found"
  }
}
```

> **注意**：automation 的错误响应可能包含额外字段 `details`（错误详情）和 `suggestion`（修复建议）。

### 通用查询参数

| 参数 | 类型 | 说明 |
|------|------|------|
| `page` | int | 页码（从 1 开始） |
| `page_size` | int | 每页数量（默认 20，最大 100） |

---

## Io API (端口 6001)

### 健康检查

#### GET /health

检查服务健康状态。

```bash
curl http://localhost:6001/health
```

**响应：**
```json
{
  "success": true,
  "data": {
    "status": "healthy",
    "service": "io",
    "version": "0.1.0",
    "uptime_seconds": 3600,
    "timestamp": "2026-01-20T08:35:05Z",
    "checks": {
      "redis": {"status": "healthy", "message": "Connected", "duration_ms": 2},
      "sqlite": {"status": "healthy", "message": "Connected", "duration_ms": 0},
      "channels": {"status": "healthy", "message": "3/3 running"}
    },
    "system": {
      "cpu_count": 11,
      "memory_total_mb": 36864,
      "process_cpu_percent": 0.0,
      "process_memory_mb": 13
    }
  }
}
```

#### GET /api/status

获取服务详细状态。

```bash
curl http://localhost:6001/api/status
```

**响应：**
```json
{
  "success": true,
  "data": {
    "name": "Communication Service",
    "version": "0.1.0",
    "uptime": 3600,
    "start_time": "2026-01-20T06:25:02Z",
    "channels": 3,
    "active_channels": 3
  }
}
```

---

### 通道管理

#### GET /api/channels

获取所有通道（完整信息）。

```bash
curl http://localhost:6001/api/channels
```

#### GET /api/channels/list

获取通道列表（精简格式）。

```bash
curl http://localhost:6001/api/channels/list
```

**响应：**
```json
{
  "success": true,
  "data": {
    "list": [
      {"id": 1, "name": "PCS#1"},
      {"id": 2, "name": "BAMS#1"}
    ]
  }
}
```

> **注意**：此端点返回精简的通道列表，不支持分页参数（通道数量通常有限）。如需完整通道信息请使用 `GET /api/channels`。

#### GET /api/channels/search

搜索通道。

```bash
curl "http://localhost:6001/api/channels/search?q=PCS&protocol=modbus_tcp"
```

#### POST /api/channels

创建新通道。

```bash
curl -X POST http://localhost:6001/api/channels \
  -H "Content-Type: application/json" \
  -d '{
    "channel_id": 1001,
    "name": "PCS#1",
    "protocol": "modbus_tcp",
    "enabled": true,
    "parameters": {
      "host": "192.168.1.10",
      "port": 502
    }
  }'
```

**响应：**
```json
{
  "success": true,
  "data": {
    "id": 1001,
    "name": "PCS#1",
    "description": null,
    "protocol": "modbus_tcp",
    "enabled": true,
    "runtime_status": "connecting",
    "message": "Channel created and started successfully"
  }
}
```

#### GET /api/channels/{id}

获取通道详情。

```bash
curl http://localhost:6001/api/channels/1
```

**响应：**
```json
{
  "success": true,
  "data": {
    "id": 1,
    "name": "PCS#1",
    "description": "变流器 #1",
    "protocol": "modbus_tcp",
    "enabled": true,
    "parameters": {
      "host": "192.168.1.10",
      "port": 502,
      "connect_timeout_ms": 3000,
      "read_timeout_ms": 3000
    },
    "logging": {"enabled": true, "level": "info", "file": null},
    "runtime_status": {
      "connected": true,
      "running": true,
      "last_update": "2026-01-20T08:38:16Z",
      "statistics": {...}
    },
    "point_counts": {
      "telemetry": 766,
      "signal": 109,
      "control": 17,
      "adjustment": 553
    }
  }
}
```

#### PUT /api/channels/{id}

更新通道配置。

```bash
curl -X PUT http://localhost:6001/api/channels/1001 \
  -H "Content-Type: application/json" \
  -d '{
    "name": "PCS#1-Updated",
    "parameters": {
      "host": "192.168.1.11"
    }
  }'
```

#### DELETE /api/channels/{id}

删除通道。

```bash
curl -X DELETE http://localhost:6001/api/channels/1001
```

#### GET /api/channels/{id}/status

获取通道运行时状态。

```bash
curl http://localhost:6001/api/channels/1001/status
```

**响应：**
```json
{
  "success": true,
  "data": {
    "id": 1,
    "name": "PCS#1",
    "protocol": "modbus_tcp",
    "connected": true,
    "running": true,
    "last_update": "2026-01-20T08:12:08Z",
    "statistics": {
      "channel_id": 1,
      "connected": true,
      "error_count": 5,
      "last_error": "Connection error message (if any)",
      "protocol_type": "unified"
    }
  }
}
```

#### PUT /api/channels/{id}/enabled

启用/禁用通道。

```bash
curl -X PUT http://localhost:6001/api/channels/1001/enabled \
  -H "Content-Type: application/json" \
  -d '{"enabled": true}'
```

#### POST /api/channels/reload

重新加载所有通道配置。

```bash
curl -X POST http://localhost:6001/api/channels/reload
```

---

### 点位管理

#### GET /api/points

获取所有通道的所有点位。

```bash
curl http://localhost:6001/api/points
```

#### GET /api/channels/{id}/points

获取指定通道的点位。

```bash
curl http://localhost:6001/api/channels/1001/points
```

**响应：**
```json
{
  "success": true,
  "data": {
    "telemetry": [
      {
        "point_id": 1,
        "signal_name": "System_Fault_status",
        "scale": 1.0,
        "offset": 0.0,
        "unit": "",
        "data_type": "uint16",
        "reverse": false,
        "description": "",
        "protocol_mapping": {
          "slave_id": 1,
          "function_code": 3,
          "register_address": 32,
          "data_type": "uint16",
          "byte_order": "AB",
          "bit_position": 0
        }
      }
    ],
    "signal": [...],
    "control": [...],
    "adjustment": [...]
  }
}
```

#### GET /api/channels/{id}/unmapped-points

获取未映射的点位。

```bash
curl http://localhost:6001/api/channels/1001/unmapped-points
```

#### GET /api/channels/{channel_id}/{type}/points/{point_id}

获取指定点位配置。

**路径参数：**
- `type`: 点位类型 (`T`=遥测, `S`=信号, `C`=控制, `A`=调节)

```bash
# 获取遥测点位
curl http://localhost:6001/api/channels/1001/T/points/1

# 获取控制点位
curl http://localhost:6001/api/channels/1001/C/points/10
```

#### POST /api/channels/{channel_id}/{type}/points/{point_id}

创建点位。

```bash
curl -X POST http://localhost:6001/api/channels/1001/T/points/100 \
  -H "Content-Type: application/json" \
  -d '{
    "point_id": 100,
    "signal_name": "New_Telemetry",
    "scale": 0.1,
    "offset": 0,
    "unit": "V",
    "data_type": "uint16"
  }'
```

> **注意**：`point_id` 必须在请求体中提供，且应与 URL 中的值一致。

#### PUT /api/channels/{channel_id}/{type}/points/{point_id}

更新点位。

```bash
curl -X PUT http://localhost:6001/api/channels/1001/T/points/100 \
  -H "Content-Type: application/json" \
  -d '{
    "scale": 0.01,
    "unit": "kV"
  }'
```

#### DELETE /api/channels/{channel_id}/{type}/points/{point_id}

删除点位。

```bash
curl -X DELETE http://localhost:6001/api/channels/1001/T/points/100
```

#### POST /api/channels/{channel_id}/points/batch

批量点位操作。

```bash
curl -X POST http://localhost:6001/api/channels/1001/points/batch \
  -H "Content-Type: application/json" \
  -d '{
    "create": [
      {
        "point_type": "T",
        "point_id": 101,
        "data": {"signal_name": "Point1", "data_type": "uint16", "scale": 1.0, "offset": 0.0}
      }
    ],
    "update": [
      {
        "point_type": "T",
        "point_id": 1,
        "data": {"scale": 0.01}
      }
    ],
    "delete": [
      {"point_type": "T", "point_id": 99}
    ]
  }'
```

**请求体字段：**

| 字段 | 类型 | 说明 |
|------|------|------|
| `create` | array | 要创建的点位列表 |
| `update` | array | 要更新的点位列表 |
| `delete` | array | 要删除的点位列表 |

**每个操作项字段：**

| 字段 | 类型 | 说明 |
|------|------|------|
| `point_type` | string | 点位类型：`T`（遥测）, `S`（信号）, `C`（控制）, `A`（调节） |
| `point_id` | integer | 点位 ID |
| `data` | object | 点位数据（创建/更新时需要，删除时不需要） |
| `force` | boolean | 创建时是否使用 upsert 模式（默认 false） |

---

### 控制操作

#### POST /api/channels/{id}/control

控制通道状态（启动/停止/重启）。

```bash
curl -X POST http://localhost:6001/api/channels/1001/control \
  -H "Content-Type: application/json" \
  -d '{"operation": "start"}'
```

**请求体：**

| 字段 | 类型 | 说明 |
|------|------|------|
| `operation` | string | 操作类型：`start`（启动）, `stop`（停止）, `restart`（重启） |

#### POST /api/channels/{channel_id}/write

仅用于向采集面注入 T/S 仿真值，支持单个和批量。C/A 设备命令会被拒绝；
真实控制必须调用 automation 的实例 action 接口。

```bash
# 单个写入
curl -X POST http://localhost:6001/api/channels/1001/write \
  -H "Content-Type: application/json" \
  -d '{
    "type": "T",
    "id": "10",
    "value": 1.0
  }'

# 批量写入
curl -X POST http://localhost:6001/api/channels/1001/write \
  -H "Content-Type: application/json" \
  -d '{
    "type": "T",
    "points": [
      {"id": "10", "value": 1.0},
      {"id": "20", "value": 50.5}
    ]
  }'
```

---

### 映射管理

#### GET /api/channels/{id}/mappings

获取通道的协议映射。

```bash
curl http://localhost:6001/api/channels/1001/mappings
```

**响应：**
```json
{
  "success": true,
  "data": {
    "telemetry": [
      {
        "point_id": 1,
        "slave_id": 1,
        "function_code": 3,
        "register_address": 100,
        "data_type": "uint16",
        "byte_order": "AB"
      }
    ],
    "control": [...]
  }
}
```

#### PUT /api/channels/{id}/mappings

批量更新映射。

**请求格式：**
```bash
curl -X PUT http://localhost:6001/api/channels/1/mappings \
  -H "Content-Type: application/json" \
  -d '{
    "mappings": [
      {
        "point_id": 1,
        "four_remote": "T",
        "protocol_data": {
          "slave_id": 1,
          "function_code": 3,
          "register_address": 100,
          "data_type": "uint16",
          "byte_order": "AB"
        }
      }
    ]
  }'
```

**字段说明：**
- `mappings` - 映射数组（必填）
- `point_id` - 点位 ID（必填）
- `four_remote` - 四遥类型：`T`(遥测)、`S`(遥信)、`C`(遥控)、`A`(遥调)（必填）
- `protocol_data` - 协议特定数据（必填），包含：
  - `slave_id` - Modbus 从站地址
  - `function_code` - 功能码（3=读保持寄存器，16=写多寄存器）
  - `register_address` - 寄存器地址
  - `data_type` - 数据类型（uint16, int16, uint32, int32, float32）
  - `byte_order` - 字节序（AB, BA, ABCD, DCBA, CDAB, BADC）

**响应：**
```json
{
  "success": true,
  "data": {
    "updated_count": 1,
    "channel_id": 1
  }
}
```

#### GET /api/channels/{channel_id}/{type}/points/{point_id}/mapping

获取单个点位的映射。

```bash
curl http://localhost:6001/api/channels/1001/T/points/1/mapping
```

---

## Automation API (端口 6002)

### 产品管理

#### GET /api/products

获取所有内置产品定义。

```bash
curl http://localhost:6002/api/products
```

**响应：**
```json
{
  "success": true,
  "data": {
    "count": 9,
    "products": [
      {"product_name": "Station", "parent_name": null},
      {"product_name": "ESS", "parent_name": "Station"},
      {"product_name": "PCS", "parent_name": "ESS"},
      {"product_name": "Battery", "parent_name": "ESS"}
    ]
  }
}
```

#### GET /api/products/{product_name}/points

获取产品的点位定义。

```bash
curl http://localhost:6002/api/products/PCS/points
```

> **注意**：产品名称区分大小写，必须与 `/api/products` 返回的 `product_name` 完全匹配。

**响应：**
```json
{
  "success": true,
  "data": {
    "product": {
      "product_name": "PCS",
      "parent_name": "ESS",
      "measurements": [
        {"measurement_id": 1, "name": "Total Power", "unit": "kw", "description": null},
        {"measurement_id": 2, "name": "DC Power", "unit": "kw", "description": null}
      ],
      "actions": [
        {"action_id": 1, "name": "Start", "unit": null, "description": null},
        {"action_id": 3, "name": "Power Set", "unit": "kw", "description": null}
      ],
      "properties": [
        {"property_id": 1, "name": "Max Power", "unit": "kw", "description": null}
      ]
    }
  }
}
```

---

### 实例管理

#### GET /api/instances

获取所有实例（分页）。

```bash
curl http://localhost:6002/api/instances
```

**响应：**
```json
{
  "success": true,
  "data": {
    "list": [
      {
        "instance_id": 1,
        "instance_name": "pcs_01",
        "product_name": "PCS",
        "properties": {"rated_power": 500.0}
      }
    ],
    "page": 1,
    "page_size": 20,
    "total": 1
  }
}
```

#### GET /api/instances/list

获取实例列表（精简）。

```bash
curl "http://localhost:6002/api/instances/list?product=PCS"
```

**查询参数：**

| 参数 | 类型 | 说明 |
|------|------|------|
| `product` | string | 按产品类型筛选 |

#### GET /api/instances/search

搜索实例。

```bash
curl "http://localhost:6002/api/instances/search?q=pcs&product=PCS"
```

#### POST /api/instances

创建实例。

```bash
curl -X POST http://localhost:6002/api/instances \
  -H "Content-Type: application/json" \
  -d '{
    "instance_name": "pcs_01",
    "product_name": "PCS",
    "display_name": "PCS #1",
    "properties": {
      "rated_power": 500.0,
      "manufacturer": "Sungrow"
    }
  }'
```

> **注意**：`product_name` 区分大小写，必须与 `/api/products` 返回的 `product_name` 完全匹配（如 `PCS`、`Battery`、`ESS`）。

#### GET /api/instances/{id}

获取实例详情。

```bash
curl http://localhost:6002/api/instances/1
```

**响应：**
```json
{
  "success": true,
  "data": {
    "instance": {
      "instance_id": 1,
      "instance_name": "pcs_01",
      "product_name": "PCS",
      "properties": {"rated_power": 500.0, "manufacturer": "Sungrow"},
      "measurement_mappings": {"1": "inst:1:M:1"},
      "action_mappings": {"1": "inst:1:A:1"}
    }
  }
}
```

#### PUT /api/instances/{id}

更新实例。

```bash
curl -X PUT http://localhost:6002/api/instances/1 \
  -H "Content-Type: application/json" \
  -d '{
    "display_name": "PCS #1 Updated",
    "properties": {
      "rated_power": 600.0
    }
  }'
```

#### DELETE /api/instances/{id}

删除实例。

```bash
curl -X DELETE http://localhost:6002/api/instances/1
```

#### GET /api/instances/{id}/data

获取实例运行时数据。

```bash
curl http://localhost:6002/api/instances/1/data
```

**响应：**
```json
{
  "success": true,
  "data": {
    "measurements": {
      "1": "85.5",
      "2": "220.0"
    },
    "actions": {
      "1": "100.0"
    }
  }
}
```

#### GET /api/instances/{id}/points

获取实例点位定义。

```bash
curl http://localhost:6002/api/instances/1/points
```

#### POST /api/instances/{id}/measurement

设置测量点值（调试用）。

```bash
curl -X POST http://localhost:6002/api/instances/1/measurement \
  -H "Content-Type: application/json" \
  -d '{
    "point_id": "1",
    "value": 90.0
  }'
```

#### POST /api/instances/{id}/action

执行动作点。

```bash
curl -X POST http://localhost:6002/api/instances/1/action \
  -H "Content-Type: application/json" \
  -d '{
    "point_id": "1",
    "value": 150.0
  }'
```

#### POST /api/instances/{id}/sync

同步单个实例数据。

```bash
curl -X POST http://localhost:6002/api/instances/1/sync \
  -H "Content-Type: application/json" \
  -d '{}'
```

> **注意**：此端点需要 `Content-Type: application/json` 头和请求体（可以是空对象 `{}`）。

#### POST /api/instances/sync/all

同步所有实例数据。

```bash
curl -X POST http://localhost:6002/api/instances/sync/all
```

#### POST /api/instances/reload

从数据库重新加载实例。

```bash
curl -X POST http://localhost:6002/api/instances/reload
```

#### GET /api/instances/export

导出实例配置（云同步）。

```bash
curl http://localhost:6002/api/instances/export
```

---

### 路由管理

#### GET /api/routing

获取所有路由配置。

```bash
curl http://localhost:6002/api/routing
```

**响应：**
```json
{
  "success": true,
  "data": {
    "measurement_routing": [
      {
        "channel_id": 1001,
        "channel_type": "T",
        "channel_point_id": 1,
        "instance_id": 1,
        "measurement_id": 1,
        "enabled": true
      }
    ],
    "action_routing": [
      {
        "instance_id": 1,
        "action_id": 1,
        "channel_id": 1001,
        "channel_type": "C",
        "channel_point_id": 10,
        "enabled": true
      }
    ],
    "total": {
      "measurement": 1,
      "action": 1
    }
  }
}
```

#### DELETE /api/routing

删除所有路由。

```bash
curl -X DELETE http://localhost:6002/api/routing
```

#### GET /api/routing/table

获取运行时路由表（Redis）。

```bash
curl http://localhost:6002/api/routing/table
```

#### GET /api/routing/by-channel/{channel_id}

按通道获取路由。

```bash
curl http://localhost:6002/api/routing/by-channel/1001
```

#### DELETE /api/routing/channels/{channel_id}

删除通道相关路由。

```bash
curl -X DELETE http://localhost:6002/api/routing/channels/1001
```

#### DELETE /api/routing/instances/{id}

删除实例相关路由。

```bash
curl -X DELETE http://localhost:6002/api/routing/instances/1
```

#### GET /api/instances/{id}/routing

获取实例路由配置。

```bash
curl http://localhost:6002/api/instances/1/routing
```

#### POST /api/instances/{id}/routing

创建单个点位路由。

```bash
# 创建测量点路由
curl -X POST http://localhost:6002/api/instances/1/routing \
  -H "Content-Type: application/json" \
  -d '{
    "point_type": "M",
    "point_id": 1,
    "channel_id": 1001,
    "four_remote": "T",
    "channel_point_id": 1
  }'

# 创建动作点路由
curl -X POST http://localhost:6002/api/instances/1/routing \
  -H "Content-Type: application/json" \
  -d '{
    "point_type": "A",
    "point_id": 1,
    "channel_id": 1001,
    "four_remote": "C",
    "channel_point_id": 10
  }'
```

**请求字段：**

| 字段 | 类型 | 说明 |
|------|------|------|
| `point_type` | string | 点位类型：`M`（测量）或 `A`（动作） |
| `point_id` | number | 测量点 ID 或动作点 ID |
| `channel_id` | number | 通道 ID（可选，null 表示解绑） |
| `four_remote` | string | 四遥类型：`T`/`S`（测量）或 `C`/`A`（动作） |
| `channel_point_id` | number | 通道点位 ID（可选） |

#### PUT /api/instances/{id}/routing

更新实例路由。

```bash
curl -X PUT http://localhost:6002/api/instances/1/routing \
  -H "Content-Type: application/json" \
  -d '{ ... }'
```

#### DELETE /api/instances/{id}/routing

删除实例路由。

```bash
curl -X DELETE http://localhost:6002/api/instances/1/routing
```

#### POST /api/instances/{id}/routing/validate

验证实例路由。

```bash
curl -X POST http://localhost:6002/api/instances/1/routing/validate \
  -H "Content-Type: application/json" \
  -d '[]'
```

> **注意**：请求体是待验证路由的数组，空数组 `[]` 表示验证当前配置。

#### 单点路由操作

```bash
# 获取测量点信息
GET /api/instances/{id}/measurements/{point_id}

# 设置测量点路由
PUT /api/instances/{id}/measurements/{point_id}/routing

# 删除测量点路由
DELETE /api/instances/{id}/measurements/{point_id}/routing

# 启用/禁用测量点路由
PATCH /api/instances/{id}/measurements/{point_id}/routing

# 动作点类似
GET /api/instances/{id}/actions/{point_id}
PUT /api/instances/{id}/actions/{point_id}/routing
DELETE /api/instances/{id}/actions/{point_id}/routing
PATCH /api/instances/{id}/actions/{point_id}/routing
```

---

### 规则管理

#### GET /api/rules

获取规则列表（分页）。

```bash
curl "http://localhost:6002/api/rules?page=1&page_size=20"
```

**响应：**
```json
{
  "success": true,
  "data": {
    "list": [
      {"id": 1, "name": "SOC Protection", "enabled": true, "description": "..."}
    ],
    "total": 5,
    "page": 1,
    "page_size": 20,
    "total_pages": 1,
    "has_next": false,
    "has_previous": false
  }
}
```

#### POST /api/rules

创建规则。

```bash
curl -X POST http://localhost:6002/api/rules \
  -H "Content-Type: application/json" \
  -d '{
    "name": "Battery Protection",
    "description": "Protect battery when SOC is low"
  }'
```

#### GET /api/rules/{id}

获取规则详情（包含 Vue Flow 数据）。

```bash
curl http://localhost:6002/api/rules/1
```

#### PUT /api/rules/{id}

更新规则（部分更新）。

```bash
curl -X PUT http://localhost:6002/api/rules/1 \
  -H "Content-Type: application/json" \
  -d '{
    "name": "Battery Protection v2",
    "enabled": true,
    "priority": 10,
    "cooldown_ms": 5000,
    "flow_json": { ... }
  }'
```

**请求体字段（均可选）：**

| 字段 | 类型 | 说明 |
|------|------|------|
| `name` | string | 规则名称 |
| `description` | string | 描述 |
| `enabled` | bool | 是否启用 |
| `priority` | int | 执行优先级 |
| `cooldown_ms` | int | 冷却时间（毫秒） |
| `flow_json` | object | Vue Flow 完整数据 |

#### DELETE /api/rules/{id}

删除规则。

```bash
curl -X DELETE http://localhost:6002/api/rules/1
```

#### POST /api/rules/{id}/enable

启用规则。

```bash
curl -X POST http://localhost:6002/api/rules/1/enable
```

#### POST /api/rules/{id}/disable

禁用规则。

```bash
curl -X POST http://localhost:6002/api/rules/1/disable
```

#### POST /api/rules/{id}/execute

手动执行规则。

```bash
curl -X POST http://localhost:6002/api/rules/1/execute
```

**响应：**
```json
{
  "success": true,
  "data": {
    "result": "executed",
    "rule_id": "1",
    "execution_id": "manual-abc123",
    "success": true,
    "actions_executed": [
      {"target_type": "instance", "target_id": "pcs_01", "point_id": 1, "value": 100.0, "success": true}
    ],
    "execution_path": ["start", "switch-soc", "action-high", "end"],
    "timestamp": "2024-01-15T10:30:00Z"
  }
}
```

#### GET /api/rules/{id}/variables

获取规则变量定义（用于监控）。

```bash
curl http://localhost:6002/api/rules/1/variables
```

---

### 调度器控制

#### GET /api/scheduler/status

获取调度器状态。

```bash
curl http://localhost:6002/api/scheduler/status
```

**响应：**
```json
{
  "success": true,
  "data": {
    "running": true,
    "total_rules": 10,
    "enabled_rules": 5,
    "tick_interval_ms": 100
  }
}
```

#### POST /api/scheduler/reload

重新加载调度器规则。

```bash
curl -X POST http://localhost:6002/api/scheduler/reload
```

---

## 管理员 API

以下端点在 io 和 automation 都可用：

#### GET /api/admin/logs/level

获取当前日志级别。

```bash
curl http://localhost:6001/api/admin/logs/level
curl http://localhost:6002/api/admin/logs/level
```

**响应：**
```json
{
  "level": "info"
}
```

#### POST /api/admin/logs/level

设置日志级别。

```bash
curl -X POST http://localhost:6001/api/admin/logs/level \
  -H "Content-Type: application/json" \
  -d '{"level": "debug"}'
```

**支持的级别：** `trace`, `debug`, `info`, `warn`, `error`

**过滤器语法：**
```json
{"level": "info,io::protocol=debug,sqlx=warn"}
```

---

## WebSocket API

### 规则监控 WebSocket

**端点：** `ws://localhost:6002/ws/rules/{rule_id}/monitor`

实时监控规则变量值。

**连接：**
```javascript
const ws = new WebSocket('ws://localhost:6002/ws/rules/1/monitor');

ws.onmessage = (event) => {
  const data = JSON.parse(event.data);
  console.log('Variables:', data.variables);
};
```

**消息格式：**
```json
{
  "type": "variable_update",
  "rule_id": 1,
  "timestamp": "2024-01-15T10:30:00Z",
  "variables": {
    "soc": 85.5,
    "power": 100.0
  }
}
```

---

## 错误处理

### HTTP 状态码

| 状态码 | 说明 |
|--------|------|
| 200 | 成功 |
| 400 | 请求参数错误 |
| 404 | 资源不存在 |
| 409 | 资源冲突（如 ID 重复） |
| 500 | 服务器内部错误 |

### 错误代码

| 代码 | 说明 |
|------|------|
| `CHANNEL_NOT_FOUND` | 通道不存在 |
| `INSTANCE_NOT_FOUND` | 实例不存在 |
| `POINT_NOT_FOUND` | 点位不存在 |
| `RULE_NOT_FOUND` | 规则不存在 |
| `INVALID_PARAMETER` | 参数无效 |
| `DUPLICATE_ID` | ID 重复 |
| `DATABASE_ERROR` | 数据库错误 |
| `REDIS_ERROR` | Redis 连接错误 |
| `VALIDATION_ERROR` | 验证失败 |

### 错误响应示例

**io 错误响应：**
```json
{
  "success": false,
  "error": {
    "code": 404,
    "message": "Channel 9999 not found"
  }
}
```

**automation 错误响应（包含建议）：**
```json
{
  "success": false,
  "error": {
    "code": 404,
    "message": "Instance not found: 9999",
    "details": "error_code: AUTOMATION_INSTANCE_NOT_FOUND, category: NotFound, retryable: false",
    "suggestion": "Use GET /api/instances to list available instances, or create a new one with POST /api/instances"
  }
}
```

**字段说明：**
| 字段 | 类型 | 说明 |
|------|------|------|
| `code` | int | HTTP 状态码 |
| `message` | string | 错误消息（用户可读） |
| `suggestion` | string? | 修复建议（可选，提供具体操作指引） |
| `details` | object? | 详细信息（可选，包含相关数据） |
