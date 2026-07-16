---
title: "aether-postgres-history"
description: "Aether 仅附加历史记录功能的可选 PostgreSQL 实现。"
updated: 2026-07-11
---

# aether-postgres-history

Aether 仅追加历史记录功能的可选 PostgreSQL 实现。

该扩展拥有其 SQL 架构和参数化写入。它仅取决于公共域和端口 crate，不是默认 SDK 或边缘组合的一部分。实时状态仍然具有权威性的SHM； PostgreSQL 仅当主机明确选择此适配器时才存储历史记录。
```bash
cargo test -p aether-postgres-history
```

您可以选择根据 MIT 或 Apache-2.0 获得许可。
