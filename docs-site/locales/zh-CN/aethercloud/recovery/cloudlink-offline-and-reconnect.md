---
title: "CloudLink 离线与重连恢复"
description: "CloudLink 中断后保持边缘端权威，并核对会话、游标和确认信息"
updated: 2026-07-18
status: partial
---

# CloudLink 离线与重连恢复

> 权威来源：[AetherCloud](https://github.com/EvanL1/AetherCloud/blob/main/docs/recovery/cloudlink-offline-and-reconnect.md)。此页面镜像到统一的 AetherIoT 文档中。

消息代理、网络、云端进程或网关连接中断后，请查阅本页。断开连接只会使云端观测变旧，不能证明网关或物理设备已经停止。

## 当前已经实现

- 已有与传输无关的会话建立、心跳、会话代次隔离、恢复游标和当前会话查询语义。
- 实验性 MQTT 传输和消息代理故障测试已经覆盖部分重连行为。
- PostgreSQL 遥测切片只在持久事务完成后确认已接受的遥测，并能重新生成相同的确认。
- 新的已认证会话代次会隔离旧代次。

## 尚未实现

生产凭据生命周期、完整的逐条上行认证、可持久化的多实例会话所有权、生产 CloudLink 组合、流量控制、持久命令交付和完整联合符合性仍在规划中。现有基础不是可以直接部署的自动重连服务。

## 安全处置

1. 让已经投入使用的 AetherEdge 继续在本地执行采集、确定性规则、安全联锁和物理控制。
2. 使用最后观测时间把云端投影标记为过期，不得把它们展示为实时状态。
3. 只有在凭据有效、协议版本受支持、质询有效且会话代次更新时才能重连。
4. 从云端最后一个持久确认游标恢复。边缘端可以重放尚未确认的幂等事实，云端必须去重。
5. 不得因为连接关闭就重新执行物理命令。
6. 拒绝摘要冲突、无法收敛的序列缺口以及来自已隔离会话代次的消息。

## 人工升级

凭据拒绝、签名或身份不匹配、持续游标缺口、数据丢失证据、冲突重放或旧代次反复连接时，必须转交人工处理。普通断线和有界重连不要求人工确认，但身份或持久顺序不确定时，智能体必须停止自动恢复。

继续阅读[CloudLink 可靠传输与生命周期](/aethercloud/concepts/cloudlink-and-core-state-machines)、[CloudLink 协议参考](/aethercloud/reference/cloudlink-mqtt-v1)和[边缘端、云端与提供商的权威边界](/aethercloud/concepts/edge-cloud-boundary)。
