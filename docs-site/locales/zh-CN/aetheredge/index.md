---
title: "AetherEdge 产品总览"
description: "AetherEdge 是开源、行业中立的 Linux 边缘运行时、内核、CLI 和 Rust SDK，以前以 AetherIot 仓库名称发布。"
updated: 2026-07-16
---

# AetherEdge 产品总览

AetherEdge 是开源、行业中立的 Linux 边缘运行时、内核、CLI 和 Rust SDK，以前以 AetherIot 仓库名称发布。

## 今天实施

- 六个独立的运行时服务，用于采集、自动化、警报、历史记录、应用程序API 和上行链路。
- 当前点和运行状况的共享内存权限。
- 嵌入式 SQLite 所需状态、历史记录、审核和持久本地发件箱。
- `aether` CLI、受控 HTTP 和 MCP 应用程序边界、域包和`aether-edge-sdk` 外观。
- 已签名的 `v0.5.0` 源代码、运行时、安装程序和 CLI 版本。

## 今天处于实验阶段

- 代理中立的 CloudLink MQTT v1 会话、遥测、重播和应用程序确认持久队列。
- 摘要固定的 AetherContracts `v0.1.0-alpha.3` 消耗和公共装置执行。

实验 CloudLink 证据无法建立生产身份验证、签名确认或端到端崩溃持久性。旧版 MQTT 保留兼容性默认值。

## 稳定的兼容性名称

仓库显示名称更改为 AetherEdge。现有包名称、二进制名称、`aether`、CLI、`aether-edge-sdk`、配置密钥、服务标识、安装程序名称和协议标识符在此迁移中不会更改。

从[代理快速入门](/agent-quickstart)、[入门](/guides/getting-started) 或[迁移] 开始指南](/migration/aetheriot-to-aetheredge)。
