---
title: "受治理的能力作业"
description: "通过版本化作业请求边缘能力，避免任意 RPC、不安全重试和云端对物理结果的越权声明"
updated: 2026-07-16
status: mixed
---

# 受管能力作业

> 权威来源：[AetherCloud](https://github.com/EvanL1/AetherCloud/blob/main/docs/concepts/governed-capability-jobs.md)。此页面已镜像到统一的 AetherIoT 文档中。

实现了传输中立域/应用程序基础和原子内存适配器。授权的租户参与者只能为声明的网关功能创建工作、检查其治理元数据、在需要时确认它、排队并提供它、请求取消、将不明确的结果标记为未知、提取经过身份验证的边缘收据以及查询生成的作业。

这不是端到端远程执行产品。 PostgreSQL、生产审核/发件箱、运行时清单目录接线、CloudLink 信封和邮箱、大型证据存储、公共 HTTP、调度工作进程和 AetherEdge 对应项仍在计划中。

## 权限和准入

- AetherCloud 拥有工作意图、租户授权、确认证据、到期和交付政策。
- AetherEdge 拥有能力接受、本地前提条件和安全检查、执行和结果事实。
- 声明的能力是可用性的证据，而不是授权的证据。创建作业需要 `edge.job.create` 和声明的许可。
- 能力声明（而不是调用者输入）提供风险、确认、重放安全、许可和物理效果元数据。
- 未知或格式错误的能力无法关闭。没有通用方法名称或任意 RPC 回退。
- 默认情况下物理效应功能仍被拒绝，边缘始终保留其最终决定。

当前内存目录是一致性测试夹具。生产资格必须使用带有新鲜度标签的运行时清单投影，并保留用于准入的确切声明版本。

## 生命周期和不确定性
```text
awaiting-confirmation -> authorized -> queued -> offered
                                      edge: accepted -> running -> terminal
                                                   \-> unknown
offered/running/unknown -> cancel-requested -> edge terminal Receipt
```

`succeeded`、`failed`、`partial`、`rejected`、`expired` 和 `canceled` 是终端边缘收据结果。边缘接受之前云端到期不会伪造收据。取消记录意图；如果效果已经发生，则稍后的`succeeded`收据仍然具有权威性。

网络超时后，作业将变为`unknown`。特别是，不安全或有物理影响的作业不会自动以新身份重试。较晚经过身份验证的收据可以解决不确定性，而不会擦除较早的超时或取消证据。

## 收据排序和幂等性

收据身份和序列是独立的重放防护。序列是无损规范`uint64`十进制字符串。精确重播返回现有事实。重用具有不同内容的身份或序列将导致关闭失败。

无序收据将保留为 `pending-predecessor`；它不会移动投影。填补空白将按顺序应用所有新的连续收据。执行终端收据需要证据摘要。大的证据字节将存在于对象存储中；作业分类帐仅存储受管理的引用和摘要。

## 实现的应用程序界面

- `edge.job.create`：验证准确的外部输入、命令时间窗口、租户范围、平台权限和功能权限。
- `edge.job.confirm`：高风险并明确确认。
- `edge.job.queue`和`edge.job.offer`：调度使用独立命令元数据进行控制。
- `edge.job.mark-unknown` 和 `edge.job.cancel-request`：记录云知识和意图，而不声明边缘结果。
- `edge.job.receipt.ingest`：从活动凭据派生租户、项目和网关并强制目标绑定。
- `edge.job.get`：租户/项目范围的查询在之前公开治理状态。

内存适配器自动存储作业状态、幂等性、所需的审计证据和发件箱记录。它无需外部服务即可证明行为；它不满足生产耐久性。

## 计划生产完成

生产工作添加了 PostgreSQL 分类账和收据收件箱、事务耦合的发件箱/审计链、运行时清单声明来源、应用程序拥有的 CloudLink 交付、证据对象、调度和到期工作进程、公共接口以及经过审核的 AetherEdge 契约。仅在持久接收接受后才会向边缘进行确认。

阅读 [ADR-0011](https://github.com/EvanL1/AetherCloud/blob/main/docs/adr/0011-governed-capability-jobs.md) 了解决策，阅读 [CloudLink 和核心状态机](/aethercloud/concepts/cloudlink-and-core-state-machines) 了解与传输无关的故障语义。
