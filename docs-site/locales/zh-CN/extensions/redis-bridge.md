---
title: "aether-redis-bridge"
description: "用于Aether点状态的可选非权威 Redis 镜像。"
updated: 2026-07-11
---

# aether-redis-bridge

用于Aether点状态的可选非权威 Redis 镜像。

此扩展实现 `StateMirror` 功能。它未由 `aether-sdk` 启用，不是默认边缘运行时的一部分，并且绝不能用作控制循环或实时状态事实来源。仅当外部集成明确需要 Redis 形状的投影时才部署它。
```bash
cargo test -p aether-redis-bridge
```

您可以选择根据 MIT 或 Apache-2.0 获得许可。
