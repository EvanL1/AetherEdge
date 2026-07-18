---
title: "AetherContracts 入门"
description: "采用最新发布的 alpha.3，理解契约层次，运行 TCK，并把尚未发布的 alpha.4 与不可变发布版本分开。"
updated: 2026-07-17
status: implemented
version: 0.1.0-alpha.4
---

# AetherContracts 入门

> 本页是面向中文用户的使用说明。带标签版本中的英文规范、JSON Schema、测试夹具和 TCK 才是规范性依据。

AetherContracts 是 AetherEdge、AetherCloud 与独立实现共同使用的公开互操作权威。请从一个精确发布版本开始，并始终把规范、结构定义、测试夹具、TCK 和发布清单作为整体使用。只复制一份结构定义或生成一种类型，并不构成完整的契约采用。

最新发布版本是 `v0.1.0-alpha.3`。`0.1.0-alpha.4` 只是尚未发布的开发目标。两者都不是生产 CloudLink 切换版本，旧传输仍是默认选项。

## 选择契约范围

| 需求                                              | 首先阅读或运行                                                      |
| ------------------------------------------------- | ------------------------------------------------------------------- |
| 通用 JSON、整数、规范化与失败规则                 | [基础规范](/aethercontracts/spec/foundation/)                       |
| Thing Model 结构和 P/M/A 迁移                     | [Thing Model v1 alpha](/aethercontracts/spec/thing-model-v1alpha1/) |
| 委托提供方拓扑、类型化状态及其显式 CloudLink 扩展 | [Integration v1 alpha](/aethercontracts/spec/integration-v1alpha1/) |
| CloudLink 消息与生命周期                          | [CloudLink v1 alpha](/aethercontracts/spec/cloudlink-v1alpha1/)     |
| 发布分发与使用方锁                                | [分发规范 v1 alpha](/aethercontracts/spec/distribution-v1alpha1/)   |
| 可执行一致性行为                                  | [TCK v1 alpha](/aethercontracts/spec/tck-v1alpha1/)                 |
| 当前门槛与产品兼容性                              | [兼容性与发布门槛](/aethercontracts/compatibility/)                 |
| 语言绑定和使用方证据                              | [一致性与使用方验证](/aethercontracts/conformance/)                 |

规范性英文规范定义语义，JSON Schema 定义结构是否可接受，测试夹具固定示例和稳定失败结果，TCK 证明可观察行为。语言绑定实现契约，但永远不能成为第二个权威。

## 验证源码检出

使用 Node.js 24 和仓库声明的 pnpm 版本：

```bash
git clone https://github.com/EvanL1/AetherContracts.git
cd AetherContracts
git checkout v0.1.0-alpha.3
corepack enable
pnpm install --frozen-lockfile
pnpm test:tck
```

`pnpm test:tck` 是自包含检查，不需要消息代理、数据库、云账号或边缘设备。修改仓库，或需要验证 TypeScript、Rust、C 与 C++ 的全部绑定基础时，再运行 `pnpm check`。

## 采用精确发布版本

面向生产的使用方不应跟随浮动分支，也不应复制未经验证的制品子集。请提交封闭的 `aether-contracts.lock.json`，导入精确的必需制品全集，并运行该发布版本的复合验证操作。验证器会检查解析后的发布提交、清单摘要、制品哈希、采用集合以及可选的在线发布身份。

AetherEdge 与 AetherCloud 中提交的使用方副本展示了这种分发模型。当前可验证的锁定证据仍指向 `v0.1.0-alpha.3`，而 alpha.4 采用与端到端门槛仍未完成。任何候选证据都不能把认证、持久确认或旧传输切换提升为生产状态。

## 选择语言绑定

- [TypeScript](/aethercontracts/packages/typescript/)
- [Rust](/aethercontracts/packages/rust/)
- [C99](/aethercontracts/packages/c/)
- [C++17](/aethercontracts/packages/cpp/)

四种绑定都执行公开测试夹具清单。它们是刻意收窄的基础能力，不是完整的生产传输编解码器。Go、Java 与 Python 绑定仍在规划中。

如果现有使用方仍引用边缘仓库的旧名称，请阅读[产品命名迁移](/aethercontracts/migration/)。软件包和协议标识保持不变。
