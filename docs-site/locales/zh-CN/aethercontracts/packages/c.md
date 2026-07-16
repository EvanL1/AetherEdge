---
title: "AetherContracts C 基础库"
description: "C99 基础是免分配的，并使用调用者拥有的字符串视图。它目前提供规范的 `uint64` 解析以及静态 Thing Model..."
updated: 2026-07-15
---

# AetherContracts C 基础库

> 权威来源：[AetherContracts](https://github.com/EvanL1/AetherContracts/blob/main/packages/c/README.md)。此页面镜像到统一的 AetherIoT 文档中。

C99 基础是免分配的，并使用调用者拥有的字符串视图。它目前提供规范 `uint64` 解析以及静态 Thing Model 属性/点/功能查找。能力元数据默认被拒绝并且没有调用API。一个有界的、调用者拥有的实验验证器执行公共 CloudLink 固定配置文件。它不是通用 JSON 解析器或完整的生产 CloudLink 传输/身份验证编解码器。

安装或使用根 CMake 项目和链接 `AetherContracts::c`。
