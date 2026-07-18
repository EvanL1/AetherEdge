---
title: "Aether SQLite 历史查询"
description: "用于 `aether-history` 拥有的嵌入式 SQLite 文件的生产 `HistoryQuery` 适配器。它读取现有表："
updated: 2026-07-12
---

# Aether SQLite 历史查询

生产 `HistoryQuery` 适配器，用于 `aether-history` 拥有的嵌入式 SQLite 文件。它读取现有表：
```text
history(time_ms, series_key, point_id, value)
```

适配器从不初始化或迁移该架构。构造是惰性的，因此暂时不存在的历史数据库不会阻塞组合根。每个查询以只读模式打开或重用 SQLite 加 `query_only=ON`；丢失或无法访问的文件会返回键入的 `Unavailable`，并且稍后的查询可以在该文件出现后恢复。所有 SQL 都使用绑定参数。

只读 SQLite 标志不是文件系统安全边界。生产主机必须通过专用只读目录挂载或等效的独立许可帐户/ACL 向 API 读取器公开历史数据库系列（数据库加 WAL/SHM 文件）。当前的基本 Compose 将所有 `/app/data` 读写装载到 `aether-api` 中，因此尚不满足此纵深防御要求，并且生产路由仍处于阻塞状态，直到部署覆盖将历史记录读取访问权限与 API 自己的可写配置/审核数据库分开。

每个委托路由都会修复确切的任务身份、绑定身份、功能定义（包括单元）、任务节奏、核心`HistoryAggregation`，以及物理系列。没有隐式聚合默认值：路由和 `HistoryWindow` 必须声明相同的策略。重复处理同样由任务拥有：`Latest` 将最大的 SQLite `rowid` 保留在聚合之前的时间戳，而 `Reject` 会因输入的无效数据而失败。逻辑 `(task, binding, feature)` 路线是唯一的。相同的物理 `(series_key, point_id)` 可以被不同的任务重复使用，因此每个任务都可以应用自己的委托节奏和聚合策略。

对于逻辑间隔结束标签 `t`，`(t - cadence, t]` 中的原始数值观测值会随着委托的 `Mean`、`Last`、`Sum`、`Min` 或`Max` 政策并加盖 `t`。 EMS 加载路由显式使用 `Mean`。输出始终是完整的请求网格；没有数字观测值的存储桶由 `FeatureValue::missing()` 和 `SampleQuality::Missing` 表示。空行不参与聚合或提前起源。存储特征的水印是实际参与聚合的数字原始观察的最大时间戳，而不是存储桶标签。读取永远不会超出 `HistoryWindow::cutoff()`。

每个存储特征查询都请求 `max_raw_samples_per_feature + 1` 行。如果存在额外行，则操作将失败关闭，而不是静默截断原始输入。日历功能（包括 UTC 一刻钟）是从相同的间隔结束网格确定性生成的。一个逻辑查询中的所有功能读取共享一个读取事务，因此共享一个 SQLite 快照。

该快照在调用时是读取一致的；它不是 `as_of` 的历史时间点快照。当前表既没有 `ingested_at`/system-time 列，也没有源或配置纪元。因此，旧的 `as_of` 之后回填的行可能会出现在以后的重播中，并且在同一 `(series_key, point_id)` 后面重新映射的设备可以连接旧的和新的物理源。任务和绑定修订无法针对当前路由关闭，但无法过滤从未与行一起存储的纪元。使用评估时捕获的冻结数据库/导出进行离线黄金测试或回溯测试。严格的时间点适配器需要双时态摄取元数据加上存储的源/绑定纪元并查询两个片段。

适配器路径也是部署配置，而不是活动历史记录编写器的证明。 `history_config.storage_*` 记录保存的意图，而 `PUT /hisApi/storage` 不会重新连接运行历史记录后端。在存储更改期间禁用数据处理，重新连接或重新启动 `aether-history`，验证活动 SQLite 后端和委托的哨兵系列，然后使用相同路径重新启动 `aether-api`。当前权限检查本身无法区分已保存的意图和未重新连接的写入器。
```rust
use aether_domain::{
    BindingIdentity, FeatureDefinition, FeatureRole, HistoryAggregation,
    HistoryDuplicatePolicy, TaskIdentity,
};
use aether_sqlite_history_query::{
    SqliteHistoryFeatureRoute, SqliteHistoryQuery, SqliteHistoryQueryConfig,
};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let task = TaskIdentity::new("energy.site-load-forecast", 1)?;
let binding = BindingIdentity::new("site-a", 1)?;
let load = FeatureDefinition::numeric("load", FeatureRole::History, "kW")?;
let route = SqliteHistoryFeatureRoute::stored(
    task,
    binding,
    load,
    900_000,
    HistoryAggregation::Mean,
    HistoryDuplicatePolicy::Latest,
    "inst:1:M",
    "101",
    "site.load.active_power",
)?;
let config = SqliteHistoryQueryConfig::new(
    "/var/lib/aether/aether-history.db",
    vec![route],
    100_000,
)?;
let history = SqliteHistoryQuery::open(config).await?;
# let _ = history;
# Ok(())
# }
```

使用以下命令运行真实的 SQLite 合约测试：
```bash
cargo test -p aether-sqlite-history-query
```
