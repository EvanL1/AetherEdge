---
title: "AetherContracts 产品概览"
description: "AetherContracts 是 AetherEdge、AetherCloud 与独立实现共同使用的语言中立互操作权威。"
updated: 2026-07-16
---

# AetherContracts 产品概览

AetherContracts 是 AetherEdge、AetherCloud 与独立实现共同使用的语言中立互操作权威。规范定义语义，JSON Schema Draft 2020-12 定义结构，测试夹具固定可观察示例，黑盒 TCK 提供可执行一致性证据。产品仓库中的副本或语言绑定都不能成为第二套事实来源。

在 AI 原生架构中，AetherContracts 负责让智能体只能引用真实存在、带版本且可以验证的能力。当前版本提供 Thing Model 与 CloudLink 基础；未来如果增加意图、方案、策略或自动化契约，也必须先完成规范、Schema、测试夹具和 TCK，不能依靠提示词自行约定。

## 当前版本

`v0.1.0-alpha.3` 冻结了实验性 CloudLink 线格式、配置档和 TCK，并提供 TypeScript、Rust、C 与 C++ 测试夹具绑定。它不是生产 CloudLink 切换版本，也不包含完整的智能体意图编译契约。

这个版本在已经签名和摘要锁定的制品中保留历史 AetherIot 消费者名称。仓库改名为 AetherEdge 后，这些历史字节仍保持不变；后续版本可以更新显示名称，但不能随意更改协议标识。

继续阅读 [AI 原生平台](/overview/ai-native-platform/)、[AetherContracts 快速开始](/aethercontracts/getting-started/)、[兼容性矩阵](/compatibility/version-matrix/)和[边缘、契约与云端联动教程](/tutorials/edge-contracts-cloud/)。
