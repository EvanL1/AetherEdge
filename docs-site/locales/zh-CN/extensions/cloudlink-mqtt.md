---
title: "aether-cloudlink-mqtt"
description: "实验性 CloudLink 边缘基础的消息代理中立 MQTT v3.1.1/QoS 1 绑定"
updated: 2026-07-17
---

# aether-cloudlink-mqtt

代理中立的 MQTT v3.1.1/QoS 1 绑定，用于实验性 CloudLink 边缘基础。它验证用户选择的端点、TLS/身份验证设置、主题前缀和网关命名空间；使用 `retain = false` 发布；仅订阅同一网关的会话/ACK/重播主题；关联 QoS 1 PUBACK；并独立于本地边缘行为重新连接。

PUBACK 仅是传输证据。专用 CloudLink 持久队列仅由经过验证的应用程序持久 ACK 删除。

实验性的 `aether.cloudlink.integration.v1alpha1` 扩展增加
`up/integration/topology` 和 `up/integration/observations` 两条上行主题。只有兼容的
云端接收方先启用、且运行时清单明确声明该扩展后，边缘端才会启用它。拓扑与观测使用
独立持久流；拓扑必须保持原子性，观测批次只能在完整观测之间拆分。每条完整 MQTT 消息
不得超过 256 KiB。这两条主题只传递只读投影，不提供物理控制。

单独的 `integration-control` 构建特性提供唯一的实验性控制请求主题、回执主题和显式激活
方法。这些主题不会出现在任何基础连接中。`aether-io` 只有在当前 CloudLink 会话已经
接纳、且新的会话代次已经持久保存后才会激活它们；每次重连都会恢复为关闭状态，必须由
新会话再次激活。控制回执的 PUBACK 仍然只是传输证据，绝不会删除持久回执。签名校验、
拓扑、本地策略、审计和作业去重属于应用与装配层，不属于 MQTT 适配器。

默认测试不需要代理。请参阅 `docs/reference/cloudlink-mqtt-v1.md` 了解选择加入的共享代理工具和环境变量。
