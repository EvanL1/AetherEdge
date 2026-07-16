---
title: "aether-shm-bridge"
description: "从 Aether 的权威共享内存数据平面到公共 `LiveState` 端口的只读功能桥接。"
updated: 2026-07-11
---

# aether-shm-bridge

从 Aether 的权威共享内存数据平面到公共 `LiveState` 端口的只读功能桥。

该桥验证逻辑通道清单、支持每个使用方的 PointWatch 位图和 UDS 提示、报告通道运行状况，并在编写器重新启动或原子 SHM 文件替换后重新连接。它不依赖于 Redis 或 PostgreSQL，并且从不授予消费者获取写入者权限。
```bash
cargo test -p aether-shm-bridge
```

您可以选择根据 MIT 或 Apache-2.0 获得许可。
