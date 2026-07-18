---
title: "接入委托设备提供方"
description: "使用与提供方无关的拓扑和观测契约接入本地设备控制器，并保持凭据、控制权和物理执行边界。"
updated: 2026-07-18
status: experimental
version: 0.1.0-alpha.4
---

# 接入委托设备提供方

> 本页是英文原文的简体中文说明。带标签版本中的英文规范、JSON Schema、测试夹具和 TCK 共同构成规范性依据；中文页面只用于任务路由和人工审查。

最新发布版本是 `v0.1.0-alpha.3`。`0.1.0-alpha.4` 只是尚未发布的开发目标，不能当作已发布版本下载、固定或声明一致性。

当 AetherEdge 连接 Home Assistant 等现有本地设备控制器时，使用
`aether.integration`。控制器提供注册信息和状态证据，但不会因此成为云端基础设施，也不能绕过边缘侧策略或物理控制边界。

接入过程从一份完整的拓扑快照开始：

```text
接入实例
├── 区域
├── 设备
└── 实体
    ├── 数据点：current_temperature（float64）
    ├── 数据点：target_temperature（decimal）
    └── 数据点：hvac_mode（string）
```

一个实体不必被压缩成单个标量。每个标准化数据点都声明自己的类型、种类和可选单位。观测批次必须绑定到确切的快照代次，因此重命名、删除或新发现的实体不能按照无关的目录解释。

对于 Home Assistant，保留注册项编号作为稳定的 `entity_id`，并把当前
`domain.object_id` 写入 `source_address`。用户重命名只改变地址，不改变稳定身份。只能映射已经具有明确数据点描述、并且满足边界限制的属性；不得复制任意属性对象。

严格按照以下质量规则处理观测：

- `good` 和 `uncertain` 必须包含类型化值；
- `bad` 和 `unavailable` 不得包含值；
- 诊断信息可以解释证据为何降级，但不得包含凭据或无边界的提供方载荷。

Home Assistant 地址和凭据始终保存在 AetherEdge 的机密存储中，不得进入这些文档、云端投影、审计载荷或智能体提示词。

若要把证据投影到 AetherCloud，必须显式启用
`aether.cloudlink.integration.v1alpha1` 扩展。该扩展原样封装完整的公开拓扑对象或观测对象。已经通过认证的外层会话提供网关身份，载荷中不得出现 Home Assistant 地址、令牌、会话信息或凭据引用。

先升级云端使用方，确认当前 `Runtime Manifest` 已声明该扩展，再启用边缘侧发布。仅完成 CloudLink 1.0 基础协商并不代表支持这个扩展。拓扑是一份不可分割的原子替换，必须完整装入 256 KiB 的消息上限，不能拆分。观测数组可以在封装前划分成多个相互独立的批次。

使用以下命令验证源码检出：

```bash
pnpm test:tck
```

公开的接入测试夹具位于 `fixtures/integration/v1alpha1/`，对应的 CloudLink
封装和持久确认示例位于 `fixtures/cloudlink-integration/v1alpha1/`。

受治理控制属于另一份独立且默认关闭的契约。第一个能力范围只允许
`device.power.set.v1`，目标必须绑定到已经接受的确切拓扑代次、稳定实体和布尔型
`is_on` 数据点。它要求显式确认、高风险权限、到期时间、幂等、审计和边缘侧最终决定。公开消息不能选择 Home Assistant 领域或操作，也不能携带提供方参数、地址、令牌或任意对象。

具体约束见[受治理控制规范](/aethercontracts/spec/integration-control-v1alpha1/)。

Home Assistant 返回成功只能记录为提供方已接受请求，不能证明物理设备已经完成预期变化。
