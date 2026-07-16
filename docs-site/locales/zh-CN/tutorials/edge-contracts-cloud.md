---
title: "通过 AetherContracts 将 AetherEdge 连接到 AetherCloud"
description: "本教程证明了当前的跨仓库集成路径，而无需声明生产 CloudLink 准备就绪。它从安全的本地运行时开始，..."
updated: 2026-07-16
---

# 通过 AetherContracts 将 AetherEdge 连接到 AetherCloud

本教程证明了当前的跨仓库集成路径，而无需声明生产 CloudLink 已准备就绪。它从安全的本地运行时开始，验证共享合约发布，然后运行可用的产品证据。

## 1.选择兼容的基准

使用 AetherEdge `v0.5.0`、AetherContracts `v0.1.0-alpha.3` 以及消耗相同完整合约锁的 AetherCloud 修订版。确认[版本矩阵](/compatibility/version-matrix) 中的确切组合。

请勿遵循 `main`、`latest`、版本范围或同级签出合约行为。

## 2.安全地启动 AetherEdge

克隆 `EvanL1/AetherEdge`，然后运行无硬件的 SDK 组合：
```bash
cargo run -p aether-example-minimal-gateway
```

该组合无需调试任何设备，也不需要 Broker 或云服务。对于受监督的运行时安装，请遵循[入门指南](/guides/getting-started)。

## 3.验证公共契约授权

在 AetherContracts `v0.1.0-alpha.3` 结帐中：
```bash
pnpm test:tck
```

然后检查每个产品的已提交`aether-contracts.lock.json`。两个锁必须命名相同的发布标签、标签对象、提交、捆绑摘要、清单摘要、安全策略、精确导入和空待导入集。

验证程序证明发布分发完整性。它不证明生产编解码器、身份验证系统、代理部署或持久云存储。

## 4。执行边缘合约证据

在 AetherEdge 中，运行重点传输中立编解码器测试：
```bash
cargo test -p aether-cloudlink
```

测试路径可验证严格输入、规范摘要、重放行为、会话防护和当前遥测映射，而无需联系代理。

## 5.执行云契约证据

在AetherCloud中，运行默认仓库检查：
```bash
pnpm check
```

默认路径可验证严格的 TypeScript 编解码器、应用桥接器、内存和 PostgreSQL 适配器合约以及文档，而无需数据库、设备、Broker 或云帐户。

可选择加入的本地双进程 Broker 工具可作为开发证据：
```bash
pnpm test:cloudlink-alpha-harness
```

MQTT PUBACK 仅证明 Broker 传输接受。仅在验证了确切的云应用程序确认后，AetherEdge 才可以删除持久队列记录。当前的 alpha 确认仍未签名，并且完整的生产耐崩溃门尚未通过。

## 6。保留授权边界

- 不要通过 CloudLink 公开直接点、寄存器、SHM 或物理控制操作。
- 不要将报告的功能视为云授权。
- 不要将所需状态、报告状态和应用状态等同起来。
- 在联合身份验证、持久性、一致性、回滚和支持窗口门之前不要删除旧路径通过。

本教程的结果是可重复的 alpha 集成证据，而不是生产调试收据。
