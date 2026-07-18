---
title: "产品命名迁移"
description: "说明 AetherIoT 母项目、AetherEdge 仓库改名，以及不可变发布制品中的历史名称。"
updated: 2026-07-17
status: experimental
version: 0.1.0-alpha.4
---

# 产品命名迁移

> 本页是面向中文用户的说明。带标签版本中的英文规范与已发布字节才是规范性依据。

AetherIoT 是 AetherEdge、AetherCloud 和 AetherContracts 的母项目。原名为 AetherIot 的边缘产品与仓库已迁移为 `EvanL1/AetherEdge`。

AetherContracts `v0.1.0-alpha.3` 已经不可变，并有意在经过签名、摘要锁定的发布制品中保留历史名称 AetherIot。使用方不得重写这些字节。尚未发布的 `0.1.0-alpha.4` 开发源码使用 AetherEdge 展示名称，同时保留所有协议、结构定义、TCK、软件包和失败代码标识；除非另有单独的版本化契约决策，否则这些标识不能改动。

仓库改名不属于协议演进，也不会改变一致性状态。alpha.4 仍是未发布的实验性开发目标，不是生产 CloudLink 切换版本。
