---
title: "符合性与使用方验证"
description: "了解 AetherContracts TCK 证明了什么、绑定如何报告证据以及消费者如何验证确切的发布关闭"
updated: 2026-07-16
status: implemented
---

# 符合性与使用方验证

> 权威来源：[AetherContracts](https://github.com/EvanL1/AetherContracts/blob/main/docs/conformance.md)。此页面镜像到统一的AetherIoT文档中。

AetherContracts将四种证据分开，以便结构接受、上下文行为、语言API和产品集成不会混淆。

## 证据层

1. **规范：**规范的英语语义和生命周期规则。
2. **Schema：**已结束 JSON Schema 2020-12 草案结构验收。
3. **夹具和 TCK：**有效、无效、迁移和具有稳定公共故障类的上下文结果。
4. **消费者证据：**产品特定的传输、持久性、重启、身份验证和故障行为。

前三层属于 AetherContracts。第四个属于每个消费者。产品无法修改本地 Wire 文件并声称它更改了公共契约。

## 运行语言中立的 TCK
```bash
pnpm test:tck
```

黑盒运行程序验证有界解析、规范整数行为、Thing Model 迁移、CloudLink 固定结果、上下文重播和游标规则以及清单一致性。默认情况下处于离线状态。

阅读 [TCK v1 alpha](/aethercontracts/spec/tck-v1alpha1) 了解运行程序合约，并阅读 [Foundation](/aethercontracts/spec/foundation) 了解常见故障语义。

## 绑定证据

每个绑定必须执行相同的公共测试夹具清单并报告相同的合约字符串故障类：
```bash
pnpm test:typescript
pnpm check:rust
pnpm check:c
```

完整的仓库检查还验证打包、CMake 安装、清理程序行为、生成的工件和发布哈希：
```bash
pnpm check
```

通过这些检查意味着发布的 alpha 曲面表现一致。它并不声称每个绑定都是完整的生产编解码器。

## 验证消费者关闭

消费者锁标识确切的发布标签、剥离的提交、清单摘要、导入的工件集和待处理集。完整的消费者必须导入整个所需的闭包，并且没有待处理的工件。

发布复合操作和离线验证器拒绝：

- 与锁不匹配的标签或操作提交；
- 具有不安全或意外布局的存档；
- 具有错误摘要的清单、工件或导入的字节；
- 不完整或额外的采用闭包；
- a尝试覆盖公开发布的本地权限文件。

在线验证在相同的本地字节检查之前验证 GitHub 发布身份。离线验证是已导入消费者树的默认设置，不会联系注册表、代理或云帐户。

## 产品证据保持独立

AetherEdge 和 AetherCloud 在各自的仓库中添加 Real-Broker、重新启动、PostgreSQL 和故障证据。这些结果可能满足发布门槛，但它们不会改变 AetherContracts 标记。同样，传递公共TCK并不能证明产品的密钥生命周期、持久发件箱事务、操作部署或回滚路径。

在调用实现一致性或更改旧传输默认值之前检查[兼容性和发布门](/aethercontracts/compatibility)。
