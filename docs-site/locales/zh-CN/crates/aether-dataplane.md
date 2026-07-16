---
title: "aether-dataplane"
description: "`aether-dataplane` 是 Aether 的业务中立共享内存核心。它可以由嵌入式网关使用，无需 Redis、PostgreSQL、SQLx、路由..."
updated: 2026-07-14
---

# aether-dataplane

`aether-dataplane` 是 Aether 的业务中立共享内存核心。它可由嵌入式网关使用，无需 Redis、PostgreSQL、SQLx、路由模型或任何 HTTP 堆栈。

它拥有：

- 稳定的 64 字节标头和 32 字节对齐的 `PointSlot` 布局；
- seqlock 一致读取和单写入器原子更新；
- 只读具有 RAII 清理功能的可写 mmap 所有者；
- 进程本地脏槽跟踪；
- 槽分配位图和生成路径助手；
- 抗撕裂快照序列化。

Mmap 构造函数拒绝无法覆盖其声明容量的映射，或者在公开任何标头或槽引用之前其活动槽计数超过该容量的映射。公共故障使用 `DataplaneError`，允许主机区分无效布局、无效路径和操作系统 I/O 故障。只读读取器和通用 `SlotIo` 特征将标头值公开为 `HeaderSnapshot`，而不是可写原子单元。逻辑清单验证仍然是组合层的工作。
```bash
cargo test -p aether-dataplane
cargo tree -p aether-dataplane --edges normal
```

之前的 `aether-rtdb-shm` 聚合箱在其滚动 v4 兼容性契约通过后已退役。行业中立的代码直接依赖于这个箱子；通道感知组合属于 `aether-shm-bridge`。
