---
title: "紧急撤销云端控制能力"
description: "在安全事件中移除云端命令入口，同时避免把它误称为物理紧急停止"
updated: 2026-07-18
status: planned
---

# 紧急撤销云端控制能力

> 权威来源：[AetherCloud](https://github.com/EvanL1/AetherCloud/blob/main/docs/recovery/emergency-revoke.md)。此页面镜像到统一的 AetherIoT 文档中。

需要立即隔离云端或智能体控制时，请查阅本页。撤销云端控制能力不等于证明物理效果已经停止。

## 当前已经实现

- 设备控制默认拒绝。
- 只有显式组合只读发现、控制开关、对应应用用例和可信治理后，实验性的固定 Home Assistant 电源动作才会出现。
- 该动作属于高风险操作，需要明确确认，并且仍受 AetherEdge 本地授权和安全策略约束。
- 云端取消只表达意图，绝不会伪造“已经停止”的结果。

## 尚未实现

AetherCloud 目前没有生产身份服务、权限撤销命令、网关紧急隔离命令、智能体总开关、凭据吊销流程、公开紧急接口或云端物理紧急停止。`integration.webhook.subscription.disable` 只会停用网络回调交付，绝不能把它描述为设备控制撤销。

## 安全处置

1. 使用适合现场和设备的独立人工应急通道。人员、设备或财产可能面临风险时，不得等待智能体处理。
2. 从生产组合中移除受影响的命令工具，或者在当前权威身份系统中撤销其身份和权限。
3. 如果已有权威外部控制，通过该控制隔离受影响的消息代理连接或网关凭据。
4. 除非现场人工流程明确要求本地隔离，否则应保留 AetherEdge 的本地采集、确定性规则、安全联锁和安全行为。
5. 在边缘端或物理证据消除不确定性之前，把未完成命令的结果标记为未知。不得声称撤销云端访问已经逆转物理效果。
6. 保存确认、命令、回执、会话和审计证据。

## 人工升级

紧急撤销始终需要一个不依赖智能体的人工入口，并指定事件负责人。重新启用受影响的命令能力之前必须由人员确认。凭据泄露、无法解释的物理效果或无法联系边缘端时，必须转交安全人员和现场安全负责人。

继续阅读[受治理的 Home Assistant 电源控制](/aethercloud/concepts/home-assistant-governed-control)、[边缘端、云端与提供商的权威边界](/aethercloud/concepts/edge-cloud-boundary)和[物联网云能力地图](/aethercloud/concepts/iot-cloud-capability-map)。
