---
title: "集成故障时的安全降级"
description: "把异常设备集成降级为有界只读或不可用状态，并保留数据过期标记"
updated: 2026-07-18
status: partial
---

# 集成故障时的安全降级

> 权威来源：[AetherCloud](https://github.com/EvanL1/AetherCloud/blob/main/docs/recovery/integration-safe-degradation.md)。此页面镜像到统一的 AetherIoT 文档中。

边缘端集成报告无效数据、拓扑缺口、过期观测、回执不确定或反复交付失败时，请查阅本页。

## 当前已经实现

- 实验性 Home Assistant 拓扑和观测链路把发现与采集保留在 AetherEdge。
- 云端投影查询有界、受租户范围约束，并明确标记为边缘端上报副本，而不是实时状态权威。
- 拓扑代次和重放规则会拒绝冲突观测或不属于当前代次的观测，并支持完整重新同步。
- 集成控制默认关闭、作用范围固定、需要明确确认，而且不会把交付当作物理完成。
- 网络回调交付使用有界重试和死信证据，不会回滚已经提交的业务结果。

## 尚未实现

已经发布的 alpha.4 契约消费、生产 CloudLink 与凭据组合、受支持消息代理证明、生产控制组合、签名密钥生命周期、公开智能体服务和受治理的控制重新启用流程仍处于门禁或规划阶段。

## 安全处置

1. 保持 AetherEdge 本地采集、确定性自动化和安全行为不依赖云端可用性。
2. 把云端投影标记为过期或不可用，保留其观测时间，不得猜测缺失的设备状态。
3. 移除受影响集成的命令入口。只有身份和租户范围仍可信时，才保留有界只读发现。
4. 在现有会话和运行时清单边界内，请求边缘端提供完整拓扑和观测重新同步。
5. 只有代次、摘要、顺序和新鲜度全部收敛后才能恢复读取。恢复控制必须经过运维人员显式启用和策略复核。
6. Home Assistant 地址、令牌、凭据和提供商原始载荷必须继续留在边缘端。

## 人工升级

反复拓扑冲突、无法收敛的序列缺口、凭据泄露、控制结果未知或任何物理安全风险都必须转交人工处理。死信重投和控制重新启用需要明确的人员复核，智能体不得为了消除告警而自行执行。

继续阅读[Home Assistant 集成](/aethercloud/concepts/home-assistant-integration)、[受治理的 Home Assistant 电源控制](/aethercloud/concepts/home-assistant-governed-control)和[审计、订阅、网络回调与数据导出](/aethercloud/concepts/audit-and-integrations)。
