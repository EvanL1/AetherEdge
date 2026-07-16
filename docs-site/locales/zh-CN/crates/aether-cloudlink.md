---
title: "aether-cloudlink"
description: "实验性摘要固定公共 AetherContracts CloudLink 子集的传输中立实现。它提供严格的封闭式 JSON 解码，..."
updated: 2026-07-16
---

# aether-cloudlink

实验性、摘要固定的公共 AetherContracts CloudLink 子集的传输中立实现。它提供严格的封闭式 JSON 解码、RFC 8785 业务摘要、会话/版本/纪元验证、稳定的交付信封、运行时清单校验和重用以及真实的 `PointSample` 映射。

此包不包含 MQTT 客户端，也不包含设备控制消息。匹配的 AetherCloud 编解码器使用相同的导入装置，同时三个公共行为工件和所有生产互操作性大门保持开放；请参阅 ADR-0017、ADR-0018 和 `contracts/cloudlink/`。
```bash
cargo test -p aether-cloudlink
```
