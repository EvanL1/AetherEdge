---
title: "Thing Model v1 alpha 1 说明"
description: "Thing Model 是一个不可变的、行业中立的定义。它描述了配置属性、边缘观察点和受控功能。它…"
updated: 2026-07-15
id: thing-model-v1alpha1
status: experimental
version: 0.1.0-alpha.3
normative: true
---

# Thing Model v1 alpha 1 说明

> 本中文页面用于帮助理解。规范性定义、Schema、测试夹具和 TCK 以 AetherContracts 对应版本的英文发布内容为唯一权威。

> 权威来源：[AetherContracts](https://github.com/EvanL1/AetherContracts/blob/main/spec/thing-model-v1alpha1.md)。此页面镜像到统一的 AetherIoT 文档中。

Thing Model 是一个不可变的、行业中立的定义。它描述了配置属性、边缘观察点和受控功能。它不包含租户实例、实时值、凭据、寄存器绑定或客户拓扑。

每个已发布的修订版都有一个 `model_id`、规范的正整数 `revision` 和规范工件摘要。该摘要属于外部发布记录，因为自散列 Thing Model 不能包含自己的摘要；因此 `thing-model.schema.json` 中有意不包含该字段。禁止为不同的规范字节重复使用同一个修订号。键在属性、点和能力三个命名空间之间必须唯一；这是 JSON Schema 结构校验之外的语义规则。每项能力内部的参数键也必须唯一，避免请求负载产生歧义。

发布摘要是 `sha256:` 加上 RFC 8785 JCS 的小写 SHA-256，覆盖 `thing-model.schema.json` 接受的完整 Thing Model 对象。没有发布包装器或租户元数据进入该投影。机器可读规则为 `profiles/thing-model/v1alpha1/publication.json`。

## 权限

- 属性由不可变工件修订版拥有。更改是通过工件部署或显式边缘本地路径来应用的，而不是通过直接字段写入来应用。
- 点是边缘权威的只读观察结果。云历史和预测并不是实时状态的权威。
- 能力声明边缘可以理解的内容。每项能力均默认拒绝，并要求权限、风险等级、确认策略、幂等性、过期时间和审计策略；能力只能作为受管作业执行。边缘负责做出最终的接受、拒绝、过期或应用决定。

期望、报告和应用是不同的事实。期望的是云意图；报告的是支撑的边缘观察；应用的是边缘报告的部署证据。仅因为消息已发送或传输确认已到达，就不能从另一个推断出任何结果。

`applied` 观测值携带非空模型引用和 `applied_at_ms`。 `not-applied` 带有空模型并且没有应用时间戳； `applying` 和 `failed` 识别尝试的模型，但不伪造应用时间。根`observed_at_ms`为每个观测状态添加时间戳。

## Voltage 迁移概况

结构迁移为：

- `P`变为`properties`。
- `M`变为`points`。
- `A`变为`capabilities`。
- `pName` 成为核心模型外部的分类或组合元数据。
- 旧数字 ID 保留为命名空间别名，而不是全局 ID。

单位拼写被标准化，而遗留拼写被保留为出处。不明确的源类型、表示为标量的数组、重复的含义和不确定的单位需要显式的迁移诊断；导入者不得默默猜测。

最小夹具的结构来源于 `EvanL1/voltage-product-lib` 的提交 `7c4eec680f8b5e9a76a57c08078a41b9d5b4550c`。源仓库只有 README 中的 MIT 声明，导入时没有独立的 LICENSE，因此未复制整个目录。该夹具不是规范性的能源模型包。

## WoT 关系

该词汇与 W3C Web of Things 的属性、事件、操作和可复用 Thing Model 保持一致，以支持未来的导入与导出。JSON-LD 处理不属于 v1 alpha 核心要求。WoT 操作只会映射到 Aether 能力声明，绝不会授予直接物理执行权限。
