---
title: "aether-cloudlink-mqtt"
description: "实验性 CloudLink 边缘基础的 Broker 中立 MQTT v3.1.1/QoS 1 绑定"
updated: 2026-07-16
---

# aether-cloudlink-mqtt

代理中立的 MQTT v3.1.1/QoS 1 绑定，用于实验性 CloudLink 边缘基础。它验证用户选择的端点、TLS/身份验证设置、主题前缀和网关命名空间；使用 `retain = false` 发布；仅订阅同一网关的会话/ACK/重播主题；关联 QoS 1 PUBACK；并独立于本地边缘行为重新连接。

PUBACK 仅是传输证据。专用 CloudLink 持久队列仅由经过验证的应用程序持久 ACK 删除。

默认测试不需要代理。请参阅 `docs/reference/cloudlink-mqtt-v1.md` 了解选择加入的共享代理工具和环境变量。
