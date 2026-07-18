---
title: "连接设备"
description: "配置通道、选择协议并将设备点映射到实例"
updated: 2026-07-10
---

# 连接设备

设备作为通信服务中的**通道**连接到 Aether（io，端口 6001）。通道是一个设备连接：协议、协议所需的传输参数以及描述设备公开内容的点表。然后，通道点映射到设备**实例** — 规则和仪表板所针对的逻辑事物模型（请参阅[数据模型](/concepts/data-model)）。

## 通道

通道在 `config/io/io.yaml` 中编写，并由 `aether sync` 加载到 SQLite 中；服务从不直接读取 YAML。附带模板 (`config.template/io/io.yaml`) 的精简示例，显示一个 TCP 和一个串行连接：
```yaml
channels:
  - id: 1
    name: "PCS#1"
    protocol: "modbus_tcp"
    enabled: true
    parameters:
      host: "192.168.1.10"
      port: 502
      connect_timeout_ms: 3000
      read_timeout_ms: 3000

  - id: 3
    name: "GENSET#1"
    protocol: "modbus_rtu"
    enabled: true
    parameters:
      device: "/dev/ttyS4"
      baud_rate: 9600
      data_bits: 8
      stop_bits: 1
      parity: "N"
```

`parameters` 块是特定于协议的：Modbus TCP 需要主机和端口，Modbus RTU 需要串行设备和线路设置，MQTT 需要代理 URL 和订阅主题，等等。协议名称在匹配之前进行规范化（`services/io/src/utils.rs` 中的 `normalize_protocol_name`），因此 `modbus-tcp`、`ModbusTCP` 和 `modbus_tcp` 全部解析为同一协议。

也可以在运行时创建通道，而无需接触 YAML：
```bash
aether channels create --name "PCS#2" --protocol modbus_tcp \
  --params '{"host": "192.168.1.11", "port": 502}'
```

它在 io 上调用 `POST /api/channels`。 `aether channels list`、`update`、`delete`、`enable` 和 `disable` 涵盖了生命周期的其余部分。

每个通道都带有一个按四种点类型划分的点表：遥测（T，模拟测量）、信号（S，数字状态）、控制（C，数字命令）和调整（A，模拟设定点）。点通过 `aether channels points list|add|update|delete` 进行管理，或在通道 YAML 旁边编写为 CSV 表，并由 `aether sync` 获取。

## 协议可用性

io 支持 14 种协议，但大多数都支持编译时 Cargo 功能 (`services/io/Cargo.toml`)，因此给定的二进制文件通常只包含一个子集。默认功能集编译 Modbus、GPIO、Aether-485、IEC 61850 和 CAN。

| 协议 | 默认编译 | 平台说明 |
|----------|--------------------:|----------------|
| Modbus TCP/RTU (`modbus`) | 是 | |
| IEC 60870-5-104 (`iec104`) | 否 | |
| IEC 61850 MMS (`iec61850`) | 是 | |
| OPC UA (`opcua`) | 否 | 可选功能；目前仅限于匿名 `SecurityPolicy::None` 会话。 |
| MQTT (`mqtt`) | 无 | 事件驱动的 JSON 负载；在 `json-mapping` |
| HTTP (`http`) | 无 | 轮询和 Webhook 模式中启用拉取；启用 `json-mapping` |
| DL/T 645-2007 (`dl645`) | 否 | 通过串行或 TCP 的智能电表 |
| CAN (`can`) / J1939 (`j1939`) | CAN 是，J1939否 | 仅限 Linux； `j1939` 表示 `can` |
| GPIO (`gpio`) | 是 | 仅限 Linux |
| BLE GATT (`ble`) | 否 | |
| Zigbee (`zigbee`) | 否 | 通过 TCP 网关 |
| 有问题 (`matter`) | 否 | |
| Aether-485 (`aether_485`) | 是 | 专用 RS-485协议 |
| 虚拟 | 始终 | 无功能门；用于测试和模拟 |

通道工厂 (`services/io/src/protocols/gateway/factory.rs`) 中另外有两个协议由操作系统控制：CAN 和 GPIO 仅在 Linux 上编译，因此无论功能如何，它们都不会存在于 macOS 版本中。虚拟是一个根本没有门的协议 - 它始终可用，并且是在涉及实际硬件之前尝试规则和映射的正确的第一个目标。

经验法则：**如果创建通道失败，请首先检查功能门。** 工厂的错误是字面上的 - `Unsupported protocol: {name}. Check if the required feature is enabled.` - 原因几乎总是未编译的协议，而不是配置拼写错误。

## 映射点实例

通道点是协议风格的（通道2上的寄存器62001）；规则和仪表板需要模型风格的值（电池组充电状态）。桥接器是一个实例加路由：

1. **定义实例**。实例将设备绑定到 `config/automation/instances.yaml` 中的产品模板。默认分布有意从空开始；可选示例位于 `packs/energy/examples/config/automation/instances.yaml` 下：

```yaml
   instances:
     pcs_01:
       product_name: PCS
       name: "PCS #1"
       properties:
         rated_power: 500.0
         rated_voltage: 380.0
   ```

产品定义了实例有哪些测量点和动作点；属性填充模板的静态值。

2. **将通道点映射到实例点。** 路由将通道点连接到实例点：遥测和信号点馈送实例测量点（M，`route:c2m` 表），实例操作点 (A) 驱动通道控制和调整点 (`route:m2c`)。可以通过 CLI 创建条目：

```bash
   aether routing create 1 --point-type M --point-id 9 \
     --channel-id 1 --four-remote T --channel-point-id 101
   ```

它会自动调用 `POST /api/instances/{id}/routing`，或使用 `aether routing batch` 批量调用。

3. **如果实例或路由是在 YAML 中编写的，则运行 `aether sync`**。同步操作会验证配置并将其写入 SQLite，服务随后从中加载配置；`--dry-run` 可以在不写入的情况下完成验证。

4. **验证。** 两次检查，桥的每一侧一次：

```bash
   aether channels unmapped-points 1     # channel side
   aether routing list --channel 1       # instance side
   ```

第一个（io 上的 `GET /api/channels/{id}/unmapped-points`）列出了在协议映射仍为空的通道上声明的点 — io 点无法轮询，因为它们尚未连接到协议地址。第二个显示了接触通道的每个路由条目，因此忘记的实例绑定会以缺失行的形式出现。

## 验证连接

首先检查通道状态：
```bash
aether channels status 1
```

这会调用 `GET /api/channels/{id}/status` 并返回 `connected`、`running`、`last_update` 和累积统计信息（读/写计数、平均响应时间）。请注意，`connected` 会检查传输状态和数据新鲜度：保持 TCP 连接但在 90 秒内未收到数据的通道会报告 `false`。

然后观察实时值。在通道端，`GET /api/channels/{channel_id}/{T|S|C|A}/{point_id}` 返回当前值及其时间戳和原始协议值。要直接检查，请打开共享内存 REPL：
```bash
aether shm
```

如果通道点更新但实例点没有更新，则路由条目丢失或错误。 SHM 是权威的实时视图，因此不需要运行外部数据库来进行此检查。

离线情况如下：`aether channels status` 报告 `connected: false`，通道运行状况 SHM 条目变为离线状态，并且点值停止更新 — 它们的时间戳变得过时。 “从未”获取过的点是共享内存中的 NaN 标记，而不是零；请参阅[数据模型](/concepts/data-model) 了解为什么不可用性是一流的值。对于整个系统通行证 — 服务启动、SQLite 可读、连接共享内存 — 运行 `aether doctor`。

## 相关页面

- [数据模型](/concepts/data-model) — 产品、实例和四种点类型
- [系统架构](/concepts/architecture) — io 和自动化所处的位置以及数据如何在之间流动它们
- [编写规则](/guides/writing-rules) — 将映射点应用于控制逻辑
