---
title: "TCK v1 alpha 1 说明"
description: "仓库测试编译每个规范JSON Schema，验证有效和无效的测试夹具，保留上下文无效的区别，并验证每个......"
updated: 2026-07-15
id: tck-v1alpha1
status: experimental
version: 0.1.0-alpha.3
normative: true
---

# TCK v1 alpha 1 说明

> 本中文页面用于帮助理解。规范性定义、Schema、测试夹具和 TCK 以 AetherContracts 对应版本的英文发布内容为唯一权威。

> 权威来源：[AetherContracts](https://github.com/EvanL1/AetherContracts/blob/main/spec/tck-v1alpha1.md)。此页面镜像到统一的 AetherIoT 文档中。

仓库测试编译每个规范的 JSON Schema，验证有效和无效的测试夹具，保留上下文无效的区别，并验证每个声明的 SHA-256。

仓库引用运行程序执行已发布的核心方案，以实现整数优先级、严格的原始 JSON、业务和运行时清单摘要、最小化会话/重播/ACK 上下文、数据丢失和游标规则以及 Thing Model 键冲突。它不是生产 CloudLink 状态机。电线无效的测试夹具目前已被证明是结构性排斥；精确的Schema到故障代码和 JSON 路径映射仍在计划中。

便携式黑盒运行器协议计划为基于标准输入和输出的 NDJSON。操作为 `validate`、`canonicalize`、`digest`、`verify-signature` 和 `check-compatibility`。它将比较接受、稳定故障代码、JSON 路径、规范字节、摘要和状态结果；它不会比较特定于语言的错误散文。

TypeScript、Rust、C 和 C++ 仍处于试验阶段，直到各自执行相同的清单和场景集。真正的Broker双线束和破坏性故障注入是单独的选择加入证据，并且永远不会进入默认的离线测试路径。
