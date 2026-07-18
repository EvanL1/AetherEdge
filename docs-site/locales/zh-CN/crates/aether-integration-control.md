---
title: "aether-integration-control"
description: "AetherContracts 0.1.0-alpha.4 候选版集成控制协议的实验性默认关闭 AetherEdge 绑定"
---

# aether-integration-control

这个包是 AetherContracts `0.1.0-alpha.4` 候选版中已冻结
`integration-control` v1alpha1 协议的实验性、默认关闭 AetherEdge 绑定。

它提供：

- 严格且闭合的动作提议和最终回执编解码；
- RFC 8785 意图摘要和精确签名字段投影；
- 当前会话、凭据代次、有效期和拓扑代次隔离；
- 由边缘端最终决定的投运、委托、权限、确认和审计端口；
- 相同任务和相同摘要的持久去重；超时、崩溃或结果未知后不会自动重试接入方动作；
- 使用精确 CloudLink 确认身份持久重放最终回执；
- 唯一语义能力 `device.power.set.v1`，目标只能是 `light`、`switch` 或 `fan`
  实体的布尔点位 `is_on`。

接入方适配器接收闭合的语义动作。它不能接收由调用方选择的接入方域、服务、服务参数、
地址或凭据。接入方接受请求只会记录关联证据；物理动作是否完成、任务是否成功仍然未知。

这个包没有运行时环境变量开关。`IntegrationControlConfig` 默认关闭，启用仍然需要
明确装配云端签名验证器、本地授权、审计接收方、持久账本和接入方执行器。Home Assistant、
CloudLink MQTT 和本地存储扩展中的可选 `integration-control` 构建特性公开这些适配
接缝。`aether-io` 只会通过单独编译且默认关闭的
`home-assistant-integration-control` 构建特性和三个显式运行时开关把它们组合起来。

```bash
cargo test -p aether-integration-control
cargo test -p aether-home-assistant-bridge --features integration-control
cargo test -p aether-store-local --features integration-control
```
