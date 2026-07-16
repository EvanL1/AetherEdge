---
title: "AetherContracts 兼容性与发布门槛"
description: "将 alpha.3 兼容性基线与保持开放的身份验证、持久性和遗留切换门区分开来"
updated: 2026-07-16
status: mixed
---

# AetherContracts 兼容性与发布门槛

> 权威来源：[AetherContracts](https://github.com/EvanL1/AetherContracts/blob/main/docs/compatibility.md)。此页面镜像到统一的 AetherIoT 文档中。

兼容性是基于证据的。共享版本字符串或成功解码一个装置并不能证明完全的互操作性。当前的 `v0.1.0-alpha.3` 版本冻结了实验性通用合约，同时将旧传输保留为默认值。

## 当前产品基线

| 产品 | 合约关系 | 当前状态 |
| --- | --- | --- |
| AetherEdge | 摘要固定的完整 alpha.3 消费者；严格的 Rust 编解码器和 MQTT 传输基础 | 实验消费者证据 |
| AetherCloud | 摘要固定的完整 alpha.3 消费者；严格的 TypeScript 编解码器、MQTT 入口和接受的遥测 ACK 切片 | 实验性消费者证据 |
| 独立实现 | 精确的发布存档、封闭的消费者锁、公共测试夹具和TCK | 支持的分发路径；一致性必须由消费者证明 |

两个产品消费者都导入相同的所需工件闭合并执行相同的公共固定结果。这证明了分发的完整性和共享的实验核心。它不证明生产身份、完整的状态机行为、崩溃持久性或安全遗留切换。

## CloudLink 门状态

| 门 | 状态 | 含义 |
| --- | --- | --- |
| 共享代理身份验证 | 提案 | 记录已冻结，但生产密钥配置，轮换、撤销和验证者所有权未实现 |
| 单线合约 | 实验性 | 核心信封、时间、身份、摘要和 ACK 语义具有一个公共权限 |
| 跨语言测试夹具 | 通过，实验性 | TypeScript，Rust、C 和 C++ 执行相同的测试夹具明显且稳定的故障类 |
| Real-Broker 双线束 | 需要消费者证据 | 产品必须通过应用程序用例证明并发的边缘和云行为 |
| 故障注入 | 需要消费者证据 | 断开连接、ACK 丢失、重新启动、重复、冲突和数据丢失结果需要产品证据 |
| 已签名持久ACK | 计划 | 签名投影、密钥生命周期和生产事实交易保持开放 |
| 传统切换 | 被阻止 | 每个前面的门必须通过并且回滚必须保持可用 |

机器可读权限是[`compatibility/cloudlink-v1alpha1-gates.json`](https://github.com/EvanL1/AetherContracts/blob/main/compatibility/cloudlink-v1alpha1-gates.json)。

## 绑定兼容性

| 绑定 | 已实现 alpha.3 表面 | 尚未声明 |
| --- | --- | --- |
| TypeScript | 规范 `uint64`、JSON 规范化、公共测试夹具清单 | 完整的生产 Schema 和传输编解码器 |
| Rust | 全范围规范 `u64`、类型化故障、公共设备清单 | 完整的生产 JSON、模型和传输编解码器 |
| C99 | 有界规范`uint64`、免分配 P/M/A 查找、有界夹具配置文件 | 完整的生产 JSON、模型和传输编解码器 |
| C++17 | C99 核心上的精简视图和结果 | 独立的连线语义或第二个编解码器 |
| Go、Java、 Python | 计划 | 当前未发布一致的绑定 |

稳定的字符串故障代码是约定的。数字错误值和消息文本仍然是特定于绑定的。在映射错误或重试行为之前，请先读取机器可读的 [`compatibility/failure-codes.json`](https://github.com/EvanL1/AetherContracts/blob/main/compatibility/failure-codes.json)。

## 兼容性规则

- 协议 `uint64` 值使用规范十进制字符串。
- 核心 JSON 对象已关闭并拒绝未知字段。
- 重复键、无效 Unicode、不安全数字和无界输入失败
- MQTT确认是传输证据，而不是持久的应用程序接受。
- Thing Model功能是声明，而不是授权。
- CloudLink不包含直接的物理控制操作。
- 后来的编码需要显式协商和它自己的TCK。

每个未来发布应发布确切的产品版本或提交并链接到可执行证据。浮动 `main`、`latest` 和隐含兼容性不是发布证据。
