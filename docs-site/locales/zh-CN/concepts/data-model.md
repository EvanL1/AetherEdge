---
title: "数据模型"
description: "产品、实例和 T/S/C/A 点 - 以及为什么实例是没有状态字段的纯事物模型"
updated: 2026-07-10
---

# 数据模型

Aether 对物理工厂进行三层建模：产品（设备类型模板）、实例（单个设备）和点（单个可测量或可操作的数量）。核心设计不变性是实例是一个**纯粹的事物模型**：它包含逻辑结构加上当前值，除此之外别无其他。连接、警报和路由位于单独的数据集中，这些数据集引用实例而无需复制到实例上。

## 三层

**产品** - 描述设备*类型*具有哪些点的模板。在 `Product` (`libs/aether-config/src/automation.rs`) 中定义：唯一的 `product_name`、类型层次结构的可选 `parent_name`（Station → ESS → Battery/PCS 等），以及三个点列表：

- `measurements` — 测量点定义（id、名称、单位、描述）
- `actions` — 操作点定义（id、名称、单位、描述）
- `properties` — 属性模板（静态配置值，例如额定功率）

内核产品库默认为空。已验证的活动包提供 JSON 模型资产（Energy Pack 在 `packs/energy/models/` 下拥有其模型），其中相同的三个列表显示为 `M`、`A` 和 `P`，层次结构显示为 `name` / `pName`。站点可以添加明确的自定义目录。候选产品在验证和运行时通过相同的限制、常规文件、大小、JSON 和重复名称检查。

**实例** — 一台物理设备。在 `Instance` / `InstanceCore` (`libs/aether-config/src/automation.rs`) 中定义：

- `instance_id`（数字，唯一）和 `instance_name`
- `product_name` — 此设备实例化哪个模板
- `parent_id` — 在站点拓扑中的位置（对于根，无实例）
- `properties` - 为产品的属性模板填写的具体值
- 可选 `created_at` - 创建时间戳（有关记录本身的簿记事实，而不是聚合的运行时状态）

这是完整的字段列表。没有 `status`、`health`、`online`、`degraded` 或 `alarm_state` 字段——请参阅下文“纯度规则”。

**Point** — 单个可测量或可操作的数量，由在其产品和点列表中唯一的数字 ID 标识。采集端的一个点是设备报告的内容（充电状态、断路器位置）；命令端的点是系统可以设置的内容（启动命令、功率设定值）。

## 点类型

通道端点使用 `libs/aether-core/src/types.rs` 中 `PointType` 定义的四类分类（通过 `aether-model` 重新导出）。枚举文档将此称为标准 IEC“四远程”分类；每个变体都带有中国标准代码 (YC/YX/YK/YT) 的 serde 别名。

| 类型 | 名称 | 信号类型 | 方向 | 写入所有者 |
|------|------|-------------|-----------|-------------|
| `T` | 遥测 | 模拟测量(YC) | 设备 → 系统 | io |
| `S` | 信号 | 数字状态 (YX) | 设备 →系统 | io |
| `C` | 控制 | 数字命令（YK） | 系统→设备 | 自动化 |
| `A` | 调整 | 模拟设定点(YT) | 系统→设备 | 自动化 |

`PointType` 提供代码库在各处使用的类别谓词：`is_measurement()` 对于 T 和 S 为真，`is_action()` 对于 C 和 A，`is_analog()` 对于 T 和 A，`is_digital()` 对于 S 和 C。

写入所有权是通过端口和组合边界的构造强制执行的。 IO 单独接收 `LiveStateWriter` 并通过 `ShmAcquisitionStateWriter` 发布 T/S 采集批次。自动化通过 `ShmDeviceCommandSink` 提交键入的 C/A 命令；它无法获取获取编写器，并且接收器在接触 SHM 之前会拒绝非命令点类型。

在实例端，四种通道类型分解为两个角色，由 `libs/aether-model/src/types.rs` 中的 `PointRole` 定义：

- `M`（测量）— 数据流设备 → 模型
- `A`（操作）— 数据流模型 →设备

实时值通过键入的 SHM 坐标进行寻址。通道到模型和模型到通道的映射是 SQLite 中的持久配置，并加载到进程内路由索引中。自定义镜像可以选择自己的外部密钥形状，但这些密钥不是核心数据模型的一部分。

## 纯度规则

实例保存**逻辑结构加上当前值，仅此而已**。 `Instance` 结构体具有标识、层次结构、属性和点映射。其上不存在任何类型的聚合状态——没有`status`、没有`health`、没有`online`、没有`alarm_state`。

这是一个深思熟虑的设计决策，而不是遗漏。每个候选状态字段实际上是*不同*子系统的属性，具有自己的编写器和自己的生命周期：

- “在线”是**通信通道**的属性，而不是设备的属性。实例的点通过路由表绑定到通道点，并且没有任何东西强制它们全部进入一个通道。实例级在线标志将是 io 在通道运行状况 SHM 段中发布的每通道事实的有损聚合。
- “有活动警报”是 **警报事件流** 的属性，警报在其自己的表中拥有该属性。通过坐标报警参考点；仅以一种方式引用点。
- “上次控制写入失败”是**单次调用**的属性，会出现在该调用的返回值中（请参阅下文“对界面和智能体的影响”）。

将其中任何一个复制到实例上都会创建其权威作者位于其他地方的事实的第二个副本。第二个副本会变得陈旧，在部分失败的情况下与原始版本发生冲突，并迫使每个作者了解每个消费者。保持实例纯净意味着每个事实只有一个写入者，消费者在读取时加入他们所需的数据集。

## 四个正交数据集

系统的运行时状态分为四个数据集。每个人都有一位作家；

| 数据集 | 位置 | 含义 | 写入方 |
|---------|-------|---------|--------|
| 实例当前值 | 实时状态 SHM 中的键入点槽，通过路由索引解析 | 事物模型值（如果从未获取，点可能为 NaN） | io （`M` 源值）；自动化（`A`，执行操作时） |
| 渠道连接 | 通道运行状况 SHM 段 | 每通道在线/离线状态和心跳 | io |
| 警报事件 | 警报 SQLite 表 `alert` 和 `alert_event` (`services/alarm/src/db.rs`) | 事件流：触发/恢复行按 `service_type` 寻址点 + `channel_id` + `data_type` + `point_id`（例如点，`service_type` 为 `"inst"`，id 列保存实例 ID） | 警报 |
| 路由配置 | 加载到每个进程索引中的 SQLite 映射 | 通道点之间的静态点对点接线和实例点 | 配置同步/API事务 |

路由索引是其他之间的连接键：C2M 将通道点 (`{channel_id}:{T|S}:{point_id}`) 映射到实例测量 (`{instance_id}:M:{point_id}`)，M2C 将实例操作 (`{instance_id}:A:{point_id}`) 映射回通道命令点。报警规则通过相同的坐标来寻址值。实例从不引用警报、连接或路由返回 - 箭头指向一个方向。

## NaN 作为哨兵

在共享内存平面中，“此处从未写入任何数据”被编码在值本身中。创建每个 SHM `PointSlot` (`crates/aether-dataplane/src/core/slot.rs`) 时，其值和原始值字段都设置为安静的 IEEE-754 NaN（硬编码位模式 `SLOT_UNWRITTEN_BITS = 0x7FF8_0000_0000_0000`）。第一次真正的写入用有限双精度替换哨兵。这消除了历史上的歧义，即零初始化时隙与真实读数 0.0 无法区分。

跨服务数据平面中**没有旁路质量标志**。 32 字节 `PointSlot` 布局是值、时间戳、原始值、seqlock 序列和脏标志 — 没有其他内容。 （io 的协议层确实在 `services/io/src/protocols/core/` 内部跟踪每点质量代码，但它们永远不会跨越 SHM 边界。）值就是数据；消费者必须显式检查 NaN：

- SHM 读取器探测返回值的 `PointSlot::is_unwritten()` 或 `f64::is_nan()`。
- 规则引擎 (`libs/aether-rules/src/executor.rs`) 跟踪哪些变量在 `RuleReadOutcome` 中不可用，并跳过求值而不是替换 0.0 — 否则像 `current < threshold` 这样的条件会在缺失时默默触发数据。

每个使用方的规则都是相同的：缺少有效的有限值是您必须处理的第一级结果，而不是在其他地方记录的错误状态。

## UI 和代理的后果

**控制按钮变灰是客户端的加入**。要确定实例 7 上的控件当前是否可以到达其设备，请通过 M2C 路由索引将实例的操作点解析为通道id，然后从通道运行状况 SHM 段中读取该通道。后端故意不将其预先加入到实例中。自动化在分派之前在写入时执行相同的连接。

**控制写入失败出现在调用方的返回值中，而不是作为实例状态。** 当目标通道离线时，自动化会使用 `AutomationError::ChannelUnreachable` (`services/automation/src/error.rs`) 拒绝写入；降级的调度路径（SHM 已写入，但向 io 的通知失败）失败并显示 `AutomationError::DispatchDegraded`。两者均通过 `AetherErrorTrait::http_status` (`libs/errors/src/lib.rs`) 到达 HTTP，其中它们的类别（分别为 `ResourceBusy` 和 `Network`）均映射到 **HTTP 503**；调用者通过错误代码（`AUTOMATION_CHANNEL_UNREACHABLE` 与 `AUTOMATION_DISPATCH_DEGRADED`）来区分它们，并且两者都是可重试的。在规则引擎中，无法解析的操作会通过执行程序的 `action_skipped` 路径生成带有 `success: false` 的 `ActionResult` 和 NaN 值，归因于导致跳过的变量。

在每种情况下，失败都是*该调用方、该调用*的信息 — 会报告、记录并可能重试，但不会将任何内容写回到实例中。 `inst:{id}:A` 的下一个读取者会看到最后接受的命令，而不是失败标志。

## 相关页面

- [系统架构](/concepts/architecture) — 服务及其通信方式
- [共享内存](/concepts/shared-memory) — 深入了解 SHM 平面：槽、seqlock、写入器所有权
- [数据Flow](/concepts/data-flow) — 端到端的上行链路和下行链路路径
- [产品模型](https://github.com/EvanL1/AetherEdge/blob/main/docs/domain/product-models.md) — 产品库及其域含义
- [安全操作](/guides/safe-operations) — 为什么控制写入被门控以及故障如何传播
