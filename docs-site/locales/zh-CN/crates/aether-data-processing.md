---
title: "Aether 数据处理"
description: "适用于 Aether 数据处理 v1 处理器边界的严格、传输中立的 JSON 编解码器。它将经过验证的域值与版本化的域值相互转换..."
updated: 2026-07-12
---

# Aether 数据处理

适用于 Aether 数据处理 v1 处理器边界的严格、传输中立的 JSON 编解码器。它将经过验证的域值与版本化 RFC 3339/JSON DTO 进行相互转换，对 Aether 接受的 `DerivedData` 进行编码，并计算 RFC 8785 输入摘要。

v1 有线格式使用精度不超过毫秒的 UTC `Z` 时间戳，保留可选的外部预测 `SourceProvenance.issued_at`，并且仅接受`interval_end` 预测时间戳语义。在传输公开域值之前，契约无效或未知字段无法关闭。 `DerivedDataDto` 和 `encode_derived_data` 有意仅进行编码：外部 JSON 负载无法通过声称已被 Aether 接受来跨越边界。接受的信封保留了处理器契约、不可变的工件出处、后备元数据、警告代码和Aether计算的帧质量。

工件版本/摘要是身份，而不是时间顺序：v1 没有 `trained_through` 或 `available_at`。编解码器也无法将事件时间 `as_of` 转换为双时态历史记录剪辑；这些保证属于未来的源和调试契约。

此箱子不包含 HTTP 客户端、存储访问、回调或模型运行时。协议适配器依赖于它；该域不依赖于任何适配器。

请参阅[规范契约参考](/reference/data-processing-contracts)、[JSON Schemas](https://github.com/EvanL1/AetherEdge/blob/main/contracts/data-processing/README.md) 和可选的[HTTP 适配器](/extensions/http-data-processor)。
```bash
cargo test -p aether-data-processing
```
