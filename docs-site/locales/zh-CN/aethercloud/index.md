---
title: "AetherCloud 产品概览"
description: "AetherCloud 是逐步演进的智能体、多云融合与受治理控制平面，不是边缘运行时的托管副本。"
updated: 2026-07-16
---

# AetherCloud 产品概览

AetherCloud 是逐步演进的智能体、多云融合与受治理控制平面。它面向 AetherEdge 节点和云端工作负载，未来负责长期上下文、期望状态、受治理作业、集成和多云协调，但不直接拥有实时物理状态。

AetherEdge 继续负责数据采集、确定性规则、安全联锁和最终物理执行。云端故障不能停止已经投运的边缘行为。

## 已经实现的基础

- 能力驱动的提供方发现与受治理基础设施规划。
- 带锁定和进程安全证据的仅规划 OpenTofu 基础设施引擎。
- 网关身份与注册、CloudLink 会话和运行时清单应用基础。
- 网关和已接受遥测事实的部分 PostgreSQL 持久化，包括持久确认发件箱证据。
- 部分制品、部署、受治理作业、审计、集成、可观测性和传输中立 MCP 应用切片。

## 仍在规划或受门槛限制

面向最终用户的对话智能体、站点语义上下文、意图编译与持续调整闭环尚未完成。生产身份与凭证生命周期、公开 CloudLink 组合、完整崩溃持久性、生产数据库组合、工作进程、强化的外发交付和可连接的 MCP 服务器也仍在规划或受发行门槛限制。

AetherCloud 管理期望状态，基础设施提供方管理其实际资源，AetherEdge 管理实时点状态与最终物理执行。

继续阅读 [AI 原生平台](/overview/ai-native-platform/)、[AetherCloud 架构](/aethercloud/concepts/architecture/)和[平台状态](/roadmap/status/)。
