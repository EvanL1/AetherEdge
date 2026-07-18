---
title: "集成控制协议 v1 alpha 1"
description: "默认关闭的受治理设备电源控制：固定意图、显式确认、边缘最终决定和分阶段回执。"
updated: 2026-07-17
id: integration-control-v1alpha1
status: experimental-default-off
version: 0.1.0-alpha.4
normative: true
---

# 集成控制协议 v1 alpha 1

> 本页帮助中文用户理解协议。带标签版本中的英文规范、JSON Schema、测试夹具和 TCK 才是规范性依据。

> 权威来源：[AetherContracts](https://github.com/EvanL1/AetherContracts/blob/main/spec/integration-control-v1alpha1.md)。

集成控制是独立且默认关闭的 CloudLink 扩展，名称为
`aether.cloudlink.integration-control.v1alpha1`。它不会改变只读拓扑和观测扩展，普通
CloudLink 1.0 会话也不会自动获得控制能力。

当前版本只允许一个语义动作：`device.power.set.v1`。协议没有“任意动作”入口，也不允许
调用方提交 Home Assistant 域、服务名、服务参数、实例地址、令牌、脚本、模板或任意
JSON。这样可以防止一个看似受限的电源动作被当作执行其他操作的容器。

## 启用条件

只有同时满足以下条件，云端才可以下发动作：

1. 边缘运行时清单明确声明控制扩展。
2. 边缘消费方、持久作业账本、本地设备接入和本地安全策略均已投运。
3. 云端已经观测到这份精确 Runtime Manifest，并显式启用控制消息生产方。
4. 动作绑定当前通过认证的 CloudLink 会话。

发布顺序必须先边缘、后云端。云端不能通过试发动作来探测边缘是否支持控制。不支持或未
启用该扩展的边缘不应收到任何动作。关闭扩展会阻止新执行，但不会删除既有作业的审计和
重放账本。

## 固定控制意图

控制意图使用 `aether.integration-control.action-intent.v1alpha1`。目标只能包含：

- `integration_id`：设备接入实例；
- `snapshot_generation`：已经接纳的准确拓扑代次；
- `entity_id`：稳定实体身份；
- `point_key`：固定为 `is_on`。

参数对象只能包含一个布尔值 `value`。边缘必须用完整拓扑解析目标：代次必须完全一致，
实体必须存在，点位必须是布尔状态点；Home Assistant 映射目前只允许 `fan`、`light`
和 `switch` 三类实体。

公共消息不包含当前 Home Assistant 实体地址。边缘在确认稳定身份和本地拓扑代次之后，
才解析当前地址。即使用户重命名了实体，过期的云端意图也无法借此指向另一个设备。

## 固定治理要求

治理字段不能由调用方降低：

| 要求         | 固定值                       |
| ------------ | ---------------------------- |
| 执行方式     | `governed-job`               |
| 默认授权     | `deny`                       |
| 权限         | `integration.device.control` |
| 风险         | `high`                       |
| 确认         | `required`                   |
| 幂等         | `required`                   |
| 到期时间     | `required`                   |
| 审计         | `required`                   |
| 边缘最终决定 | `true`                       |

意图还会携带有长度限制的授权和确认引用。它们只是云端治理证据，不能覆盖边缘本地策略。
边缘在调用 Home Assistant 之前，仍要验证云端身份、用户确认、设备接入投运状态、准确
目标和本地安全决定。

## 签名、到期与重放

动作提议把网关、当前会话、凭据代次、作业身份、签发时间、强制到期时间、完整意图及其
摘要绑定在一起。

`intent_digest` 是对完整意图进行 RFC 8785 规范化后计算的 SHA-256，表示为
`sha256:` 加 64 位小写十六进制文本。`cloud_authentication` 使用 Ed25519 对协议规定
的准确字段投影签名。字段形状正确不代表签名有效；边缘必须用已经配置的云端公钥验证。

生产环境的云端密钥签发、轮换、撤销和验证方归属尚未冻结，因此该能力仍保持实验性和默认
关闭状态。

作业重放身份是 `(gateway_id, job_id)`。第一次持久接纳会把它绑定到唯一意图摘要：

- 相同作业和相同摘要的重放返回已存回执，不得再次调用 Home Assistant；
- 相同作业使用不同摘要会以 `DIGEST_CONFLICT` 拒绝；
- 超时、崩溃或接入方返回含糊结果时，结果是 `unknown`，不能自动重试物理动作。

提议只在 `evaluation_time_ms < expires_at_ms` 时有效。到期时间早于签发时间会被拒绝；
到期时刻及之后同样会被拒绝。

## 边缘执行

布尔意图只在边缘内部映射：

- `true` 选择固定的开启操作；
- `false` 选择固定的关闭操作。

公共消息和调用方都不能选择 Home Assistant 的服务名、目标语法或服务参数。实例地址和
凭据继续留在边缘本地密钥存储中。

边缘必须先持久记录作业身份和意图摘要，再记录“准备尝试”的审计证据，之后才可调用接入方。
本地策略拒绝或调用前审计写入失败时，不得发出设备请求。

## 回执与证据

回执通过已认证的 CloudLink 上行消息返回，并继承精确重放、业务摘要和连续持久确认规则。
它会绑定作业、回执序号、能力、准确目标和意图摘要。

允许的证据阶段只有：

- `edge-accepted`：边缘接纳意图；
- `edge-rejected`：边缘拒绝意图；
- `provider-accepted`：Home Assistant 接纳请求；
- `provider-rejected`：Home Assistant 拒绝请求；
- `unknown`：结果无法确定。

所有回执的 `physical_outcome` 都固定为 `unknown`。协议没有“物理完成”或“执行成功”
阶段。`provider-accepted` 只表示提供方接受请求，不能证明物理设备已经完成动作，也不能
证明无线报文到达设备、执行器已经动作或目标状态已经出现。后续状态观测是另一条提供方
证据，也不是独立的物理确认。

云端必须分别保存提供方接受、观测状态、物理确认和最终作业结论，不能把
`provider-accepted` 直接显示为物理执行成功。

## 当前状态

规范结构位于 `schemas/integration-control/v1alpha1/`，一致性测试固定了唯一动作、封闭
参数、密钥边界、默认关闭、准确拓扑绑定、到期、重放冲突和回执阶段。

`0.1.0-alpha.4` 是尚未发布的开发目标，仍处于实验阶段，并且默认关闭；最新发布版本仍是
`v0.1.0-alpha.3`，且不包含该控制扩展。alpha.4 源码不声明生产云端密钥生命周期、产品
作业账本、Home Assistant 命令适配器、端到端消息代理证据或物理确认已经交付。
