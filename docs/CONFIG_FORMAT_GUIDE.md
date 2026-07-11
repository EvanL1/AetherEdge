# AetherEMS 配置格式指南

本文档说明 AetherEMS 使用的各种配置文件格式及其用途。

## 概述

AetherEMS 使用以下配置格式：

| 格式 | 用途 | 位置 |
|------|------|------|
| YAML | 服务配置、实例定义、规则 | `config/*.yaml` |
| CSV | 点位定义、Modbus 映射 | `config/io/*/` |
| SQLite | 运行时数据存储 | `data/aether.db` |

## 目录结构

```
config/
├── global.yaml                    # 全局配置（日志、共享内存）
├── io/
│   ├── io.yaml               # 通道列表定义
│   └── {channel_id}/             # 每个通道的配置
│       ├── telemetry.csv         # 遥测点位定义
│       ├── signal.csv            # 信号点位定义
│       ├── control.csv           # 控制点位定义
│       ├── adjustment.csv        # 调节点位定义
│       └── mapping/              # 协议映射
│           ├── telemetry_mapping.csv
│           ├── signal_mapping.csv
│           ├── control_mapping.csv
│           └── adjustment_mapping.csv
└── automation/
    ├── automation.yaml               # 模型服务配置
    ├── instances.yaml            # 设备实例定义
    └── rules/                    # 规则定义
        └── *.yaml / *.json
```

---

## YAML 配置

### global.yaml - 全局配置

```yaml
# API 服务配置
api:
  host: "127.0.0.1"          # 内部进程 API 仅本机访问；远程请求走 aether-api

# 日志配置
logging:
  level: "info"              # trace, debug, info, warn, error
  dir: "logs"
  rotation:
    strategy: "daily"        # daily, size
    max_size_mb: 100
    max_files: 7

# 规则引擎配置
rules:
  tick_ms: 100               # 调度扫描间隔（毫秒）

# 共享内存配置（高级，通常无需修改）
shared_memory:
  max_instances: 65536
  max_points_per_instance: 65536
  max_channels: 65536
  max_points_per_channel: 65536
```

### io.yaml - 通道配置

```yaml
channels:
  - id: 1                          # 通道 ID（必须唯一）
    name: "PCS#1"                  # 显示名称
    description: "变流器 #1"        # 描述
    protocol: "modbus_tcp"         # 协议类型（见下方协议列表）
    enabled: true                  # 是否启用
    parameters:                    # 协议特定参数
      host: "192.168.1.10"
      port: 502
      connect_timeout_ms: 3000
      read_timeout_ms: 3000
    logging:
      enabled: true
      level: "info"
```

**支持的协议类型：**

| 协议 | 说明 | 必需参数 |
|------|------|---------|
| `modbus_tcp` | Modbus TCP | `host`, `port` |
| `modbus_rtu` | Modbus RTU（串口） | `device`, `baud_rate`, `parity` |
| `iec104` | IEC 60870-5-104 | `host`, `port` |
| `opcua` | OPC UA（当前仅匿名、无消息安全模式） | `endpoint_url` |
| `di_do` | GPIO 数字 I/O | `driver`, `gpio_base_path` |
| `dl645` | DL/T 645-2007 电表 | `device`, `baud_rate`, `address` |
| `mqtt` | MQTT 发布/订阅 | `broker` |
| `http` | HTTP 轮询/Webhook | `url`（轮询）或 `listen_path`（Webhook） |
| `can` | CAN 总线 | `device` |
| `j1939` | J1939 车载网络（基于 CAN） | `device`, `pgn_list` |
| `virtual` | 虚拟协议（测试/仿真） | `interval_ms` |

#### MQTT 协议连接配置

| 参数 | 类型 | 必需 | 默认值 | 说明 |
|------|------|------|--------|------|
| `broker` | string | ✓ | `tcp://localhost:1883` | Broker 地址，格式：`tcp://host:port` 或 `ssl://host:port` |
| `client_id` | string | ✗ | 自动生成 UUID | 客户端标识 |
| `username` | string | ✗ | - | 认证用户名 |
| `password` | string | ✗ | - | 认证密码 |
| `subscriptions` | array | ✗ | [] | 订阅主题列表 |
| `subscriptions[].topic` | string | ✓ | - | MQTT 主题，支持 `+`/`#` 通配符 |
| `subscriptions[].qos` | u8 | ✗ | 1 | QoS 等级 (0/1/2) |
| `keep_alive_secs` | u64 | ✗ | 30 | 心跳间隔（秒） |
| `connect_timeout_ms` | u64 | ✗ | 5000 | 连接超时（毫秒） |
| `reconnect_delay_ms` | u64 | ✗ | 5000 | 重连延迟（毫秒） |

**MQTT 主题通配符说明：**
- `+` 单层通配符：`device/+/telemetry` 匹配 `device/001/telemetry`、`device/002/telemetry`
- `#` 多层通配符：`device/#` 匹配 `device/001/telemetry`、`device/001/status/online`

#### HTTP 协议连接配置

| 参数 | 类型 | 必需 | 默认值 | 说明 |
|------|------|------|--------|------|
| `mode` | enum | ✗ | `polling` | 模式：`polling`（轮询）/ `webhook`（被动接收） |
| `url` | string | ✓* | - | 目标 URL（轮询模式必需） |
| `method` | enum | ✗ | `GET` | HTTP 方法：`GET`/`POST`/`PUT` |
| `headers` | object | ✗ | {} | 请求头，如 `Authorization` |
| `body` | string | ✗ | - | 请求体（POST/PUT） |
| `interval_ms` | u64 | ✗ | 5000 | 轮询间隔（毫秒） |
| `timeout_ms` | u64 | ✗ | 3000 | 请求超时（毫秒） |
| `listen_path` | string | ✓* | - | 监听路径（Webhook 模式必需） |
| `auth_token` | string | ✗ | - | Webhook 认证 token |
| `max_retries` | u32 | ✗ | 3 | 失败重试次数 |

**HTTP 模式选择指南：**
- **polling**：主动从外部 API 拉取数据，适用于传统 HTTP API
- **webhook**：被动接收外部推送，适用于支持事件通知的系统

### instances.yaml - 设备实例

```yaml
instances:
  pcs_01:                          # 实例 ID（必须唯一）
    product_name: pcs              # 产品类型（对应 product 定义）
    name: "PCS #1"                 # 显示名称
    properties:                    # 自定义属性
      rated_power: 500.0           # kW
      rated_voltage: 380.0         # V
      manufacturer: "Sungrow"
      model: "SC500HV"
      installation_date: "2024-01-01"
```

---

## CSV 配置

### 点位定义 CSV

**文件：** `telemetry.csv`, `signal.csv`, `control.csv`, `adjustment.csv`

```csv
point_id,signal_name,scale,offset,unit,reverse,data_type
1,System_Fault_status,1,0,,0,uint16
2,Voltage_A,0.1,0,V,0,uint16
3,Power_kW,0.01,-1000,kW,0,int32
```

| 列名 | 类型 | 说明 |
|------|------|------|
| `point_id` | int | 点位 ID（通道内唯一） |
| `signal_name` | string | 点位名称 |
| `scale` | float | 缩放系数（原始值 × scale + offset = 工程值） |
| `offset` | float | 偏移量 |
| `unit` | string | 工程单位（可选） |
| `reverse` | int | 是否反转（0=否，1=是） |
| `data_type` | string | 数据类型（见下方类型列表） |

**支持的数据类型：**
- `bool` - 布尔值
- `uint16` - 无符号 16 位整数
- `int16` - 有符号 16 位整数
- `uint32` - 无符号 32 位整数
- `int32` - 有符号 32 位整数
- `float32` - 32 位浮点数
- `float64` - 64 位浮点数

### Modbus 映射 CSV

**文件：** `mapping/telemetry_mapping.csv`, `mapping/control_mapping.csv` 等

```csv
point_id,slave_id,function_code,register_address,data_type,byte_order,bit_position
1,1,3,32,uint16,AB,
2,1,3,100,uint32,ABCD,
3,1,3,200,bool,,0
```

| 列名 | 类型 | 说明 |
|------|------|------|
| `point_id` | int | 对应点位定义中的 point_id |
| `slave_id` | int | Modbus 从站地址（1-247） |
| `function_code` | int | Modbus 功能码（见下方） |
| `register_address` | int | 寄存器地址（0-based） |
| `data_type` | string | 寄存器数据类型 |
| `byte_order` | string | 字节序（见下方） |
| `bit_position` | int | 位位置（仅用于 bool 类型，0-15） |

**Modbus 功能码：**

| 代码 | 说明 | 用途 |
|------|------|------|
| `1` | Read Coils | 读取线圈状态 |
| `2` | Read Discrete Inputs | 读取离散输入 |
| `3` | Read Holding Registers | 读取保持寄存器（最常用） |
| `4` | Read Input Registers | 读取输入寄存器 |
| `5` | Write Single Coil | 写单个线圈 |
| `6` | Write Single Register | 写单个寄存器 |
| `15` | Write Multiple Coils | 写多个线圈 |
| `16` | Write Multiple Registers | 写多个寄存器（最常用） |

**字节序说明：**

| 值 | 说明 | 字节顺序 |
|-----|------|---------|
| `AB` | Big-Endian（默认） | 高字节在前 |
| `BA` | Little-Endian | 低字节在前 |
| `ABCD` | Big-Endian 32位 | 最高字节在前 |
| `DCBA` | Little-Endian 32位 | 最低字节在前 |
| `CDAB` | Mid-Big 32位 | 交换字顺序 |
| `BADC` | Mid-Little 32位 | 交换字顺序 |

---

## SQLite 数据库

**文件：** `data/aether.db`

这是运行时数据库，由 `aether sync` 命令从 YAML/CSV 同步生成。

**主要表：**

| 表名 | 说明 |
|------|------|
| `channels` | 通道配置 |
| `points` | 点位定义 |
| `point_mappings` | 协议映射 |
| `instances` | 设备实例 |
| `measurement_routing` | C2M 上行路由 |
| `action_routing` | M2C 下行路由 |
| `rules` | 规则定义 |

**注意：** 不要直接编辑 SQLite 文件，始终通过 YAML/CSV 配置 + `aether sync` 来管理。

---

## 配置加载优先级

1. **命令行参数** - 最高优先级
2. **环境变量** - `AETHER_*` 前缀
3. **配置文件** - YAML/CSV
4. **默认值** - 程序内置

---

## 常见问题

### Q: CSV 文件为什么没有注释？

CSV 标准不支持注释。如需添加说明，请：
1. 在同目录创建 `README.md` 文件
2. 参考本文档的字段说明

### Q: 如何验证配置是否正确？

```bash
# 验证所有配置（不实际同步）
aether sync --check

# 查看详细验证结果
aether sync --check --detailed
```

### Q: 修改配置后如何生效？

```bash
# 同步配置到数据库
aether sync

# 验证系统状态
aether doctor
```

### Q: 如何备份配置？

```bash
# 从数据库导出为 YAML/CSV
aether export --output backup/

# 整个 config 目录也可以用 git 管理
```
