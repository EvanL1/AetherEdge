---
title: "命令结果不确定时的恢复"
description: "依据权威证据处理超时或结果不明的命令，避免盲目重复物理操作"
updated: 2026-07-18
status: partial
---

# 命令结果不确定时的恢复

> 权威来源：[AetherCloud](https://github.com/EvanL1/AetherCloud/blob/main/docs/recovery/unknown-command-outcome.md)。此页面镜像到统一的 AetherIoT 文档中。

当受治理作业、部署命令或集成控制请求已经接受或发送，但在期限内没有收到权威结果时，请查阅本页。

## 当前已经实现

- 受治理作业和部署状态机会把 `unknown` 保留为独立结果，而不是把超时改写为成功或失败。
- 迟到的已认证回执或边缘端观测仍然是事实，并且可以消除不确定性。
- 取消只记录意图，不会抹除迟到的物理成功结果。
- 内存基础会保留命令身份、幂等性、有序回执、冲突检测、审计和发件箱语义。

## 尚未实现

生产 PostgreSQL 作业与部署账本、CloudLink 交付、调度器、到期工作进程、公开命令接口、完整 MCP 传输以及对应的 AetherEdge 交付路径仍在规划中。静态文档无法检查物理环境，也无法自行消除未知结果。

## 安全处置

1. 保存原命令身份、幂等键、意图摘要、能力声明版本、签发时间和到期时间。
2. 查询现有作业或部署投影，只接受属于同一身份的已认证边缘端证据。
3. 缺少权威证据时继续保持 `unknown`。
4. 对不可安全重放或具有物理效果的操作，绝不能通过创建新命令身份来自动重试。
5. 取消请求可能阻止尚未被边缘端接受的工作，但不能证明已经接受的工作停止了。
6. 迟到回执到达后，应保留它，同时保留此前的超时和取消证据。

## 人工升级

任何高风险或不可安全重放的物理操作进入 `unknown` 后，都必须由人员检查边缘端证据；必要时还要检查物理环境。人员决定继续保留未知状态、请求取消，还是创建一个经过独立确认的新意图。智能体不得根据交付、超时或连接消失推断操作已经完成。

继续阅读[受治理能力作业](/aethercloud/concepts/governed-capability-jobs)、[期望、报告与应用部署](/aethercloud/concepts/desired-reported-applied-deployment)和[受治理的 Home Assistant 电源控制](/aethercloud/concepts/home-assistant-governed-control)。
