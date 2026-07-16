---
title: "AetherEdge 产品概览"
description: "AetherEdge 是 AetherIoT 的确定性边缘运行时、内核、CLI 和 Rust SDK，负责实时状态与最终物理执行。"
updated: 2026-07-16
---

# AetherEdge 产品概览

AetherEdge 是 AetherIoT 的确定性边缘运行时。它连接现场设备，掌握实时状态，在本地运行已经投运的规则、告警和历史记录，并负责最终物理执行。

在 AI 原生架构中，智能体通过公开能力发现、查询和受治理命令与 AetherEdge 交互。模型不进入数据采集、安全联锁或硬实时闭环；即使智能体、云端或互联网不可用，已经投运的行为仍然继续运行。

## 已经实现

- 六个独立运行时服务，负责数据采集、自动化、告警、历史、应用接口和上行链路。
- 以共享内存作为实时点与健康状态权威。
- 使用嵌入式 SQLite 保存期望状态、历史、审计和持久本地发件箱。
- `aether` CLI、受治理 HTTP 与 MCP 应用边界、领域 Pack 和 `aether-edge-sdk`。
- 已签名的 `v0.5.0` 源码、运行时、安装包和 CLI 发行制品。

## 实验性能力

- 与 Broker 厂商无关的 CloudLink MQTT v1 会话、遥测、重放和应用确认持久队列。
- 通过摘要锁定的 AetherContracts `v0.1.0-alpha.3` 消费与公开测试夹具执行。

这些实验证据不能证明生产认证、签名确认或端到端崩溃持久性，旧版 MQTT 仍是兼容默认路径。

## 尚未完成

最终用户对话智能体、意图到自动化编译、历史仿真和持续效果调整不属于当前 AetherEdge 测试版。AetherEdge 提供的是这些能力未来安全落地所需要的确定性执行基础。

仓库显示名称已经改为 AetherEdge。现有 crate、二进制、`aether` CLI、`aether-edge-sdk`、配置键、服务标识、安装包和协议标识在本次迁移中保持稳定。

从 [AI 原生平台](/overview/ai-native-platform/)、[智能体快速入门](/agent-quickstart/)或[快速开始](/guides/getting-started/)继续。
