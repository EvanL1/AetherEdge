---
title: "AetherContracts 入门"
description: "选择确切的实验版本，导航合约层，运行 TCK，并采用摘要固定消费者闭包"
updated: 2026-07-16
status: implemented
---

# AetherContracts 入门

> 权威来源：[AetherContracts](https://github.com/EvanL1/AetherContracts/blob/main/docs/getting-started.md)。此页面镜像到统一的 AetherIoT 文档中。

AetherContracts 是由 AetherEdge、AetherCloud 和独立实现共享的公共互操作性权威。从精确的版本开始，并将其规范、Schema、测试夹具、TCK 和清单放在一起。单个复制的 Schema 或生成的类型并不是完整的合约采用。

当前版本是 `v0.1.0-alpha.3`。它是实验性的，将旧传输保留为默认值，并且不是生产 CloudLink 切换版本。

## 选择契约面

| 需要 | 首先读取或运行 |
| --- | --- |
| 常见 JSON、整数、规范化和失败规则 | [基础](/aethercontracts/spec/foundation) |
| Thing Model结构和 P/M/A 迁移 | [Thing Model v1 alpha](/aethercontracts/spec/thing-model-v1alpha1) |
| CloudLink消息和生命周期 | [CloudLink v1 alpha](/aethercontracts/spec/cloudlink-v1alpha1) |
| 释放分发和使用方锁定 | [分发 v1 alpha](/aethercontracts/spec/distribution-v1alpha1) |
| 可执行一致性行为 | [TCK v1 alpha](/aethercontracts/spec/tck-v1alpha1) |
| 当前门和产品兼容性 | [兼容性](/aethercontracts/compatibility) |
| 约束力和消费者证据 | [一致性](/aethercontracts/conformance) |

规范性规范定义语义。 JSON Schema 定义结构接受度。夹具引脚示例和稳定的故障结果。 TCK 证明了可观察到的行为。语言绑定实现该契约，但永远不会成为第二个权威。

## 验证源签出

使用 Node.js 24 和仓库声明的 pnpm 版本：
```bash
git clone https://github.com/EvanL1/AetherContracts.git
cd AetherContracts
git checkout v0.1.0-alpha.3
corepack enable
pnpm install --frozen-lockfile
pnpm test:tck
```

`pnpm test:tck` 是独立的。它不需要代理、数据库、云帐户或边缘设备。更改仓库或验证所有 TypeScript、Rust、C 和 C++ 绑定基础时运行 `pnpm check`。

## 采用精确的版本

面向生产的使用方不应遵循浮动分支或复制未经验证的子集。提交封闭的 `aether-contracts.lock.json`，导入所需的确切工件闭包，然后运行发布的复合验证操作。验证者检查剥离的发布提交、清单摘要、工件哈希、采用关闭和可选的在线发布身份。

AetherEdge 和 AetherCloud 中签入的消费者副本演示了这种分发模型。他们的 alpha.3 证据不会升级身份验证、持久确认或旧版切换到生产状态。

## 选择一个绑定

- [TypeScript](/aethercontracts/packages/typescript)
- [Rust](/aethercontracts/packages/rust)
- [C99](/aethercontracts/packages/c)
- [C++17](/aethercontracts/packages/cpp)

所有四个绑定均执行公共测试夹具清单。它们是有意缩小的基础，而不是完整的生产传输编解码器。 Go、Java 和 Python 绑定仍在计划中。

如果现有使用方仍然引用以前的边缘仓库名称，请阅读[AetherEdge命名迁移](/aethercontracts/migration)。包和协议标识符保持稳定。
