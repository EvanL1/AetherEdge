---
title: "aether-testkit"
description: "为 Aether 扩展作者提供可重用的一致性检查和确定性测试替身。"
updated: 2026-07-16
---

# aether-testkit

为 Aether 扩展作者提供可重用的一致性检查和确定性测试替身。

该套件验证实时状态往返和有序批量读取、持久发件箱的 FIFO 和确认行为、有界/来源保留 `HistoryQuery` 投影以及请求驱动的 `DataProcessor` 实现的精确描述符/请求/结果关联。处理器一致性还检查有限有序预测输出，并要求 `unavailable` 响应不包含派生输出。

`MemoryCloudLinkTransport::pair` 是用于会话/重放测试的传输中立伪绑定。它为持久发送发出传输发布的证据，但从不发明云应用程序 ACK，因此测试必须显式创建收据。

`ScriptedDataProcessor` 是队列驱动的 `DataProcessor` 测试替身。它报告可配置的运行状况结果，按 FIFO 顺序使用排队的 `ProcessingResult` 或 `PortError` 值，并保留收到的完整 `DataProcessingRequest` 值。因此，应用程序、适配器和示例测试可以证明发送到处理器的确切帧以及结果/错误路径，而无需模型运行时或网络服务。

扩展测试针对具体适配器调用一致性帮助程序，以便功能语义在本地和外部边界之间保持一致。
```bash
cargo test -p aether-testkit
```

您可以选择根据 MIT 或 Apache-2.0 获得许可。
