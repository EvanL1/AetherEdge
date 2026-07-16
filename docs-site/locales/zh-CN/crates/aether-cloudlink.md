---
title: "aether-cloudlink"
description: "实验性摘要固定公共 AetherContracts CloudLink 子集的传输中立实现。它提供严格的封闭式 JSON 解码，..."
updated: 2026-07-16
---

# aether-cloudlink

实验性、摘要固定的公共 AetherContracts CloudLink 子集的传输中立实现。它提供严格的封闭式 JSON 解码、RFC 8785 业务摘要、会话/版本/纪元验证、稳定的交付信封、运行时清单校验和重用以及真实的 `PointSample` 映射。

此 crate 不包含 MQTT 客户端或设备控制消息。配套的 AetherCloud 编解码器使用相同的导入测试夹具，但三个公共行为工件和全部生产互操作性门槛仍未关闭。当前行为与生产限制见 [CloudLink MQTT 参考](/reference/cloudlink-mqtt-v1)。
```bash
cargo test -p aether-cloudlink
```
