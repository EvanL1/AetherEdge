---
title: "部署拓扑"
description: "AetherIoT 支持将物理权限保持在边缘的部署，同时仅在创造价值的地方添加云协调。"
updated: 2026-07-16
---

# 部署拓扑

AetherIoT支持将物理权限保留在边缘的部署，同时仅在创造价值的地方添加云协调。

## 仅限边缘站点
```text
field devices -> AetherEdge -> local applications
                    |
                    `-> embedded history and durable local outbox
```

使用此拓扑进行评估、隔离站点和不需要云协调的应用程序。默认运行时不需要代理、PostgreSQL、云帐户、浏览器或 AI 客户端。

## 使用 AetherCloud 的 Edge
```text
field devices -> AetherEdge == CloudLink ==> AetherCloud -> operator clients
                    ^                         |
                    |                         `-> provider adapters
                    `--- final local policy        and governed jobs
```

AetherContracts 定义共享的 CloudLink 行为。 AetherCloud 记录带时间戳的预测并发送所需的状态或受治理的作业。 AetherEdge 在本地策略下验证、接受、拒绝、过期或应用该意图。

当前的 CloudLink alpha 路径是实验性的。传统边缘上行链路保持默认状态，直到身份验证、签名确认、崩溃持久性和联合一致性门通过。

## 多云控制平面单元
```text
tenant home cell
├── AetherCloud application modules
├── PostgreSQL transactional state
├── encrypted artifact storage
└── capability-driven provider adapters
    ├── provider A
    └── provider B
```

一个部署堆栈拥有一种独立锁定的基础设施状态。单元不会创建租户范围或跨提供商的全局状态文件。每个提供商继续拥有其本机资源状态。

## AetherEMS解决方案

AetherEMS通过AetherEdge分层能源模型、工作流程和应用程序，并可以通过公共合约连接到AetherCloud。它无法更改边缘、云或合约权限边界。

## 失败预期

| 失败 | 所需行为 |
| --- | --- |
| 互联网或云不可用 | AetherEdge继续委托采集、本地历史记录、规则、警报、联锁和控制 |
| 代理不可用 | 本地持久发件箱保留有限的工作； MQTT 交付不是应用程序收据 |
| 提供程序 API 不可用 | AetherCloud 公开类型化失败，并且永远不会将其规范化为空的成功观察 |
| AI 客户端不可用 | 确定性行为保持不变 |
| 云作业被本地策略拒绝 | 拒绝仍然是可审计的事实；云意图永远不会绕过边缘 |
