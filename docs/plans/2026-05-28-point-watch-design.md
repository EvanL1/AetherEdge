# PointWatch 设计文档

**状态**: 设计草稿 / Decision Record  
**日期**: 2026-05-28  
**作者**: 系统生成（基于代码库深度分析）

---

## 1. 摘要与动机

### 背景

comsrv 和 modsrv 通过共享内存（SHM）交换实时数据：comsrv 拥有 T/S 槽，modsrv 拥有 C/A 槽。目前已有一条 M2C 方向的低延迟 IPC 通道：`ShmNotifier`（modsrv）→ `/tmp/aether-m2c.sock` → `ShmCommandListener`（comsrv），延迟约 1–2ms。

### 问题

`aether-rules` 的 `RuleScheduler`（`libs/aether-rules/src/scheduler.rs`）每 100ms tick 一次。`OnChange` 触发器的检测链路（`scheduler.rs:680–727`）是：

```
SHM 写入（comsrv）
  → ShmRedisSync（后台同步，延迟 0–100ms）
    → Redis Hash（inst:{id}:M）
      → scheduler.tick() Phase 0 fetch_point_snapshot（每 100ms 一次）
        → should_trigger_onchange 比较 last_value
```

端到端延迟：**0–200ms 最坏情况**，典型约 100ms。这对储能系统"电网并/离网切换"等需要 <20ms 响应的场景完全不够用。

### 目标

在 comsrv 写入 T/S 槽后的 **<5ms 内**，将变化事件推送给 modsrv 规则引擎。命名锁定：
- `PointWatchSignaler`（comsrv 侧，发送方）
- `PointWatchListener`（modsrv 侧，接收方）
- Socket 路径：`/tmp/aether-point-watch.sock`

---

## 2. 架构总览

```
comsrv                                  modsrv
┌─────────────────────────────┐        ┌────────────────────────────────────┐
│  SlotWriter::set_direct()   │        │  PointWatchListener                │
│  (hot path: T/S 写入)       │        │  ┌──────────────────────────────┐  │
│         │                   │        │  │  UDS 接收循环                │  │
│         ▼                   │        │  │  read_exact(56B)             │  │
│  [bitmap check]             │        │  │  → validate + dedup          │  │
│  watch_bitmap[slot/64]      │  UDS   │  │  → mpsc::Sender<PointEvent>  │  │
│  bit slot%64 是否为 1？      │ ─────► │  └──────────────────────────┘   │  │
│         │ 是                │        │           │                       │  │
│         ▼                   │        │           ▼                       │  │
│  mpsc::Sender<PointEvent>   │        │  PointWatchDispatcher             │  │
│  (bounded 2048)             │        │  ┌──────────────────────────────┐ │  │
│         │                   │        │  │  sub_index:                  │ │  │
│  drain task                 │        │  │  DashMap<(ch,pt,pid),        │ │  │
│  (tokio::spawn)             │        │  │    Vec<rule_id>>             │ │  │
│  batch_send(UDS socket)     │        │  │                              │ │  │
└─────────────────────────────┘        │  │  → 直接唤醒 rule executor    │ │  │
                                       │  └──────────────────────────┘   │  │
                                       │                                   │  │
                                       │  RuleScheduler (100ms tick 保留) │  │
                                       └────────────────────────────────────┘

数据流（上行遥测）：
Device → comsrv protocol → SlotWriter::set_direct
                         → [PointWatchSignaler emit，if bit set]
                         → UDS → PointWatchListener
                                → PointWatchDispatcher
                                  → 唤醒对应规则执行器（<5ms）
                         → ShmRedisSync（100ms 后台同步，保持不变）
```

---

## 3. 数据结构

### 3.1 PointWatchEvent（新 56 字节报文）

不复用现有 `ShmNotification`（48 字节，专为 M2C C/A 命令设计）。PointWatch 在语义上是一条"遥测变化通知"，携带 value 以供 modsrv 跳过再次读 SHM，同时加 `slot_index` 供订阅索引快速 dispatch。

```rust
/// PointWatch 事件报文（56 字节，固定大小）
///
/// 由 PointWatchSignaler 在 comsrv set_direct 热路径上生成，
/// 通过 UDS 发送给 modsrv PointWatchListener。
///
/// # 字段对齐说明
///
/// repr(C) + 显式 padding，保证无隐式填充，bytemuck::Pod 安全推导。
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PointWatchEvent {
    /// Channel ID（4B）
    pub channel_id: u32,
    /// Point ID（4B）
    pub point_id: u32,
    /// Point type（1B）: 0=Telemetry, 1=Signal（只有 T/S comsrv 会发送）
    pub point_type: u8,
    /// 对齐 padding（7B）
    pub _padding: [u8; 7],
    /// 工程值，f64::to_bits() 编码（8B）
    pub value_bits: u64,
    /// 原始值，f64::to_bits() 编码（8B）
    pub raw_bits: u64,
    /// SHM 槽索引（8B）— 供 modsrv 绕过反向查表直接读槽
    pub slot_index: u64,
    /// 时间戳，毫秒（8B）
    pub timestamp_ms: u64,
    /// 生产者标识（进程启动时随机生成，8B）
    pub producer_id: u64,
}

const _: () = assert!(std::mem::size_of::<PointWatchEvent>() == 56);
```

**字节布局**（偏移）：
```
0-3:    channel_id     (u32)
4-7:    point_id       (u32)
8:      point_type     (u8)
9-15:   _padding       (7B)
16-23:  value_bits     (u64)
24-31:  raw_bits       (u64)
32-39:  slot_index     (u64)
40-47:  timestamp_ms   (u64)
48-55:  producer_id    (u64)
```

> **为什么不加 `seq` 字段？**
> M2C 通知需要 seq 是因为同一个控制点在极短时间内可能发多条命令，comsrv 需要去重。PointWatch 方向语义不同：modsrv 收到一个 event 后直接读 SHM（或使用 event 中携带的 value），重复的新 event 只会让规则多触发一次；业务上幂等（规则条件重新判断），不需要去重。如果未来需要限流，应在 comsrv 侧做 value deadband 过滤（见第 8 节）。

### 3.2 订阅 Bitmap（PointWatchSignaler 侧）

**采用 Option A：独立 mmap 文件**

文件路径：`/shm/rtdb/aether-point-watch-subs.shm`（Docker）或 `/tmp/aether-point-watch-subs.shm`（macOS/测试）。

```rust
/// 订阅 bitmap，逐槽标记 modsrv 是否关心该槽
///
/// 每个 u64 word 覆盖 64 个槽。MAX_SLOTS = 100_000，需 1563 个 word。
/// 这个文件由 modsrv 写（规则加载时更新），comsrv 读（set_direct 热路径）。
pub struct PointWatchSubsBitmap {
    /// mmap 映射区域，[AtomicU64; WATCH_WORDS_COUNT]
    mmap: MmapMut,
    /// 槽总数（与主 SHM 的 slot_count 一致）
    slot_count: usize,
}

/// 1563 words × 64 bits = 100,032 个槽（覆盖 DEFAULT_MAX_SLOTS = 100_000）
pub const WATCH_WORDS_COUNT: usize = 1563;
/// bitmap 文件大小（字节）
pub const WATCH_BITMAP_SIZE: usize = WATCH_WORDS_COUNT * 8; // 12,504 字节

impl PointWatchSubsBitmap {
    /// 检查 slot 是否被订阅（comsrv hot path 调用）
    #[inline]
    pub fn is_watched(&self, slot: usize) -> bool {
        let word_idx = slot / 64;
        let bit_idx = slot % 64;
        // SAFETY: mmap 起始地址对齐到 AtomicU64
        let words = unsafe {
            std::slice::from_raw_parts(
                self.mmap.as_ptr() as *const AtomicU64,
                WATCH_WORDS_COUNT,
            )
        };
        words
            .get(word_idx)
            .map(|w| w.load(Ordering::Relaxed) & (1u64 << bit_idx) != 0)
            .unwrap_or(false)
    }

    /// 设置订阅（modsrv 规则加载时调用）
    #[inline]
    pub fn set_watched(&self, slot: usize) {
        let word_idx = slot / 64;
        let bit_idx = slot % 64;
        // SAFETY: 同上
        let words = unsafe {
            std::slice::from_raw_parts(
                self.mmap.as_ptr() as *const AtomicU64,
                WATCH_WORDS_COUNT,
            )
        };
        if let Some(w) = words.get(word_idx) {
            w.fetch_or(1u64 << bit_idx, Ordering::Release);
        }
    }

    /// 清空所有订阅（规则重新加载前调用）
    pub fn clear_all(&self) { /* 遍历 words 写 0 */ }
}
```

**为什么选 Option A（独立文件）而不是 Option B（加到 UnifiedHeader）**：

1. `UnifiedHeader` 当前是 64 字节，已满，加 bitmap 需要扩展协议版本，影响现有 magic/version 验证。
2. bitmap 的写者是 modsrv（订阅更新），读者是 comsrv——与主 SHM 的写者所有权模型（comsrv = T/S，modsrv = C/A）不兼容，加到同一文件会引入跨进程写竞争。
3. 独立文件大小固定为 12,504 字节，可单独 mmap，comsrv 中 `is_watched()` 是纯 load，热路径无额外开销。

**为什么不选 Option C（HTTP push）**：

HTTP 增加 5–10ms 延迟且依赖 comsrv HTTP 端口在线，规则加载是一次性操作，不值得引入网络 I/O。

### 3.3 PointWatchSignaler（comsrv 侧）

```rust
/// comsrv 侧发送器。SlotWriter 的 set_direct 调用后触发。
///
/// 必须非阻塞：bitmap miss → 0ns，bitmap hit but channel full → drop。
pub struct PointWatchSignaler {
    /// 订阅 bitmap（mmap，modsrv 更新，comsrv 读）
    subs: Arc<PointWatchSubsBitmap>,
    /// SHM slot → (channel_id, point_type, point_id) 反向映射
    reverse_index: Arc<ReverseSlotIndex>,
    /// 发往 drain task 的 bounded channel
    tx: mpsc::Sender<PointWatchEvent>,
    /// 丢弃计数（可观测性）
    dropped_count: Arc<AtomicU64>,
    /// 生产者 ID（进程维度，启动时随机生成）
    producer_id: u64,
}

impl PointWatchSignaler {
    /// hot path：set_direct 完成后立即调用
    ///
    /// 约定：在 SlotWriter::set_direct 完成 seqlock write 之后，
    /// mark_dirty_slot 之前/之后均可（顺序不影响正确性）。
    #[inline]
    pub fn emit(&self, slot: usize, value: f64, raw: f64, timestamp_ms: u64) {
        // 1. bitmap 检查（Relaxed load，热路径最快路径）
        if !self.subs.is_watched(slot) {
            return; // 99% 的槽不被规则引擎订阅
        }

        // 2. 反向查表（Arc 内部是 Vec，O(1)）
        let Some(origin) = self.reverse_index.get(slot) else {
            return;
        };

        // 3. 构造事件，写入 bounded mpsc（非阻塞）
        let event = PointWatchEvent {
            channel_id: origin.channel_id,
            point_id: origin.point_id,
            point_type: origin.point_type as u8,
            _padding: [0; 7],
            value_bits: value.to_bits(),
            raw_bits: raw.to_bits(),
            slot_index: slot as u64,
            timestamp_ms,
            producer_id: self.producer_id,
        };

        // try_send：如果 channel 满直接丢弃，不阻塞 comsrv 热路径
        if self.tx.try_send(event).is_err() {
            self.dropped_count.fetch_add(1, Ordering::Relaxed);
        }
    }
}
```

### 3.4 PointWatchDispatcher（modsrv 侧）

```rust
/// 规则订阅索引 + 事件 dispatch
///
/// modsrv 加载/重载规则时重建此索引。
pub struct PointWatchDispatcher {
    /// (channel_id, point_id) → Vec<rule_id>
    ///
    /// 注意：point_type 不参与 key（T/S 均可触发规则，规则配置中的
    /// PointKind 在 should_trigger_onchange 时再做二次过滤）。
    /// 如果未来需要区分 T vs S 订阅，可扩展为 (channel_id, point_type, point_id)。
    sub_index: DashMap<(u32, u32), Vec<i64>>,
    /// 规则执行唤醒 sender（per-rule 或 global event queue）
    event_tx: mpsc::Sender<WatchEvent>,
}

/// 发往规则调度器的唤醒事件
#[derive(Debug, Clone)]
pub struct WatchEvent {
    pub rule_ids: Vec<i64>,
    pub channel_id: u32,
    pub point_id: u32,
    pub value: f64,
    pub raw: f64,
    pub timestamp_ms: u64,
}
```

### 3.5 drain task（comsrv 侧）

```rust
/// 从 bounded mpsc 消费事件并批量写 UDS socket。
///
/// 独立 tokio task，不在 set_direct 热路径上。
async fn drain_task(
    mut rx: mpsc::Receiver<PointWatchEvent>,
    socket_path: &str,
    dropped_count: Arc<AtomicU64>,
    shutdown: CancellationToken,
) {
    let mut stream: Option<UnixStream> = None;
    let mut backoff = ExponentialBackoff::new(1_000, 5_000); // 1s–5s

    loop {
        tokio::select! {
            event = rx.recv() => {
                let Some(event) = event else { break; };
                // 尝试批量 drain（最多 64 个事件，避免单次 syscall 过大）
                let mut batch = vec![event];
                while batch.len() < 64 {
                    match rx.try_recv() {
                        Ok(e) => batch.push(e),
                        Err(_) => break,
                    }
                }
                send_batch(&mut stream, &batch, socket_path, &mut backoff,
                           &dropped_count).await;
            }
            _ = shutdown.cancelled() => break,
        }
    }
}
```

---

## 4. 线路协议

### 4.1 帧格式（56 字节，固定长度，无帧头）

```
偏移  长度  字段          类型      说明
----  ----  -----------  --------  ----------------------------
   0     4  channel_id   u32 LE    通道 ID
   4     4  point_id     u32 LE    点位 ID
   8     1  point_type   u8        0=Telemetry, 1=Signal
   9     7  _padding     [u8;7]    全零
  16     8  value_bits   u64 LE    f64::to_bits(工程值)
  24     8  raw_bits     u64 LE    f64::to_bits(原始值)
  32     8  slot_index   u64 LE    SHM 槽索引（listener 可选用）
  40     8  timestamp_ms u64 LE    毫秒时间戳
  48     8  producer_id  u64 LE    comsrv 启动随机 ID
```

**协议选择理由**：
- 固定 56 字节 → `read_exact(&mut [u8; 56])` 无需 length-prefix，与现有 M2C 协议风格一致
- `bytemuck::Pod` 零拷贝序列化/反序列化
- 不加 `seq`：PointWatch 事件的幂等性由规则引擎的 deadband 逻辑保证（见第 8 节）
- `slot_index` 允许 modsrv listener 在不走反向查表的情况下直接读 SHM 确认值（可选优化）

---

## 5. comsrv 集成（发送侧）

### 5.1 在 set_direct 中埋点

修改 `libs/aether-rtdb-shm/src/core/writer.rs`，`set_direct`（第 133 行）：

```rust
// 当前
pub fn set_direct(&self, slot: usize, value: f64, raw: f64, timestamp_ms: u64) {
    assert!(slot < self.slot_count, ...);
    self.slot_at(slot).set(value, raw, timestamp_ms);
    self.mark_dirty_slot(slot);
    self.header().writer_heartbeat.store(timestamp_ms, Ordering::Relaxed);
}

// 修改后
pub fn set_direct(
    &self,
    slot: usize,
    value: f64,
    raw: f64,
    timestamp_ms: u64,
    // 新增可选参数：若为 Some，热路径完成后尝试 emit
    watcher: Option<&PointWatchSignaler>,
) {
    assert!(slot < self.slot_count, ...);
    self.slot_at(slot).set(value, raw, timestamp_ms);      // seqlock write 完成
    self.mark_dirty_slot(slot);
    self.header().writer_heartbeat.store(timestamp_ms, Ordering::Relaxed);

    // emit 在 seqlock write 之后 —— 保证 modsrv 读 SHM 时数据已 committed
    if let Some(w) = watcher {
        w.emit(slot, value, raw, timestamp_ms);
    }
}
```

**关键约束**：`emit` 必须在 `slot_at(slot).set()` 完成之后，这样 modsrv 通过 `slot_index` 读 SHM 时能看到最新值。`try_send` 是非阻塞的，emit 本身不会 block set_direct。

**调用链**：`SlotWriter` 目前有两条写路径：
1. `set_direct` — 直接写槽（热路径，`services/comsrv/src/core/channels/channel_task.rs` 调用）
2. `write_slot`（`SlotIoWrite` trait 实现）— 也需要同样添加 emit

`SlotIoWrite::write_slot`（`writer.rs:245`）也应调用 emit，实现方式相同。

### 5.2 drain task 设计

drain task 是一个 `tokio::spawn` 的后台任务，与 comsrv 生命周期绑定：

```
PointWatchSignaler (Arc)
  ↓ try_send（非阻塞）
mpsc::channel(capacity = 2048)
  ↓ async recv + batch drain
drain_task（tokio task）
  ↓ write_all(&bytes) × batch_size
UnixStream → /tmp/aether-point-watch.sock
```

**Channel 容量选择**：2048 个事件 × 56 字节 = 112 KB 内存。按 10,000 点/秒写入速率，drain task 每次批量发送 ≤64 个事件，单次 UDS write 约 3.5 KB，drain 频率 ~156 次/秒，远低于 drain task 能力。即使 UDS 短暂不通（modsrv 重启），2048 的 backlog 可以吸收约 200ms 的事件。超出丢弃，计数器可观测。

**连接管理**：drain task 持有 `Option<UnixStream>`：
- 连接正常 → 批量写
- 写失败 → 关闭连接，进入重连退避（1s–5s 指数退避，镜像 `ShmNotifier`）
- 重连期间收到的事件 → `try_send` 到 channel（未满则缓冲，满则 drop+计数）

---

## 6. modsrv 集成（接收侧）

### 6.1 PointWatchListener

`PointWatchListener` 的结构完全镜像 `ShmCommandListener`（`services/comsrv/src/core/channels/shm_listener.rs`）：

```rust
pub struct PointWatchListener {
    /// 规则 dispatch
    dispatcher: Arc<PointWatchDispatcher>,
    socket_path: String,
    shutdown: watch::Receiver<bool>,
    dropped_count: Arc<AtomicU64>,
}

impl PointWatchListener {
    pub async fn run(&self) -> io::Result<()> {
        // 清理 stale socket（逻辑与 ShmCommandListener::run 第 70–102 行相同）
        // bind → accept loop → spawn handle_connection
    }

    async fn handle_connection(
        mut stream: UnixStream,
        dispatcher: Arc<PointWatchDispatcher>,
        mut shutdown: watch::Receiver<bool>,
        dropped_count: Arc<AtomicU64>,
    ) {
        let mut buf = [0u8; 56]; // PointWatchEvent::SIZE
        loop {
            tokio::select! {
                result = stream.read_exact(&mut buf) => {
                    match result {
                        Ok(_) => {
                            let event: PointWatchEvent = *bytemuck::from_bytes(&buf);
                            dispatcher.dispatch(event).await;
                        }
                        Err(e) if e.kind() == UnexpectedEof => break,
                        Err(_) => break,
                    }
                }
                _ = shutdown.changed() => break,
            }
        }
    }
}
```

**注意**：PointWatch 方向是 comsrv（发送方）主动连接 modsrv（监听方），与 M2C 方向相反。modsrv bind socket，comsrv connect。这样 modsrv 启动时就监听，comsrv 任何时候都可以连接（重试直到成功）。

### 6.2 订阅索引构建

`PointWatchDispatcher` 在 modsrv 的 `RuleScheduler::load_rules` 或 `reload_rules` 之后重建。

**重建流程**：

```rust
impl PointWatchDispatcher {
    /// 从已加载规则集合重建订阅索引
    pub fn rebuild_from_rules(
        rules: &[ScheduledRule],
        routing_cache: &RoutingCache,
    ) -> Self {
        let sub_index: DashMap<(u32, u32), Vec<i64>> = DashMap::new();

        for scheduled in rules {
            if !scheduled.rule.enabled {
                continue;
            }
            let TriggerConfig::OnChange { point_refs, .. } = &scheduled.trigger else {
                continue;
            };
            for pref in point_refs {
                // PointRef { instance, point_type, point } → (channel_id, point_id)
                // 通过 routing_cache 查询 instance → channel 映射
                if let Some((channel_id, point_id)) =
                    routing_cache.resolve_instance_point(pref.instance, pref.point)
                {
                    sub_index
                        .entry((channel_id, point_id))
                        .or_default()
                        .push(scheduled.rule.id);
                }
            }
        }
        // 同时更新 PointWatchSubsBitmap（通过 slot_index 查表）
        // ...
        Self { sub_index, ... }
    }
}
```

**订阅 bitmap 更新**：重建 `sub_index` 后，遍历所有被订阅的 `(channel_id, point_id)`，通过 `ChannelToSlotIndex.lookup(channel_id, point_type, point_id)` 查出 slot 索引，再调用 `PointWatchSubsBitmap::set_watched(slot)`。

**注意事项**：
- routing_cache 包含 `instance_id → channel_id + point_id` 的映射关系，需要确认 `RoutingCache` 有 `resolve_instance_point` 方法，如无，需在 `aether-routing` crate 加一个
- `PointRef` 中的 `instance` 字段是 `instance_id`，而 SHM slot 索引需要通过 `channel_id + point_type + point_id` 查 `ChannelToSlotIndex`；中间层需要 instance → channel 路由表

### 6.3 dispatch 逻辑

```rust
impl PointWatchDispatcher {
    pub async fn dispatch(&self, event: PointWatchEvent) {
        let key = (event.channel_id, event.point_id);
        let Some(rule_ids) = self.sub_index.get(&key) else {
            return; // 无规则订阅此点，快速返回
        };

        // 构造唤醒事件，发给 RuleScheduler 的 event_rx
        let watch_event = WatchEvent {
            rule_ids: rule_ids.clone(),
            channel_id: event.channel_id,
            point_id: event.point_id,
            value: f64::from_bits(event.value_bits),
            raw: f64::from_bits(event.raw_bits),
            timestamp_ms: event.timestamp_ms,
        };

        if self.event_tx.try_send(watch_event).is_err() {
            self.dropped_count.fetch_add(1, Ordering::Relaxed);
        }
    }
}
```

### 6.4 RuleScheduler 中的 event 消费

`RuleScheduler` 需要新增一个 `watch_rx: mpsc::Receiver<WatchEvent>` 字段，并在主循环中多路复用：

```rust
// scheduler.rs start() 修改
loop {
    tokio::select! {
        _ = tick_interval.tick() => {
            if let Err(e) = self.tick().await {
                error!("Tick err: {}", e);
            }
        }
        Some(watch_event) = self.watch_rx.recv() => {
            // 直接触发规则执行，不等待下一个 tick
            if let Err(e) = self.execute_watch_triggered(&watch_event).await {
                error!("Watch trigger err: {}", e);
            }
        }
        _ = self.shutdown.cancelled() => {
            info!("Scheduler shutdown");
            break;
        }
    }
}
```

`execute_watch_triggered` 只执行 `watch_event.rule_ids` 中的规则，不遍历全部规则。

---

## 7. 生命周期与错误处理

### 7.1 启动顺序

```
comsrv 启动：
  1. mmap 主 SHM（create/open）
  2. mmap PointWatchSubsBitmap（create，初始全零）
  3. 加载 ReverseSlotIndex
  4. 创建 PointWatchSignaler（含 bounded channel + drain task）
  5. drain task 尝试连接 /tmp/aether-point-watch.sock（modsrv 未启动时失败，进入退避）
  6. 开始正常协议服务

modsrv 启动：
  1. 等待 comsrv health（common::dependency::wait_for_dependency()，已有）
  2. bind /tmp/aether-point-watch.sock，启动 PointWatchListener
  3. mmap PointWatchSubsBitmap（open 已存在的文件）
  4. 加载规则 → 重建 PointWatchDispatcher → 更新 bitmap
  5. comsrv drain task 的重连退避触发，连接成功，开始接收 events
```

### 7.2 重连逻辑（drain task 侧，comsrv）

镜像 `ShmNotifier::try_reconnect`（`notifier.rs:270`）：

| 状态 | 行为 |
|------|------|
| 连接中 | 正常批量发送 |
| 写失败 | 关闭流，进入退避（1s 起步，指数翻倍，最大 5s） |
| 退避中 | 接收到 events 继续写 channel buffer；buffer 满则 drop + 计数 |
| 重连成功 | 退避重置，继续发送 |
| modsrv 完全不在 | drain task 永远处于退避循环，comsrv 不受影响 |

### 7.3 modsrv 重启

modsrv 重启时：
1. listener 重新 bind socket
2. 重载规则 → 重建 sub_index + bitmap
3. bitmap 更新后，comsrv `is_watched()` 读到新订阅，自然恢复

**modsrv 重启窗口（0–5s）内**：comsrv 的 drain task 处于退避，事件被缓冲或丢弃。100ms tick fallback 兜底（见第 8 节），规则引擎不会"漏掉"值，只是有额外延迟。

### 7.4 comsrv 重启

comsrv 重启时：
1. 重新生成 `producer_id`（时间戳 XOR pid，与 `ShmNotifier::new_producer_id` 相同逻辑）
2. 重建 ReverseSlotIndex
3. 重新 mmap bitmap（文件可能有旧订阅，清空或直接复用均可；建议清空后重新由 modsrv 填写）

**建议**：comsrv 每次创建 bitmap 文件时清零，等 modsrv 重新填写。这样避免 stale bitmap 导致 comsrv 热路径做无效 emit。

### 7.5 背压与溢出策略

| 位置 | 机制 | 溢出策略 |
|------|------|---------|
| `SlotWriter::emit → tx.try_send` | bounded mpsc(2048) | **丢弃 + dropped_count++** |
| `drain_task → UDS` | 系统 socket buffer（默认 ~256KB） | 写失败 → 重连退避 |
| `PointWatchListener → dispatcher.event_tx` | bounded mpsc(1024) | **丢弃 + dropped_count++** |
| `dispatcher.event_tx → RuleScheduler` | bounded mpsc(1024) | 同上 |

**可观测性**：comsrv health API（`/health`）现有 `dropped_count` 字段（参考 `modsrv` 中的实现），PointWatchSignaler 的 `dropped_count` 应该暴露在同一接口。

---

## 8. OnChange 语义变化

### 8.1 两路并行（推荐实现方式）

**保留 100ms tick 作为 fallback，新增事件驱动路径，两路并行运行**：

```
事件驱动路径：slot write → UDS → dispatch → 规则立即执行（目标 <5ms）
tick fallback：每 100ms snapshot → 检查 last_value → 执行未被事件唤醒的规则
```

这样的好处：
1. 不影响现有 OnChange 规则（零迁移成本）
2. UDS 断线时，tick fallback 自动兜底（降级为 <200ms 延迟，不是硬失败）
3. 两路都会触发同一规则，但规则幂等（前提：deadband 仍然有效）

### 8.2 deadband 在哪里评估

**决策：deadband 在 modsrv 侧评估（`execute_watch_triggered` 中），而不是 comsrv 侧过滤**。

理由：
- comsrv 不知道每条规则的 deadband 配置，除非开一条反向订阅 API
- 在 modsrv 侧评估与 tick 路径的评估逻辑保持一致（同一个 `should_trigger_onchange` 函数）
- comsrv bitmap 已经过滤了"无人订阅的点"，进入 dispatch 的 event 数量已经大幅减少

**`execute_watch_triggered` 的 deadband 评估**：

```rust
async fn execute_watch_triggered(&self, watch_event: &WatchEvent) -> Result<()> {
    let now = Instant::now();
    let rules = self.rules.read().await;

    for rule_id in &watch_event.rule_ids {
        let Some(scheduled) = rules.iter().find(|s| s.rule.id == *rule_id) else {
            continue;
        };
        if !scheduled.rule.enabled {
            continue;
        }
        let TriggerConfig::OnChange {
            point_refs,
            time_deadband_ms,
            value_deadband,
        } = &scheduled.trigger else {
            continue;
        };

        // 构造单点 snapshot（只有触发的这个点）
        let mut snapshot = HashMap::new();
        let key = /* 从 watch_event.channel_id + point_id 反查 PointRef cache_key */ ...;
        snapshot.insert(key, Some(watch_event.value));

        if should_trigger_onchange(
            &scheduled.onchange_state,
            point_refs,
            *time_deadband_ms,
            value_deadband.as_ref(),
            &snapshot,
            now,
        ) {
            // 执行规则（直接调用 executor.execute，不持锁）
            drop(rules); // 释放读锁
            // ... 执行 + 更新 onchange_state
            return Ok(());
        }
    }
    Ok(())
}
```

**注意**：`execute_watch_triggered` 中的 `snapshot` 只包含触发该事件的那一个点，而 tick 路径中的 snapshot 包含所有订阅点。对于多点 OnChange 规则（`point_refs` 长度 > 1），单点事件触发时 `should_trigger_onchange` 会检查所有 `point_refs`，其中未更新的点不在 snapshot 里，会被 skip（逻辑在 `scheduler.rs:176–184`）。

这意味着**多点 OnChange 规则在纯事件模式下，只有当触发点恰好满足 deadband 时才执行**；其余情况退化到 100ms tick fallback。这是正确行为：多点 AND 语义，任一点变化则触发，tick 最终会以完整 snapshot 评估。

### 8.3 TriggerConfig 无需修改

现有 `TriggerConfig::OnChange` 结构体（`scheduler.rs:126–132`）不需要添加字段。`time_deadband_ms` 和 `value_deadband` 在事件触发路径中同样适用。

---

## 9. 测试策略

### 9.1 PointWatchEvent 单元测试

```rust
#[test]
fn test_event_size_and_layout() {
    assert_eq!(std::mem::size_of::<PointWatchEvent>(), 56);
    assert_eq!(std::mem::align_of::<PointWatchEvent>(), 8);
}

#[test]
fn test_event_roundtrip() {
    let event = PointWatchEvent {
        channel_id: 1001,
        point_id: 42,
        point_type: 0, // Telemetry
        _padding: [0; 7],
        value_bits: 220.5f64.to_bits(),
        raw_bits: 2205.0f64.to_bits(),
        slot_index: 500,
        timestamp_ms: 1748430000000,
        producer_id: 0xDEADBEEF,
    };
    let bytes: [u8; 56] = *bytemuck::bytes_of(&event);
    let decoded: PointWatchEvent = *bytemuck::from_bytes(&bytes);
    assert_eq!(event, decoded);
}
```

### 9.2 PointWatchSubsBitmap 单元测试

```rust
#[test]
fn test_bitmap_set_and_check() {
    let bitmap = PointWatchSubsBitmap::new_in_memory(100_000);
    assert!(!bitmap.is_watched(0));
    bitmap.set_watched(63);
    assert!(bitmap.is_watched(63));
    assert!(!bitmap.is_watched(64));
    bitmap.set_watched(99_999);
    assert!(bitmap.is_watched(99_999));
}
```

### 9.3 PointWatchSignaler 单元测试（无 UDS）

`PointWatchSignaler` 接受 `mpsc::Sender`，测试时直接检查 channel 中的事件：

```rust
#[test]
fn test_signaler_emit_hit() {
    let (tx, mut rx) = mpsc::channel(16);
    let bitmap = Arc::new(PointWatchSubsBitmap::new_in_memory(1000));
    bitmap.set_watched(5);
    // 构造 reverse_index 包含 slot=5 → (ch=1001, T, pid=0)
    let signaler = PointWatchSignaler::new(bitmap, reverse_index, tx, ...);

    signaler.emit(5, 220.0, 2200.0, 1000);
    let event = rx.try_recv().unwrap();
    assert_eq!(event.channel_id, 1001);
    assert_eq!(f64::from_bits(event.value_bits), 220.0);
}

#[test]
fn test_signaler_emit_miss() {
    // slot 5 未订阅，emit 后 channel 应为空
    signaler.emit(5, 220.0, 2200.0, 1000);
    assert!(rx.try_recv().is_err());
}

#[test]
fn test_signaler_overflow_increments_dropped() {
    // channel capacity = 1，发 2 个事件
    let (tx, _rx) = mpsc::channel(1);
    bitmap.set_watched(5);
    signaler.emit(5, 220.0, 2200.0, 1000);
    signaler.emit(5, 221.0, 2210.0, 1001); // 第 2 个 drop
    assert_eq!(signaler.dropped_count(), 1);
}
```

### 9.4 端到端集成测试

模拟真实 UDS 连接的集成测试（参考 `shm_listener.rs` 中的测试结构）：

```rust
#[tokio::test]
async fn test_point_watch_end_to_end() {
    let socket_path = format!("/tmp/test-pw-{}.sock", std::process::id());

    // 启动 listener（modsrv 侧）
    let listener_task = tokio::spawn(async move {
        // ... bind + handle 1 event
    });

    // 启动 signaler + drain task（comsrv 侧）
    tokio::time::sleep(Duration::from_millis(10)).await; // 等 listener ready
    signaler.emit(slot, 220.0, 2200.0, now_ms());

    // 验证 listener 收到事件
    let event = received_events.recv().await.unwrap();
    assert_eq!(f64::from_bits(event.value_bits), 220.0);

    // 延迟断言（不超过 5ms）
    // ...
}
```

### 9.5 OnChange 语义回归测试

在 `should_trigger_onchange` 的现有测试套件（`scheduler.rs:790` 后）中补充：
- 单点 snapshot（事件驱动场景）在 deadband 内不触发
- 单点 snapshot 在 deadband 外触发
- 多点规则仅收到一点 snapshot 时的行为（参考第 8.2 节描述）

---

## 10. 迁移路径

### Phase 1：基础设施（不改变任何业务行为）

1. 在 `libs/aether-rtdb-shm/` 新增 `point_watch.rs` 模块：
   - `PointWatchEvent` 结构体 + bytemuck 实现
   - `PointWatchSubsBitmap`（mmap 文件封装）
   - `PointWatchSignaler`（含 bounded channel + `emit`）
2. 在 `services/comsrv/` 新增 `core/point_watch/` 目录：
   - `drain_task.rs`（UDS 写循环 + 重连退避）
3. **`SlotWriter::set_direct` 的签名不改变**：新增 `watcher` 参数设为 `Option<&PointWatchSignaler>`，全部调用方传 `None`（编译通过，行为不变）

### Phase 2：comsrv 热路径接入

1. comsrv bootstrap 初始化 `PointWatchSignaler`，drain task 开始运行
2. 真正的 `channel_task.rs` 调用点改为传入 `Some(&signaler)`
3. 验证：comsrv health API 的 `pw_dropped_count` 字段出现

### Phase 3：modsrv Listener 接入

1. 新增 `services/modsrv/src/infra/point_watch_listener.rs`（镜像 `channel_health.rs`）
2. `PointWatchDispatcher`：只 dispatch，不执行规则（先验证 event 格式和延迟数据）
3. 加 metrics：事件接收数、dispatch 延迟（收到 event → 准备执行的时间）

### Phase 4：规则引擎事件驱动接入

1. `RuleScheduler` 加 `watch_rx` + `execute_watch_triggered`
2. 上线后同时运行 tick + 事件，观察重复执行比例
3. 如果两路都触发同一规则，规则幂等（deadband 兜底），安全

### Phase 5（可选）：纯事件模式

如果 Phase 4 验证 <5ms 延迟稳定，可将 `OnChange` 规则的 tick 路径改为"仅 fallback"：只有在 watch_rx 没有 event 的 tick 才做 Redis snapshot。这是一个性能优化，不是功能变更，可以视情况推进。

---

## 11. 开放问题

### Q1. RoutingCache 是否有 `resolve_instance_point` 方法？

`aether-routing` 的 `RoutingCache` 目前主要用于 M2C 路由查找（`channel_id → route_config`）。从 `instance_id + point_id` 到 `channel_id + point_id` 的反向查找，目前通过 SQLite 的路由表完成（`route:m2c` 表）。

**需确认**：`PointWatchDispatcher::rebuild_from_rules` 中的 `routing_cache.resolve_instance_point(instance_id, point_id)` 是否可行，还是需要在 `aether-routing` crate 新增接口，或者直接查 SQLite。

建议：新增 `RoutingCache::instance_to_channel(instance_id: u32, point_id: u32) -> Option<(u32, u32)>` 方法，内部查 SQLite 缓存表。

### Q2. `set_direct` 签名变更的影响范围

`SlotWriter::set_direct` 目前有多个调用方：
- `services/comsrv/src/core/channels/channel_task.rs`（热路径）
- `libs/aether-rtdb-shm/src/batch_direct.rs`（批量写）
- 其他使用 `SlotIoWrite::write_slot` trait 方法的地方

**需确认**：是否有 crate 边界问题（`PointWatchSignaler` 在 `aether-rtdb-shm` 里，`SlotWriter` 也在同一 crate，应该没问题；但 `ReverseSlotIndex` 也在同一 crate，可以内部引用）。

如果 `SlotWriter` 不适合持有 `PointWatchSignaler`（循环依赖或 crate 边界），可以将 emit 调用移到 `UnifiedWriter` 层（`unified_shm.rs`），只在 `UnifiedWriter::set_direct` 包装里调用。

### Q3. bitmap 文件由谁 create

当前方案：comsrv 创建文件（清零），modsrv open 并写入订阅位。这与主 SHM 的"comsrv create，modsrv open"逻辑一致。

**需确认**：comsrv 和 modsrv 启动时间差。如果 modsrv 先于 comsrv 启动（不应该，`wait_for_dependency` 保证 comsrv 先），modsrv 无法 open bitmap 文件。

**建议**：bitmap 文件的 open 失败时，modsrv 以降级模式运行（只用 tick fallback），不影响功能，等 comsrv 就绪后重建 dispatcher。

### Q4. 高频点的 emit 压力

按目前设计，comsrv 每次 `set_direct` 都触发 `is_watched()` 检查（一个 Relaxed load）。100,000 个槽，假设 100 个订阅，`is_watched` miss 率 99.9%，miss 路径是一次 load + branch + return，约 1–2ns，无问题。

但是：被订阅的点如果以 1kHz 频率更新（某些 SHM 配置），会产生 1000 events/秒进入 bounded channel。channel 容量 2048，drain task 批量发送，UDS 写速度远大于 1000×56 = 56 KB/s。理论上没有瓶颈。

**需确认**：最极端场景，全部 100,000 个点被订阅，每个点 100Hz → 10M events/秒。此时 `is_watched` 的 miss 率为 0，每次都会 `try_send`。`try_send` 的 channel 容量 2048，约 **99.9%+ 的事件被丢弃**。

这是已知的设计取舍：热路径不能 block，overflow 就丢弃。如果真的需要 10M/s 所有点的 watch，需要重新考虑 batch 批量事件而不是 per-event UDS 写。**但这不是当前使用场景**（规则引擎只订阅少数关键点）。

### Q5. `producer_id` 去重机制

PointWatch 事件没有 `seq` 字段（见第 3.1 节理由）。如果 drain task 重连后，旧的 events 被重新发送（理论上 bounded channel 可能缓存了重连前的 event），modsrv 会执行重复的规则触发。

**这是否有问题？** 规则触发是幂等的（deadband 会过滤噪声），重复触发最坏情况是多执行一次规则，规则本身不应有副作用。对于控制操作（写 C/A），规则执行会通过 ShmDispatch 路径写入，ShmDispatch 已有自己的 seq 保护。

如果未来需要严格去重，可在 `PointWatchEvent` 中加 `seq` 字段，listener 侧做与 `ShmCommandListener` 相同的 `DashMap<(ch,pt,pid), (last_producer, last_seq)>` 检查。

### Q6. 多个 OnChange 规则订阅同一点时的 dispatch 并发

`PointWatchDispatcher::sub_index.get((channel_id, point_id))` 返回 `Vec<rule_id>`，然后 `executor.execute` 并发执行多条规则（`buffer_unordered`）。多条规则可能同时写同一个 C/A 槽。这不是 PointWatch 引入的新问题（tick 路径也会同时执行多规则），但需要确认 `ShmDispatch` 的单写者约束（comsrv 写 T/S，modsrv 写 C/A，但两条规则写同一 C/A 点会冲突）。

**推荐**：这是规则配置的约束，文档中明确禁止两条规则同时写同一 C/A 点。由规则 linter 或 `aether sync` 检查。

---

## 附录：相关文件索引

| 文件 | 关系 |
|------|------|
| `libs/aether-rtdb-shm/src/notifier.rs` | M2C 方向 UDS 发送器（参考实现） |
| `libs/aether-rtdb-shm/src/notification.rs` | M2C 48 字节报文（参考 layout） |
| `libs/aether-rtdb-shm/src/core/writer.rs:133` | `set_direct` 埋点位置 |
| `libs/aether-rtdb-shm/src/core/slot.rs` | `PointSlot` seqlock 实现 |
| `libs/aether-rtdb-shm/src/core/header.rs` | `UnifiedHeader`（64B，已满） |
| `libs/aether-rtdb-shm/src/reverse_index.rs` | `ReverseSlotIndex`（slot→origin 反向映射） |
| `libs/aether-rules/src/scheduler.rs:680` | `fetch_point_snapshot`（当前 Redis 依赖点） |
| `libs/aether-rules/src/scheduler.rs:109` | `TriggerConfig::OnChange`（无需修改） |
| `libs/aether-rules/src/scheduler.rs:157` | `should_trigger_onchange`（事件路径复用） |
| `services/comsrv/src/core/channels/shm_listener.rs` | M2C 方向 UDS 接收器（参考实现） |
| `docs/plans/2026-05-24-redis-removal-strategy.md` | 整体 Redis 去除战略 |
