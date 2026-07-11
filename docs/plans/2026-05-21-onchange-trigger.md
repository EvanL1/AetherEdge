# OnChange 触发器设计

## 背景与动机

### 现状

`aether-rules` 调度器（`libs/aether-rules/src/scheduler.rs`）目前只支持
`TriggerConfig::Interval` 一种触发模式：按固定周期（`interval_ms`）在 100ms
tick 驱动下定时执行规则。对绝大多数巡检、统计、心跳类规则，这已经足够。

### 痛点

**柴发 + Modbus RTU 场景**：

- 柴发 Modbus 采集周期 80–200ms，comsrv 上送节奏与 tick 完全异步。
- 100ms 定时轮询平均引入 50ms 累积延迟（最坏 100ms）。
- 更关键的是：规则执行时**无法区分**"数据这一拍真的变了"和"tick 到了但数据和上次一样"。
  两种情况的执行代价完全一样，但只有前者才有业务意义。

具体地，对于"柴发过压 → 切负载"这类控制规则：

1. 定时触发：电压从 395V → 420V 时，规则最多要等 100ms 才响应。
2. 无变化判别：电压稳定在 420V 时，每 100ms 仍重复执行一次规则，浪费执行资源，
   还可能重复写 action point 引发 SHM seqlock 竞争。

### 目标

支持"数据变化即触发"的事件采样语义，同时**不引入新的 IPC 拓扑**：

- 不新增 UDS socket，不修改 SHM 协议。
- 仍在 100ms tick 内同步采样（RTDB 快照），变化检测纯属计算层。
- 与现有 Interval 规则共享同一 `RuleScheduler` 主循环，零额外线程。

---

## 设计

### TriggerConfig 扩展

```rust
// libs/aether-rules/src/scheduler.rs:111-137
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TriggerConfig {
    Interval {
        interval_ms: u64,
    },
    OnChange {
        point_refs: Vec<PointRef>,
        #[serde(default)]
        time_deadband_ms: Option<u64>,
        #[serde(default)]
        value_deadband: Option<ValueDeadband>,
    },
}
```

`TriggerConfig` 以 `tag = "type"` 的 enum 序列化形式存储在 SQLite 的
`rules.trigger_config`（`Option<String>`）列中。`OnChange` variant 反序列化失败
时，`load_rules()` 自动降级为 `Interval { interval_ms: cooldown_ms }`（`scheduler.rs:331-344`）。

### PointRef — 点位引用

```rust
// libs/aether-rules/src/scheduler.rs:43-47
pub struct PointRef {
    pub instance: u32,
    pub point_type: PointKind,  // Measurement | Action
    pub point: u32,
}
```

`PointRef::cache_key()` 生成 `"M:{instance}:{point}"` 或 `"A:{instance}:{point}"`
格式的字符串，同时用作快照 map 的 key 和 `OnChangeState.last_value` 的 key
（`scheduler.rs:52-58`）。

### ValueDeadband — 值变化死区

```rust
// libs/aether-rules/src/scheduler.rs:77-82
pub enum ValueDeadband {
    Absolute { threshold: f64 },
    Percent  { threshold: f64 },
}
```

- `Absolute`：`|new - last| > threshold`，适合工程量（电压 ±0.5V）。
- `Percent`：`|new - last| / |last| * 100 > threshold`，适合比例变化（功率 ±1%）。
  特殊情况：`last == 0` 时只要 `new != 0` 即触发（穿零保护，`scheduler.rs:93-98`）。

### time_deadband_ms — 时间死区

规则级别的最小触发间隔。两次触发之间不足 `time_deadband_ms` 毫秒，直接跳过
（`scheduler.rs:171-176`）。时间死区是**规则级**（一条规则一个计时器），
不是点级。

### 双死区 AND 语义

```text
触发条件 = time_deadband 通过 AND 至少一个 point_ref 的 value_deadband 通过
```

两个死区**同时**必须满足（AND 关系）：

| time_deadband | value_deadband | 含义 |
|--------------|---------------|------|
| None | None | 任意点变化即触发（每 tick 可触发） |
| Some(200) | None | 距上次触发 ≥200ms，且有任意点变化 |
| None | Some(Absolute(0.5)) | 有任意点 |Δ| > 0.5 即触发（频率不限） |
| Some(200) | Some(Absolute(0.5)) | 最严格：同时满足两个条件 |

**不实现**的死区类型：

- **迟滞死区**（Hysteresis）：属于告警/控制逻辑，在 `alarmsrv` 或规则节点内处理。
- **变化率死区**（Rate-of-change）：归属规则引擎内的 Calculation 节点，不是调度层关心的。
- **积分死区**（Integral）：YAGNI，当前无需求。

两种死区就能覆盖工业级 80% 场景，不过早泛化。

---

## JSON 示例

### trigger_config 字段格式

**Interval 触发（现有）**

```json
{
  "type": "interval",
  "interval_ms": 5000
}
```

**OnChange 触发（新增）**

```json
{
  "type": "on_change",
  "point_refs": [
    { "instance": 42, "point_type": "measurement", "point": 0 },
    { "instance": 42, "point_type": "measurement", "point": 1 }
  ],
  "time_deadband_ms": 200,
  "value_deadband": {
    "type": "absolute",
    "threshold": 0.5
  }
}
```

`time_deadband_ms` 和 `value_deadband` 均为可选，省略即表示"无该死区"。

### 柴发场景规则配置示例

**规则 1：监视柴发运行状态（布尔型，无值死区）**

```json
{
  "name": "柴发运行状态联动",
  "enabled": true,
  "priority": 100,
  "cooldown_ms": 0,
  "trigger_config": {
    "type": "on_change",
    "point_refs": [
      { "instance": 10, "point_type": "measurement", "point": 5 }
    ],
    "time_deadband_ms": 500
  }
}
```

说明：`point 5` 为运行/停止状态位（0/1），500ms 时间死区防止起停抖动重复触发。
不设 `value_deadband`，因为布尔跳变本身就是最小有效变化。

**规则 2：监视柴发输出电压（模拟量，需值死区）**

```json
{
  "name": "柴发过压保护",
  "enabled": true,
  "priority": 200,
  "cooldown_ms": 2000,
  "trigger_config": {
    "type": "on_change",
    "point_refs": [
      { "instance": 10, "point_type": "measurement", "point": 0 }
    ],
    "time_deadband_ms": 100,
    "value_deadband": {
      "type": "absolute",
      "threshold": 1.0
    }
  }
}
```

说明：电压变化 ≥1V 且距上次触发 ≥100ms 才执行。`cooldown_ms: 2000` 确保执行后
的 2s 内不再重复执行（cooldown 是执行后的冷却，time_deadband 是触发前的门控，
两者正交）。

**规则 3：柴发周期性健康检查（仍用 Interval）**

```json
{
  "name": "柴发健康巡检",
  "enabled": true,
  "priority": 50,
  "cooldown_ms": 0,
  "trigger_config": {
    "type": "interval",
    "interval_ms": 60000
  }
}
```

说明：每分钟统计一次柴发累计运行时间、启动次数等，不需要事件驱动，
保留 Interval 语义更清晰。

---

## NaN 处理

NaN 是本项目 SHM v3 的哨兵值，含义是"数据暂时不可用"
（见 `CLAUDE.md` "Instance 是纯物模型"一节）。

OnChange 触发器对 NaN 的处理规则（`scheduler.rs:181-184`）：

### 规则 1：入站 NaN 不触发

```rust
// scheduler.rs:181-184
let new_value = match snapshot.get(&key) {
    Some(Some(v)) if v.is_finite() => *v,
    _ => continue, // missing 或 NaN → 跳过该点
};
```

NaN 被视为"数据缺失"，不代表"数据变了"。快照中的 NaN 会被
`fetch_point_snapshot()` 过滤掉（`scheduler.rs:695-698`），映射为 `None`。

### 规则 2：NaN → 有效值的恢复触发一次

```rust
// scheduler.rs:186-188
match state.last_value.get(&key) {
    None => return true, // 首次有效观测触发
    ...
}
```

当 `last_value` 中没有该点的记录（初始化或 NaN 期间未更新），
首次观测到有限值即触发一次。这确保了通信恢复后规则能立即响应，
而不是等到下一次"真正的变化"。

### 规则 3：有效值 → NaN 不更新 last_value

Phase 3 的 `last_value` 更新只写入有限值（`scheduler.rs:616-621`）：

```rust
if let Some(Some(v)) = snapshot.get(&key) {
    if v.is_finite() {
        scheduled.onchange_state.last_value.insert(key, *v);
    }
}
```

NaN 不会覆盖 `last_value`，这意味着通信中断期间的最后有效值被保留，
恢复后的首个有效值会与该值比较，而不是盲目触发。

---

## 实现说明

### tick() 四阶段快照模式

完整流程在 `scheduler.rs:419-631`，总结如下：

```text
tick()
  │
  ├── Phase 0   读锁收集所有 OnChange 规则的 point_refs（HashSet 去重）
  │             释放读锁
  │
  ├── Phase 0.5 batch HMGET：按 instance 分组，每组一次 hash_mget
  │             生成 snapshot: HashMap<cache_key, Option<f64>>
  │             （不持有任何锁）
  │
  ├── Phase 1   读锁遍历所有规则：
  │             - Interval: elapsed >= interval_ms?
  │             - OnChange: should_trigger_onchange(state, refs, deadbands, snapshot)?
  │             → 收集待执行列表 Vec<(idx, Arc<Rule>, is_onchange)>
  │             释放读锁（~10μs）
  │
  ├── Phase 2   buffer_unordered(max_concurrency=4) 并发执行规则
  │             不持有任何锁
  │
  └── Phase 3   写锁：更新 last_execution + last_cooldown_start
                对 OnChange 规则额外更新 onchange_state.last_value + last_trigger
                释放写锁（~100μs）
```

### should_trigger_onchange — 纯函数

变化检测提取为独立的自由函数（`scheduler.rs:162-200`），不依赖 `self`，
便于单元测试和 benchmark 覆盖各种死区组合，无需启动完整调度器。

```rust
pub fn should_trigger_onchange(
    state: &OnChangeState,
    point_refs: &[PointRef],
    time_deadband_ms: Option<u64>,
    value_deadband: Option<&ValueDeadband>,
    snapshot: &HashMap<String, Option<f64>>,
    now: Instant,
) -> bool
```

### fetch_point_snapshot — 按 instance 批量 HMGET

`scheduler.rs:666-713`：

1. 将 `HashSet<PointRef>` 按 `(hash_key, PointKind)` 分组（同一 instance 同一
   point_type 聚合）。
2. 每组调用一次 `rtdb.hash_mget(&hash_key, &fields)`。
3. 解析结果：非 `f64`、NaN、Inf 均存为 `None`。
4. HMGET 失败时对该组所有点写入 `None`（不 panic，规则这轮跳过即可，warn 日志）。

所有 OnChange 规则的 N 个订阅点，最多 `ceil(N / 平均每instance订阅点数)` 次
Redis 往返，而非 N 次逐点 GET。对 1000 个订阅点、100 个 instance 的场景，
合并为约 100 次 HMGET，每次 RTT ~200μs，总快照时间 ~20ms（Redis 本地）。

### 不改动的内容

- **SHM 协议**：不修改任何 SHM header 字段，不加新的 slot 类型。
- **UDS 通知**：OnChange 触发走 RTDB 快照路径，不走 `ShmNotifier` → `ShmCommandListener`。
- **schema**：`rules` 表的 `trigger_config` 列已是 `TEXT`（`Option<String>`），新
  variant 直接存 JSON，**零迁移**。

---

## 选择指南（决策矩阵）

### 何时用 Interval

| 场景 | 推荐 interval_ms |
|------|-----------------|
| 定时巡检（设备健康汇总） | 60000（1 分钟） |
| 定时统计（日电量 period_delta） | 3600000（1 小时）或 86400000（每天） |
| 定时心跳（远程通信保活） | 30000（30 秒） |
| 定时数据归档 | 视业务需求 |

特征：**时间是第一驱动力**，数据是否变化无关紧要。

### 何时用 OnChange

| 场景 | 推荐配置 |
|------|---------|
| 事件联动（状态位变化 → 动作） | `time_deadband_ms: 200`，无值死区 |
| 过/欠压保护（模拟量越限） | `value_deadband: Absolute(1.0)` + `time_deadband_ms: 100` |
| 状态变化告警联动 | `time_deadband_ms: 500`，无值死区 |
| 快速控制响应（RTU 事件驱动） | `time_deadband_ms: 0`（或省略），无值死区 |

特征：**数据变化是第一驱动力**，时间是辅助约束。

### 数据流参考

```
上行: Device → comsrv → SHM(T/S slots) [~10ns/点]
                      → ShmRedisSync(100ms) → Redis inst:{id}:M
                                                    ↑
                              OnChange 快照在此读取 ┘
下行: OnChange 规则执行 → action 节点 → SHM(C/A slots) write
                                      → UDS notify → comsrv → Device
```

OnChange 快照读取 `inst:{id}:M`（Redis），不直接读 SHM（SHM 是 comsrv/modsrv 的
内部高速通道，规则引擎与 SHM 之间没有直接接口，参见 `CLAUDE.md` 服务间通信表）。

---

## 性能预期

| 操作 | 估算延迟 | 备注 |
|------|---------|------|
| `ValueDeadband::exceeds()` 单次判断 | ~50ns | 纯算术，待 bench 验证 |
| `should_trigger_onchange()` 单次（10 个点） | ~500ns | 含 HashMap 查找 |
| Phase 0 读锁收集订阅（1000 规则） | ~10μs | HashSet insert，常数因子小 |
| Phase 1 读锁 filter_map（1000 规则） | ~10μs | 纯内存遍历 |
| Phase 3 写锁更新 onchange_state（100 触发规则） | ~100μs | HashMap insert，有界 |
| fetch_point_snapshot（100 个 instance，1000 点） | ~20ms | ~100 次 HMGET，Redis 本地 |

**关键约束**：fetch_point_snapshot 是 100ms tick 内唯一的异步 IO，
20ms 远低于 100ms 预算。即使快照慢 2× 变成 40ms，仍有 60ms 余量供规则执行。

1000 个 OnChange 规则的单 tick 总开销（不含规则实际执行）约 ~50μs 计算 +
~20ms 快照，完全在 100ms 内。

**不影响 Interval 规则**：Phase 0/0.5 对没有 OnChange 规则时快速短路：

```rust
// scheduler.rs:437-441
let snapshot = if subscriptions.is_empty() {
    HashMap::new()
} else {
    self.fetch_point_snapshot(&subscriptions).await
};
```

纯 Interval 场景的 tick 开销与改动前完全一致。

---

## 向前兼容

### trigger_config 是 Option

`rules.trigger_config` 列为 `TEXT NULL`（`types.rs:47`）。`load_rules()` 中：

```rust
// scheduler.rs:331-344
let trigger = rule
    .trigger_config
    .as_ref()
    .and_then(|json| serde_json::from_str(json).ok())
    .unwrap_or_else(|| {
        let interval_ms = if rule.cooldown_ms > 0 { rule.cooldown_ms } else { 1000 };
        TriggerConfig::Interval { interval_ms }
    });
```

- `trigger_config` 为 `NULL` → 降级为 `Interval { interval_ms: cooldown_ms }`。
- `trigger_config` JSON 反序列化失败（旧格式/损坏）→ 同样降级。
- 旧规则**零改动**，自动按 cooldown_ms 的 Interval 运行。

### 无 schema 迁移

`trigger_config` 列早已存在（用于旧版 Interval JSON），OnChange 只是新的
JSON variant，`SCHEMA_VERSION` 无需递增，`aether init` / `aether sync`
不受影响。

---

## 不做什么（Non-goals）

### 真·事件驱动 IPC

方案：comsrv 每次写 SHM 后通过新增 UDS socket 通知规则引擎，
规则引擎 await 事件再触发。

**不做**：增加新的 IPC 拓扑（新 socket、新连接管理、新 backpressure 设计），
复杂度 >> 收益。100ms 快照采样在柴发场景已经够用。YAGNI。

### 迟滞死区

方案：触发后设置一个"关闭阈值"，值回到阈值以下才再次允许触发。

**不做**：迟滞是告警/控制逻辑，属于 `alarmsrv` 或规则执行节点内部实现，
不是调度层关心的。放在调度层会把"何时执行"和"执行什么"混在一起。

### 变化率死区

方案：`|Δv/Δt| < threshold` 才触发。

**不做**：变化率计算需要时序状态，用 `Calculation` 节点的 `period_delta` 或自定义
formula 实现更合适，不在调度层增加复杂度。

### Sub-100ms 响应

方案：独立于 tick，专门起一个 polling 线程以 10ms 周期采样。

**不做**：柴发 Modbus RTU 协议本身 80–200ms 上送一次，sub-100ms 响应没有物理意义。
100ms tick 对应的 ≤100ms 延迟已经是该协议的理论最优。

---

## 实现检查清单

以下各项已在 `libs/aether-rules/src/scheduler.rs` 中实现：

- [x] `PointRef` struct + `cache_key()` 方法（第 36–59 行）
- [x] `PointKind` enum（第 62–70 行）
- [x] `ValueDeadband` enum + `exceeds()` 方法（第 72–103 行）
- [x] `TriggerConfig::OnChange` variant（第 111–137 行）
- [x] `OnChangeState` struct（第 146–155 行）
- [x] `should_trigger_onchange()` 纯函数（第 162–200 行）
- [x] `ScheduledRule.onchange_state` 字段（第 211 行）
- [x] `tick()` Phase 0 订阅收集（第 423–434 行）
- [x] `tick()` Phase 0.5 快照批量 HMGET（第 437–441 行）
- [x] `tick()` Phase 1 OnChange 决策（第 465–477 行）
- [x] `tick()` Phase 3 onchange_state 更新（第 613–626 行）
- [x] `fetch_point_snapshot()` 按 instance 分组批量拉取（第 666–713 行）

待办事项（实现阶段追踪）：

- [ ] `should_trigger_onchange` 单元测试覆盖（各种死区组合 + NaN 路径）
- [ ] `ValueDeadband::exceeds` bench（确认 ~50ns 假设）
- [ ] API 层（`modsrv` PUT /rules）支持写入 `trigger_config` 字段
- [ ] 前端规则编辑器支持 OnChange 配置 UI（`point_refs` 选择器）
- [ ] `aether sync` YAML 规则定义支持 `trigger_config` 字段
