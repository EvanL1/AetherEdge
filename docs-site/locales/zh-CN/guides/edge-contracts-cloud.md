---
title: "边缘端、公共协议与云端联动任务"
description: "验证 AetherEdge、AetherContracts 与 AetherCloud 的跨仓库联动，同时明确实验性能力和生产门槛。"
updated: 2026-07-16
---

# 边缘端、公共协议与云端联动任务

本任务验证当前可用的跨仓库联动路径，但不表示实验性 CloudLink 已经具备生产条件。你将先启动一个不接入硬件的本地运行时，再验证公共协议版本，最后运行边缘端与云端已有的联动检查。

## 1. 选择兼容基线

使用 AetherEdge `v0.5.0`、AetherContracts `v0.1.0-alpha.3`，并选择读取同一份完整契约锁定文件的 AetherCloud 版本。请先在[版本兼容矩阵](https://docs.aetheriot.workers.dev/compatibility/version-matrix/)中确认准确组合。

不要使用 `main`、`latest`、版本范围或相邻目录中的源码来推断契约行为。

## 2. 安全启动 AetherEdge

克隆 `EvanL1/AetherEdge`，然后运行不依赖硬件的 SDK 示例：

```bash
cargo run -p aether-example-minimal-gateway
```

该示例不会配对或控制任何设备，也不需要消息代理或云服务。如需安装受治理的运行时，请参阅[入门任务](https://docs.aetheriot.workers.dev/guides/getting-started/)。

## 3. 验证公共协议版本

切换到 AetherContracts `v0.1.0-alpha.3` 后运行：

```bash
pnpm test:tck
```

然后检查两个产品仓库中已经提交的 `aether-contracts.lock.json`。两份文件必须指向相同的发布标签、标签对象、提交、契约包摘要、清单摘要、安全策略和精确导入列表，而且待导入列表必须为空。

这项检查只能证明契约发布与分发的完整性，不能证明生产级编解码器、身份认证、消息代理部署或持久化云存储已经就绪。

## 4. 验证边缘端契约

在 AetherEdge 中运行与传输方式无关的编解码器测试：

```bash
cargo test -p aether-cloudlink
```

这些测试会验证严格输入、规范摘要、重放处理、会话隔离和当前的遥测映射，不会连接消息代理。

## 5. 验证云端契约

在 AetherCloud 中运行默认仓库检查：

```bash
pnpm check
```

默认检查会验证严格的 TypeScript 编解码器、应用桥接层、内存与 PostgreSQL 适配器契约以及文档，不需要连接数据库、设备、消息代理或云账户。

如需进一步验证，可以运行本地双进程消息代理测试：

```bash
pnpm test:cloudlink-alpha-harness
```

MQTT 的 `PUBACK` 只能证明消息代理已经接收数据。AetherEdge 必须验证对应的云端应用确认后，才能删除持久队列中的记录。当前的 alpha 确认仍未签名，也尚未通过完整的生产级崩溃恢复验证。

## 6. 保持控制边界

- 不要通过 CloudLink 暴露数据点、寄存器、共享内存或直接物理控制操作。
- 不要把设备报告的能力视为云端授权。
- 不要混淆期望状态、上报状态和实际应用状态。
- 在联合身份认证、持久化、一致性、回滚和支持周期全部通过验证前，不要移除旧路径。

本任务的结果是可重复的 alpha 集成证据，而不是生产投运回执。
