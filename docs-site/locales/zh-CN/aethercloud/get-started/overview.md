---
title: "AetherCloud概述"
description: "了解多云产品、其权限边界和第一个垂直切片"
updated: 2026-07-16
status: mixed
---

# AetherCloud概述

> 权威来源：[AetherCloud](https://github.com/EvanL1/AetherCloud/blob/main/docs/get-started/overview.md)。此页面镜像到统一的 AetherIoT 文档中。

AetherCloud 是 AetherEdge 的可选 AI 原生多云融合和控制平面。它为人员、应用程序和编码代理提供了一个地方来了解队列和云资源、选择位置、检查遥测、发布版本化工件以及跨边缘站点和基础设施提供商协调审核工作。

它不是获取设备数据或关闭控制循环的运行时。当AetherCloud、广域网或 AI 提供商不可用时，AetherEdge 主机继续运行。

## AetherCloud中的内容

- 租户身份、用户、权限、项目和队列清单
- 云连接、提供商能力发现和规范化清单
- 放置决策和提供商范围内的部署堆栈
- 审核的基础设施规划和应用作业
- 网关注册和连接观察
- 版本化包、配置、模型和应用程序工件
- 单独的期望、报告和应用的修订事实
- 从边缘接收的持久遥测和警报投影
- 过期、审核的功能作业及其人工编写和代理编写的应用程序的收据
- API和文档

## 边缘保留的内容

- 设备协议会话和获取
- 权威的实时点值
- 委托通道配置
- 确定性规则、警报、联锁和回退行为
- 验证和最终接受可能影响物理系统的工作
- 断开连接期间的有限存储和转发行为

## 在提供程序中保持权威的内容

- 提供程序资源的实际存在和生命周期状态
- 提供程序本机身份、配额、区域、故障域和功能
- 一个部署的远程锁定基础架构状态stack

AetherCloud 拥有所需的放置和编排状态，但不会假装标准化投影是提供程序的实际状态。在添加提供商或基础设施工作流程之前，请阅读[多云融合模型](/aethercloud/concepts/multi-cloud-fusion)。

在添加跨网络的功能之前，请阅读[权限边界](/aethercloud/concepts/edge-cloud-boundary)。

## 交付顺序

仓库以 TypeScript 模块化整体和代理可读合约开始。提供商发现、受控基础设施计划和网关注册/声明基础已经实现了域/应用程序切片。内存适配器使它们保持独立；他们的生产适配器和公共租户路线仍在计划中。基础设施引擎特意仅用于计划，没有应用操作。

IoT 产品序列包括网关身份、CloudLink 会话、运行时清单、遥测/警报摄取、工件注册表、所需/报告/应用的部署、受控作业、操作集成和 MCP。每个切片必须是有用的，而不需要后面的切片。设备控制不是仓库基础里程碑的一部分。

阅读[功能图](/aethercloud/concepts/iot-cloud-capability-map)以了解完整的有界上下文，并阅读[垂直切片路线图](/aethercloud/guides/iot-cloud-roadmap)以了解阶段门和排除。

## 开始开发

阅读[仓库布局](/aethercloud/reference/repository-layout)，然后按照[代理开发指南](/aethercloud/guides/build-with-an-agent)。当前的 API 表面记录在 [HTTP 参考](/aethercloud/reference/http-api) 中。
