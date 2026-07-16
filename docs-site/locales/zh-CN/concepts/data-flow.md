---
title: "数据流"
description: "SHM 原生上行链路和下行链路路径端到端，具有延迟预算"
updated: 2026-07-15
---

# 数据流

Aether 沿着两条独立的路径移动数据。 **上行链路**将测量点——遥测 (T) 和信号 (S) 值——从设备通过 aether-io 传送到共享内存，然后从那里传送给每个消费者。 **下行链路** 将操作点 — 控制 (C) 和调整 (A) 命令 — 从规则引擎或 HTTP API 通过Aether自动化传送回设备。实时点值和命令传输使用共享内存段作为事实和传输的来源。默认服务不需要 Redis 或 PostgreSQL 来获取实时数据。

## 上行链路（设备→消费者）

1. 协议帧到达通信通道，aether-io 中通道的协议适配器将其解码为点值。
2. aether-io 通过 `ShmAcquisitionStateWriter` (`extensions/shm-bridge/src/acquisition_writer.rs`) 提交每个类型的 T/S 批次。适配器在突变前后验证不可变清单和写入器生成；槽索引写入是私有实现细节。
3. **事件路径（立即）。** 每次槽写入后，`PointWatchPublisher` (`extensions/shm-bridge/src/point_watch.rs`) 都会检查每个事件使用方拥有的独立位图。命中时，有界队列会向该消费者的 UDS 发送 `PointWatchEvent`。 aether-automation、aether-alarm 和 aether-api 无法窃取或覆盖彼此的订阅。该事件只是一个唤醒提示；每个消费者重新读取 SHM，并轮询修复丢弃的事件。
4. **直接读取路径。**消费者从一个 SQLite 拓扑快照解析通道/实例坐标，并重新读取匹配的 SHM 槽。历史记录和上行链路将其精确配置的点和所需的路由绑定到一个提交的点/运行状况时期，然后将该不可变的一代固定为整个收集/上传过程。事件不会默默地改变其节奏。

生产 `aether-uplink` 组合仍然使用已弃用的旧版 MQTT 主题及其通用 `FileOutbox` 交付边界。实验性 CloudLink 路径将相同的固定生成转换为 `PointSample` 事实，添加发布纪元和拓扑摘要，并将规范业务内容放置在单独的 `CloudLinkSpool` 中。 MQTT QoS 1/PUBACK 仅推进传输状态；匹配的云持久应用程序 ACK 是唯一的删除权限。 Disconnect 使记录在相同的流位置/批次 ID/摘要下保持可重播，并且不会阻止任何 SHM 使用方。```
Device ──frame──► aether-io protocol adapter (decode)
                        │
                        ▼  set_direct (~10 ns/point)
                  SHM T/S slot (authoritative)
                   │             │
      per-consumer │             │ periodic sampling
      bitmap + UDS │             ├─► aether-history
       ┌───────────┴────┐        └─► aether-uplink
       ▼                ▼
 aether-automation aether-alarm/aether-api
    event hint   event hint → SHM re-read
```

## 下行链路（规则/API → 设备）

1. 外部 HTTP、CLI 或 MCP 控制调用成为Aether自动化中的传输中立`RequestContext`。 `ControlApplication` 检查 `device.control` 权限并显式确认，在本地 SQLite 中保留强制尝试审核事件，然后才调用命令调度程序。内部确定性规则操作在分阶段迁移期间直接进入现有调度程序路径。
2. 调度程序调用 aether-automation 的 `execute_action` (`services/automation/src/instance_data.rs`)，后者从内存中路由缓存（由 `aether sync` 填充的 `route:m2c` 表的镜像）**一次**将实例操作点解析为其通道命令点。已解析的目标将通过调用的其余部分进行线程处理，因此并发路由重新加载无法在飞行中更改决策。
3. 离线门读取通道运行状况 SHM 段。在写入任何内容之前，离线通道会拒绝使用 `ChannelUnreachable` 进行写入。
4. 值验证后，`ShmDeviceCommandSink` (`extensions/shm-bridge/src/command_sink.rs`) 会镜像 C 或 A 槽。写入前后检查写入器生成和规范路径；不匹配意味着 aether-io 重新启动并重建了该段，因此写入被丢弃并且分派失败，而不是进入过时的布局。
5. 相同的命令适配器通过 Unix 域套接字发送固定大小的 56 字节帧。该通知携带通道/点坐标、值位、发出/到期时间戳以及用于重复数据删除的生产者 ID + 序列号。如果 aether-io 关闭，通知程序会以指数退避（1-5 秒）重新连接。本机部署默认为 `/tmp/aether-m2c.sock`； Docker 将 `AETHER_M2C_SOCKET` 设置为 `/shm/rtdb/aether-m2c.sock`，以便两个隔离容器都能看到套接字。
6. aether-io 的 `ShmCommandListener` (`services/io/src/core/channels/shm_listener.rs`) 接收通知、拒绝过期帧、按顺序进行重复数据删除，并将命令转发到所属通道的队列。在协议调度之前，`CommandGuard` 验证可写点是否存在以及该值是否满足其最小/最大/步长策略；只有这样，协议适配器才能将其写入现场总线。

实时命令数据永远不会传输数据库：传输方式是 SHM 加上 UDS 通知。本地 SQLite 围绕外部命令存储安全审核事件，但不是命令传递的一部分，并且从不镜像实时点值。中途失败的调度（共享内存已写入，但通知丢失，或没有可用的写入器）对调用者来说是一个错误；请参阅[数据模型](/concepts/data-model)，了解这些故障如何映射到 HTTP 状态。

## 数据处理路径（源数据 → 派生数据）

Aether 数据处理为请求驱动的计算引入了第三种非权威路径。它既不是上行镜像，也不是下行命令路径：
```text
caller
  │ typed data-processing task
  ▼
DataProcessingApplication
  ├─ HistoryQuery ───────────── historical observations
  ├─ LiveState ──────────────── current read-only tail
  └─ task/request context ───── future or external covariates
             │
             ▼ complete, bounded ProcessingFrame
         DataProcessor
             │
             ▼ schema-validated, expiring ProcessingResult
       authenticated HTTP DerivedData response
```

该应用程序解析语义绑定，对齐和聚合时间戳，要求委托的单元/符号元数据精确匹配，检查丢失和过时的输入，并发送处理器请求中的值。版本 1 不执行运行时单位/符号转换。处理器永远不会接收 SHM、SQLite 或内部服务 API 的凭据，也永远不会通过返回 Aether 来解析站点标识符。

结果记录其输入水印、输入摘要、处理器出处、质量、状态和到期时间。它是导出的证据而不是测量：它不会写入 IO 拥有的 T/S 段。如果自动化使用结果，则在现有审核的命令路径可以起作用之前，单独的规划/控制用例会验证新鲜度和安全性。因此，处理器丢失会删除可选的建议输入，而不会中断采集或本地安全规则。请参阅[数据处理流程](/concepts/data-processing-flow) 了解完整的合约。

事件时间 `as_of` 本身并不是历史知识切割。当前历史记录没有摄取/源纪元，并且工件出处没有训练/可用性削减，因此时间点评估使用冻结的历史记录和工件输入，而不是查询当今的可变源以获取旧帧。

## 延迟预算

微秒数据是生产硬件（Cortex-A55 @ 1.4 GHz、ECU-1170 / EdgeLinux 22.04）上记录的历史测量值自述文件和变更日志。纳秒数字是自述文件规定的热路径写入的数量级；发布资格必须重新运行当前压力门。

| 阶段 | 延迟 | 源标签 |
|-------|---------|--------------|
| aether-io 共享内存写入 (`set_direct`) | ~10 ns/点 | 自述文件 |
| 收到数据更改→ aether-automation 事件 (PointWatch)交付） | P50 206 µs，P99 526 µs | README/CHANGELOG，测量 |
| +规则评估+控制SHM写入+UDS通知aether-io | ~215 µs P50，~540 µs P99（累积） | README，测量 |
| +设备协议写入（Modbus / IEC 104现场总线） | +5–10 ms | README |
| aether-alarm → aether-api/aether-uplink，服务HTTP跃点 | 本地HTTP | — |

CHANGELOG还记录P99.9 的事件路径为 1.4–2.2 毫秒，并指出 PointWatch 取代了之前的 100 毫秒 Redis-tick 轮询模型（50–150 毫秒端到端）——在关键路径上大约提高了 500 倍。软件内部控制路径为亚毫秒级；现场总线写入控制物理控制环路。

## 可选状态镜像

外部状态镜像是扩展，而不是控制路径的参与者。 `extensions/redis-bridge` 实现 `StateMirror` 扩展合约并显式构建和启动。它可以观察 SHM 并发布最终一致的远程视图，但没有默认服务从中读取数据，并且镜像故障不会影响采集、规则、警报、历史记录、API 读取、上行链路或命令传递。

相同的边界适用于其他自定义存储：通过扩展 API 使用 SHM/事件，保持存储非权威性，并且不将存储添加到核心服务启动依赖项。

## 相关页面

- [架构](/concepts/architecture) — 这些路径连接的服务
- [共享内存](/concepts/shared-memory) — 槽布局、seqlock、写入所有权
- [数据模型](/concepts/data-model) — 点、实例和 NaN/缺席语义
- [数据处理](/concepts/data-processing) — 可选的行业中立处理边界
- [数据处理流程](/concepts/data-processing-flow) — 处理器请求数据流和故障语义
- [CloudLink MQTT v1](/reference/cloudlink-mqtt-v1) — 实验性应用程序-ACK/重播边缘路径
- [规则Engine](/concepts/rule-engine) — PointWatch 事件到达后会发生什么
