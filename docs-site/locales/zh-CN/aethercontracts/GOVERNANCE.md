---
title: "契约治理"
description: "规定互操作变更、智能体文档状态、破坏性变化、一致性证据和不可变分发的治理方式。"
updated: 2026-07-18
status: implemented
---

# 契约治理

> 本页是英文原文的简体中文说明。带标签版本中的英文规范、JSON Schema、测试夹具和 TCK 共同构成规范性依据；中文页面只用于任务路由和人工审查。

最新发布版本是 `v0.1.0-alpha.3`。`0.1.0-alpha.4` 只是尚未发布的开发目标，当前源码和候选证据不能替代不可变发布标签。

AetherContracts 变更必须按照互操作变更审查，不能当作某个软件开发包的局部便利功能。规范性行为必须具有英文规范、适用时的结构化 JSON Schema、有效和无效测试夹具、TCK 证据，以及明确的兼容性分类。

协议门槛和发布制品保留各自领域的状态词汇。智能体文档目录不能把多个事实压缩到一个复合状态中，而是使用以下相互独立的封闭字段：

- `implementation_status`：`implemented`、`partial`、`planned` 或
  `deprecated`；
- `production_readiness`：`production-ready`、`experimental`、
  `not-production-ready` 或 `not-applicable`；
- `context_sensitivity`：`public`、`internal`、`redacted-only` 或
  `sensitive-never-load`；
- `priority`：`core` 或 `optional`；
- `document_role`：`agent-task`、`operations`、`safety`、`recovery`、
  `reference`、`decision` 或 `status`。

开放或受阻的协议门槛必须继续作为明确的兼容性证据，不能隐藏在描述该门槛的文档状态中。

语言绑定只有通过完整且适用的 TCK 后，才能声明一致。破坏性变化必须使用新的契约版本。alpha 制品可以演进，但每次变化仍必须同时更新测试夹具和哈希，避免使用方把两组不同字节误认为同一个发布身份。

CloudLink 和 Thing Model 可以沿相互独立的版本线演进。发布包记录二者的兼容版本，不要求它们同步升级。

Git 仓库、带标签的发布包、契约清单和公开的 SHA-256 校验值共同构成权威分发来源。软件包登记服务和内容分发网络只是镜像，必须保留发布摘要，不能在现有版本号下提供可变契约字节。

使用方通常应固定发布版本或软件包锁，而不是通过 Git 子模块直接耦合仓库。带签名的构建来源证明仍在规划中，当前 alpha 不声明已经具备该能力。
