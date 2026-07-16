---
title: "AetherEdge 命名迁移说明"
description: "AetherIoT 是 AetherEdge、AetherCloud 和 AetherContracts 的总括项目。以前名为 AetherIot 的边缘产品和仓库移至……"
updated: 2026-07-16
---

# AetherEdge 命名迁移说明

> 权威来源：[AetherContracts](https://github.com/EvanL1/AetherContracts/blob/main/MIGRATION.md)。此页面镜像到统一的 AetherIoT 文档中。

AetherIoT 是 AetherEdge、AetherCloud 和 AetherContracts 的总括项目。以前名为 AetherIot 的边缘产品和仓库已移至 `EvanL1/AetherEdge`。

AetherContracts `v0.1.0-alpha.3` 是不可变的，并有意在其签名、摘要固定的发布工件中保留历史 AetherIot 名称。消费者不得重写这些字节。未来的合约版本可能会使用 AetherEdge 显示名称，同时保留每个协议、Schema、TCK、程序包和故障代码标识符，除非单独的版本化合约决策另有说明。

仓库重命名不是协议演变，也不会更改一致性状态。 alpha.3 版本仍处于试验阶段，不是生产 CloudLink 切换版本。
