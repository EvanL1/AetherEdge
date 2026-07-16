---
title: "期望的、报告的和应用的部署"
description: "推出不可变的工件意图，而不将调度、连接或超时视为边缘应用程序证据"
updated: 2026-07-16
status: mixed
---

# 期望、报告和应用的部署

> 权威来源：[AetherCloud](https://github.com/EvanL1/AetherCloud/blob/main/docs/concepts/desired-reported-applied-deployment.md)。此页面已镜像到统一的AetherIoT文档中。

云端域/应用程序基础和原子内存适配器是针对一个网关目标实现的。它们提供已发布的工件查找、无损的期望生成、不同的报告和应用事实、暂停/恢复/取消请求/回滚意图、经过验证的边缘观察、未知结果、重播/冲突处理、租户范围的查询、审计证据和发件箱证据。

这不是端到端的部署。 PostgreSQL、生产审核/发件箱、CloudLink 外向交付和观察信封、目标快照、金丝雀/批次调度、公共 HTTP 和 AetherEdge 对应项仍在计划中。

## 权威和事实

- AetherCloud 拥有所需的修订和推出意图。
- AetherEdge 拥有其报告的观察结果和最终兼容性/策略决策。
- 仅当边缘证据表明已应用或失败时才存在已应用。调度、下载、验证、连接会话或 HTTP 成功都无法创建它。
- 网络超时会创建单独的 `unknown` 协调状态。它不捏造应用事实。稍后经过验证的边缘观察可能会解决投影问题。
- 工件发布使修订符合所需要求；它不会部署或应用它。

实现的视图返回 `reported: null` 和 `applied: null` 直到这些事实存在，而不是重载单个状态字段。

## 状态和排序

单目标推出状态基础是：
```text
running <-> paused -> cancel-requested
   |                     |
   +-- edge applied ----> completed
   +-- reject/fail -----> completed-with-failures
```

回滚创建一个更新的 Desired 生成，指向另一个不可变的、已发布的 Artifact Revision。它附加所需的历史记录；它不会编辑前一代或暗示边缘恢复任何内容。

每个边缘观察都有一个稳定的身份和无损的期望生成。精确重播是安全的。重复使用具有不同内容冲突的观察身份。对于老一代人来说，一个事实被保留在观察历史中，而不会向后滚动当前的预测。比云已知的所需生成更新的事实失败关闭。

取消是停止尚未跨越相关边缘边界的工作的请求。它无法擦除应用的证据或逆转物理效应。

## 已实现的应用程序界面

- `deployment.rollout.start`：高风险、显式确认、已发布的 Artifact 前提条件、过期、幂等性、许可和所需审核。
- `deployment.rollout.pause`、`deployment.rollout.resume` 和 `deployment.rollout.cancel-request`：受控的推出意图
- `deployment.rollout.rollback`：具有已发布工件前提条件的高风险新一代所需生成。
- `deployment.rollout.mark-unknown`：记录不确定的交付/执行结果，而不推断失败或成功。
- `deployment.observation.report`：活动网关凭证范围和目标绑定、精确解码、幂等性和边缘证据投影。
- `deployment.rollout.get`：租户/项目范围的查询。

内存仓库以原子方式存储聚合、幂等性、审计和发件箱证据，并证明乐观版本冲突。它是一个一致性适配器，而不是生产持久性。

## 计划扩展

目标快照、队列、金丝雀/批次限制、暂停门、每个目标部分成功、运行状况策略、推出计划和大型证据存储都建立在这个单目标聚合的基础上。生产CloudLink仅通过应用程序拥有的发件箱记录发送版本化的所需报价或不可变参考；它不能写入边缘缓存、SHM、点或设备寄存器。

读取 [ADR-0010](https://github.com/EvanL1/AetherCloud/blob/main/docs/adr/0010-desired-reported-applied-deployment.md) 以了解决定，并读取 [Artifact 注册表](/aethercloud/concepts/artifact-registry) 以了解发布机构。
