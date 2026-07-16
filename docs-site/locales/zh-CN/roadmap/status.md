---
title: "平台现状和路线图"
description: "已实施、实验和计划功能的状态分别报告。产品名称不会提升技术准备程度。"
updated: 2026-07-16
---

# 平台状态和路线图

分别报告已实施、实验和计划功能的状态。产品名称不会升级技术准备情况。

## AetherEdge

**实现：** 六服务运行时、SHM 实时状态权限、嵌入式本地操作、受治理命令、`aether` CLI、`aether-edge-sdk`、Pack v1、MCP 和 OpenAPI 基础，并已签署`v0.5.0` 源/运行时/CLI 工件。

**实验：** CloudLink MQTT v1 边缘基础、应用程序 ACK 驱动的持久队列、AetherContracts alpha.3 消耗和真实 Broker 开发证据。

**计划或限制：** 生产 CloudLink密钥生命周期、签名 ACK、完整联合一致性、旧版切换和剩余应用程序边界迁移。

## AetherCloud

**实现的基础：** 模块化整体域/应用程序切片、功能驱动的提供程序、仅计划 OpenTofu、网关注册、部分 CloudLink/遥测持久性、工件/部署/作业基础、审计和集成切片、可观察性和传输中立的 MCP 接口。

**实验或部分：** MQTT 编解码器和入口、本地/AWS IoT 工具、PostgreSQL 接受的遥测 ACK 发件箱和有限审计接口。

**计划或门控：** 生产身份、完整的 CloudLink 持久性和映射、生产组合和工人、公共作业/部署交付、强化的出站集成以及可连接的 MCP 服务器。

## AetherContracts

**已实现、实验性：** alpha.3 规范、封闭的 Schema、测试夹具、TCK、摘要固定消费者验证和四个测试夹具绑定。

**计划或门控：**生产身份验证密钥生命周期、签名的持久 ACK、完整的生产编解码器和生产 CloudLink 切换版本。

## 平台文档

**在此迁移中实现：**共享产品概述、统一导航、部署拓扑、用户旅程、端到端 alpha 教程、兼容性矩阵、状态页面和 AetherIot AetherEdge 迁移指南。

**计划：** 自定义 `aetheriot.dev` 和 `docs.aetheriot.dev` 域、自动跨仓库版本聚合、发布通道状态源以及未来的 GitHub 组织（当合适的地址可用时）。
