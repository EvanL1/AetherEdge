---
title: "系统架构"
description: "通过共享内存、每个消费者 UDS 事件、SQLite、HTTP 和 MQTT 进行通信的隔离边缘服务"
updated: 2026-07-11
---

# 系统架构

Aether 是一个人工智能原生工业边缘网关，围绕共享内存热路径构建为六个独立监督的 Rust 服务。设备由 aether-io 轮询，值落在 SHM 中，每个实时消费者从 SQLite 解析其逻辑点并直接读取段。可选扩展可以将 SHM 镜像到外部存储中，但没有默认服务读取该镜像。生成的应用程序和下游产品接口是可选的客户端；它们不是边缘内核的架构边界。
```
  Devices ─────► aether-io(:6001) ───── authoritative SHM live state
   protocols       sole T/S writer       │          │
                         ▲               │          └─ optional Redis StateMirror
                         │ SHM + UDS      │
                         └──── aether-automation(:6002) (rules / C/A command owner)
                                          │
                 ┌──────────────┬──────────┼──────────────┐
                 ▼              ▼          ▼              ▼
          aether-alarm(:6007) aether-history(:6004) aether-api(:6005) aether-uplink(:6006)
          SHM + own event  SHM sampling  SHM + own event  SHM sampling
          bitmap / UDS     SQLite history bitmap / UDS    durable outbox
                 │              │          │              │
                 └─ local HTTP ─┘          └─ WebSocket   └─ MQTT cloud

  SQLite aether.db ───── configuration/discovery for every process
  SQLite history.db ──── default local historian store
  PostgreSQL/TimescaleDB ─ optional history adapter
```

在参考 Docker 部署 (`docker-compose.yml`) 中，每个容器都使用 `network_mode: host` 运行。 `/dev/shm` 以读/写方式安装在 `/shm/rtdb` 上，用于主段、每个消费者订阅位图和跨容器 UDS 套接字。五个内部进程API绑定到`127.0.0.1`；仅受 JWT 保护的 `aether-api` 网关可远程访问。设备操作也会通过自动化再次进行身份验证，因此环回标头无法冒充操作员。没有核心服务挂载 Redis 套接字或等待外部数据库。

## 服务

默认端口在 `libs/aether-model/src/service_ports.rs` 中定义一次，并在配置不覆盖它们时用作备用端口。

| 服务 | 端口 | 角色 |
|---------|------|------|
| aether-io | 6001 | 通信服务 —工业协议驱动程序（14 种协议：Modbus、IEC 104、IEC 61850、OPC UA、MQTT、HTTP、DL/T 645、CAN/J1939、GPIO、BLE、Zigbee、Matter、Aether-485、Virtual）、通道管理、遥测到共享的独家编写者内存 |
| aether-automation | 6002 | 模型服务——产品定义、设备实例、规则引擎执行 |
| aether-history | 6004 | 历史数据服务——默认嵌入SQLite；可选 PostgreSQL / TimescaleDB，通过 `postgres-storage` |
| aether-api | 6005 | API 网关 — 统一 REST API、WebSocket 推送到浏览器、JWT 身份验证 |
| aether-uplink | 6006 | 网络服务 — 用于云上行链路的 MQTT 代理集成、TLS 证书管理 |
| aether-alarm | 6007 | 报警服务 - 报警规则、报警事件、通知 |
| aether-redis | 6379 | 单独构建的 Redis `StateMirror` 扩展（`redis` 配置文件）的可选基础架构 |
| TimescaleDB | 5432 | 用于历史数据的可选时间序列数据库，运行时配置通过aether-history |

## 可选数据处理应用程序

Aether 数据处理添加了行业中立的应用程序功能，而无需更改默认的六项服务。它从只读实时状态、历史查询、请求上下文和行业包绑定中组装有界观察框架，然后调用已配置的本地或远程 `DataProcessor`。
```text
authenticated HTTP
              │ typed processing query (non-idempotent)
              ▼
  DataProcessingApplication
       ├─ LiveState (read-only)
       ├─ HistoryQuery
       └─ task/context inputs
              │ complete ProcessingFrame
              ▼
         DataProcessor
              │ validated, expiring result
              ▼
       direct DerivedData response
```

处理器故意位于每个 Aether 数据权限之外：它无法附加到 SHM、读取历史数据库或通过回调内部服务 API 来解析 `plant_id`。默认运行时不需要处理器。部署可以在进程中组成功能或隔离处理器边车背后的模型和网络依赖项。版本 1 在选择加入 `aether-api` 中托管 `DataProcessingApplication`；未实现独立的 `aether-data-processor`、缓存、CLI/MCP 绑定或调度程序。进程名称保留为未来的编排边界保留。

默认的 SQLite 读取是一个调用时快照，而不是双时态历史剪切。 `as_of` 过滤事件时间，而后期摄取、物理源历元和模型训练/可用性削减则需要冻结评估输入或更强大的适配器/合约。

数据处理从不写入 IO 拥有的 T/S 平面，也从不调度设备命令。自动化可以使用新鲜的、经过验证的派生数据作为单独的规划或控制用例的输入，其授权、安全性和审计规则保持不变。请参阅[数据处理](/concepts/data-processing)和[数据处理流程](/concepts/data-processing-flow)。

## 通信路径

下面的延迟数据来自生产硬件（Cortex-A55 @ 1.4 GHz、ECU-1170）的历史 `README.md` 和 CHANGELOG 测量。发布资格使用当前的跨进程压力和浸泡门。

| 路径 | 机制 | 延迟等级 |
|------|-----------|---------------|
| aether-io→所有消费者（实时数据） | 共享内存写入；每个消费者将配置的插槽从 SQLite | 每点约 10 ns 解析为 SHM |
| aether-io → aether-automation/aether-alarm/aether-api（点更改提示） | 独立过滤的 PointWatch 位图 + 每个消费者的 UDS | 有界的亚毫秒级本地事件路径；轮询修复下降 |
| aether-automation → aether-io（控制命令） | 共享内存写入加上 UDS 通知（`ShmCommandListener` 在 aether-io 端） | 亚毫秒； ~215 µs P50，包括规则评估（测量） |
| aether-io → 设备（协议写入） | 现场总线（Modbus、IEC 104 等） | +5–10 ms；控制物理控制回路 |
| aether-alarm → aether-api、aether-uplink | HTTP（通过 `AETHER_API_URL` / `AETHER_UPLINK_URL` 配置的目标） | 本地 HTTP |
| aether-uplink → 云 | 旧版默认情况下MQTT；实验性代理中立 CloudLink MQTT v1 是选择加入 | 网络 |
| aether-api → 生成/下游客户端 | 经过身份验证的 HTTP 和 WebSocket | 网络 |
| 所有服务 ↔ SQLite | 进程内配置发现(`AETHER_DB_PATH`); aether-history 使用单独的嵌入式历史文件 | 本地 |

如果 aether-io 重新启动，UDS 通知通道会以指数退避（1-5 秒）自动重新连接，因此 aether-io 重新启动不需要重新启动 aether-automation。

两个属性可确保热路径安全：

- **写入所有权。** aether-io 是唯一的写入者共享内存中的遥测/信号槽； aether-automation 是控制/动作槽的唯一编写者。请参阅[共享内存](/concepts/shared-memory)。
- **事件是提示，SHM 是真理。** 事件消费者总是会重新读取槽位； aether-history 和 aether-uplink 保留基于间隔的采样语义。
- **外部存储仅扩展。** 所有六个默认服务均无需 Redis 或 PostgreSQL 即可启动和运行。镜像或历史适配器可以独立启用，而不会成为控制路径的一部分。

## 启动顺序

aether-io 拥有点/健康 SHM 发布，通常首先启动。它发布了两个具有非零纪元和最终提交见证人的飞机； aether-automation 只能附加到与其 SQLite 派生的清单和相同提交的发布相匹配的文件。

排序是在应用程序代码中强制执行的，而不是通过使每个外围服务都依赖于 Redis。外设SHM读取器延迟打开，可以在aether-io之前启动；缺少写入器是一种可重试的读取时条件。在 aether-io 重新启动时，新的点和健康生成将完全初始化，并通过其规范路径自动重命名。现有消费者保留旧的 inode，直到定期身份检查重新打开新一代，并且其订阅位图不会被截断：

1. 在启动期间，Aether自动化调用 `common::dependency::wait_for_dependency("aether-io", <aether-io>/health, 30s)` (`services/automation/src/bootstrap.rs`)。帮助程序 (`libs/common/src/dependency.rs`) 每 2 秒轮询一次运行状况 URL，直到返回 HTTP 2xx 或超时到期。如果 30 秒后 aether-io 仍然不健康，aether-automation 会记录警告并继续启动 — 共享内存可能不可用，直到 aether-io 启动。
2. 当 aether-automation 打开实时状态时，`ShmReadTopologyGeneration` 检查物理标头和提交见证。在服务发布完整一代之前，哈希值、槽计数、纪元或编写器生成不匹配仍然可重试不可用。

## 配置流程
```
config/*.yaml ──► aether sync ──► SQLite (aether.db) ──► services load at startup
```

配置在 `config/` 下以 YAML（和 CSV 点表）形式编写。 `aether` CLI 解析它并将其写入共享 SQLite 数据库 (`tools/aether/src/core/syncer.rs`)；服务只能从 SQLite 读取——没有服务箱解析 YAML。每个服务容器都会接收指向 `aether.db` 的相同 `AETHER_DB_PATH`。 aether-io 自动协调来自 SQLite 的通道运行时、点/健康布局、协议映射和路由投影；受控的显式协调仍然可用于操作员恢复。

## 状态所在位置

- **实时点值** - 共享内存段（Linux 上的`AETHER_SHM_PATH`、`/dev/shm`）。这是热路径的真相来源；请参阅[共享内存](/concepts/shared-memory)。
- **可选镜像** - `aether-redis-bridge` 等扩展可以观察 SHM 并发布最终一致的外部视图。它们从来都不是事实来源，也不是核心服务的启动依赖项。
- **SQLite (`aether.db`)** — 所有配置：渠道、产品、实例、规则、服务设置。仅由 `aether sync` 和服务自己的配置 API 编写。
- **历史数据库** - 默认情况下嵌入 `aether-history.db`。 PostgreSQL / TimescaleDB 仍然是大型部署的可选适配器。

## 相关页面

- [共享内存](/concepts/shared-memory) — 段布局、seqlock、写入所有权
- [数据流](/concepts/data-flow) — 端到端的上游和下游路径
- [数据处理](/concepts/data-processing) — 可选的跨行业处理编排
- [数据处理流程](/concepts/data-processing-flow) — 数据组装和派生结果流程
- [规则引擎](/concepts/rule-engine) — aether-automation 如何评估和执行规则
- [数据模型](/concepts/data-model) — 产品、实例、点
- [部署指南](/guides/deployment) — Docker Compose 和安装程序
