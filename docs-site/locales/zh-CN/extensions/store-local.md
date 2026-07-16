---
title: "aether-store-local"
description: "必须在没有外部服务的情况下运行的网关的本地适配器。"
updated: 2026-07-16
---

# aether-store-local

网关的本地适配器，必须在没有外部服务的情况下运行。

| 适配器 | 持久性 | 预期用途 |
|---|---|---|
| `MemoryLiveState` | process-local | SDK嵌入、测试、小型组合 |
| `MemoryHistorySink` | 进程本地 | 测试和主机管理的持久性 |
| `MemoryHistoryQuery` | 进程本地 | 数据的有界逻辑历史测试夹具处理 |
| `MemoryCovariateSource` | 流程本地 | 用于数据处理的已知未来协变量测试夹具 |
| `SnapshotCovariateSource` | 可原子替换的 JSON | 无需外部的生产已知未来协变量服务 |
| `MemoryAuditSink` | 进程本地 | 测试和主机管理的持久性 |
| `SqliteAuditSink` (`sqlite-audit`) | 嵌入式 SQLite | 无需外部的强制命令审核服务 |
| `MemoryOutbox` | 流程本地 | 一致性测试和临时工作负载 |
| `FileOutbox` | 崩溃可恢复文件 | 离线生产存储并转发 |
| `MemoryCloudLinkSpool` | 进程本地 | 确定性应用程序 ACK/重放一致性 |
| `FileCloudLinkSpool` | 崩溃可恢复文件 | 实验CloudLink位置、重放和丢失证据 |

`MemoryHistoryQuery` 和 `MemoryCovariateSource` 由完整版本的 `BindingIdentity` 键入。他们仅按请求顺序投影请求的逻辑特征，应用半开放时间窗口和硬样本限制，并为每个返回的特征保留一个精确的出处条目。未知的绑定是永久性的调试错误；空的选定窗口仍然是可用性结果。

这些读取适配器故意与 `MemoryHistorySink` 分开。查询或替换确定性装置不会改变仅附加历史接收器，也不会更改 SHM 实时状态权限。

## SnapshotCovariateSource

`SnapshotCovariateSource` 是用于预报协变量（例如天气预报）的生产零服务适配器。构造仅保留路径和硬限制，因此缺少可选快照不会阻止主机启动。每个预测解析都会读取阻塞工作线程上当前发布的文件，在解析之前应用字节绑定，并完全验证它。
```rust
use aether_store_local::{SnapshotCovariateLimits, SnapshotCovariateSource};

# fn example() -> Result<(), Box<dyn std::error::Error>> {
let limits = SnapshotCovariateLimits::new(
    4 * 1024 * 1024, // file bytes
    256,             // bindings
    32,              // runs per binding
    64,              // features per run and response
    4_096,           // samples per run and response
)?;
let source = SnapshotCovariateSource::open("./data/covariates.json", limits)?;
# let _ = source;
# Ok(())
# }
```

JSON 形状很严格：拒绝未知字段和未知枚举值。时间戳是 UTC Unix 毫秒。一次运行对于每个非确定性特征都有一个发布时间、一个源水印、一个精确的有效时间网格以及一个编辑安全的逻辑源引用。
```json
{
  "schema": "aether.covariate-snapshot.v1",
  "bindings": [
    {
      "id": "example-site",
      "revision": 1,
      "runs": [
        {
          "issued_at_ms": 1783741200000,
          "watermark_ms": 1783741800000,
          "valid_times_ms": [1783743300000, 1783744200000],
          "features": [
            {
              "name": "temp_avg",
              "value_type": "number",
              "unit": "Cel",
              "source_ref": "weather.nwp.air_temperature",
              "values": [32.1, 32.0],
              "quality": ["good", "good"]
            }
          ]
        }
      ]
    }
  ]
}
```

`value_type` 是 `number`、`string` 或 `boolean`；非数字特征省略`unit`。仅当匹配质量为 `missing` 时，值才可能为 `null`。质量为 `good`、`uncertain`、`substituted` 或 `missing`。

对于请求的 `as_of`，适配器会选择其 `issued_at_ms <= as_of` 的最新运行。当最新的合格运行具有错误的网格、类型或单位时，它永远不会默默地回退到较旧的运行。其选择的水印也必须等于或早于`as_of`。所请求的半开窗口和样本数定义了精确的规则网格；缺失、额外或脱离网格的有效时间是 `InvalidData` 结果，而不是截断的响应。

对于节奏为 `c` 的 v1 间隔结束预测，未来网格从 `as_of+c` 开始。当前能源负载/PV 任务需要每个非日历未来协变量 `issued_at`。

`quarter_hour` 已保留，不得存储在文件中。当请求作为单位 `1` 的数字未来协变量时，它会根据 UTC 有效时间确定性地生成，以 `calendar.utc.quarter_hour` 出处和请求 `as_of` 作为其水印。

通过写入和同步同级临时文件来发布更新，然后在同一文件系统上的配置路径上重命名它。下一个决议将看到新的运行集。丢失的文件返回`Unavailable`；无效更新返回 `InvalidData`（或硬绑定的 `Rejected`），并且永远不会重用过时的内存快照。避免就地写入，这可能会将部分文件暴露给并发读取器。

## FileOutbox
```rust
use std::sync::Arc;

use aether_ports::{DurableOutbox, OutboxMessage};
use aether_domain::TimestampMs;
use aether_store_local::FileOutbox;

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let outbox: Arc<dyn DurableOutbox> =
    Arc::new(FileOutbox::open("./data/uplink.outbox", 10_000)?);
outbox
    .enqueue(OutboxMessage::new(
        "telemetry/site-a",
        br#"{"temperature": 21.5}"#.to_vec(),
        TimestampMs::new(1_700_000_000_000),
    ))
    .await?;
# Ok(())
# }
```

每个成功的突变都已同步到日志中。恢复重放完整的校验和有效记录，并将不完整或校验和无效的最终记录视为崩溃撕裂的尾部。在稍后提交的记录失败之前关闭损坏，而不是丢弃后来的数据。该日志允许一个进程写入者，受条目计数限制，并且可以使用 `FileOutbox::compact()` 回收。

长时间运行的主机应定期调用压缩；兼容性 `uplink` 在启动时和每小时执行一次。容量限制实时条目，而压缩限制日志中过时的已确认记录。

磁盘耐用性不定义网络传输。选定的 `UplinkPublisher` 决定何时可以确认条目。

## CloudLink 持久队列

`MemoryCloudLinkSpool` 和 `FileCloudLinkSpool` 实现专用 `CloudLinkSpool` 端口。它们保留流纪元、单调位置、稳定的批次标识/摘要、提供/PUBACK 状态、最后持久的应用程序 ACK 以及容量溢出数据丢失证据。传输发布永远不会删除记录。陈旧会话、错误流、错误批次和错误摘要 ACK 无法关闭；精确重复的 ACK 是幂等的。

文件适配器拥有独占进程锁，并同步增量日志中的每个状态转换。恢复仅截断不完整的尾部；校验和或语义失败是损坏，即使在最后一个完整记录中也无法关闭。 `FileCloudLinkSpool::compact()` 以原子方式重写游标元数据和实时记录，并且适配器在 256 个突变后接受更多工作之前进行压缩。其文件格式独立于旧版 `FileOutbox`，并且无法通过通用发件箱端口打开。
