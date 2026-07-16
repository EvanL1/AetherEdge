---
title: "AetherCloud 架构"
description: "了解模块化单体的进程、依赖方向和演进规则"
updated: 2026-07-16
status: mixed
---

# AetherCloud 架构

> 权威来源：[AetherCloud](https://github.com/EvanL1/AetherCloud/blob/main/docs/concepts/architecture.md)。此页面镜像到统一的 AetherIoT 文档中。

AetherCloud 从模块化单体起步：一个仓库、一组领域与应用模块，以及若干可以独立运行和扩展的小型组合根。这样可以让事务与重构保持局部，不必把长连接边缘会话和请求／响应式 API 负载耦合在一起。

## 目标进程模型
```text
                         application modules
                        /        |          \
                 HTTP API    CloudLink     workers
                    |           |            |
               REST / MCP   CloudLink/MQTT IaC/jobs/projections

                            web console
                                 |
                              HTTP API
```

- `api` 提供 REST、OpenAPI、WebSocket 通知以及更高版本的 MCP Streamable HTTP。
- `cloud-link` 拥有经过身份验证的长期 AetherEdge 会话和协议确认。
- `workers` 执行独立的基础设施计划、刷新提供程序清单、高级部署和遥测投影、重试工作并交付发件箱。
- `web` 是 API 的经过身份验证的客户端，没有特权数据路径。

只有 API 是仓库基础里程碑中已经装配的长时间运行进程。除公开的存活状态和产品元数据外，它还提供经过身份验证的审计 JSON 与有限的可恢复 SSE 快照；当前配置的承载身份和内存审计适配器不代表生产级 IAM 或持久化。`apps/mcp` 是已经实现的传输中立资源／工具接口，而不是可运行的 MCP 组合根。它把审计、数据导出和受管作业委托给应用用例；MCP SDK 传输与身份装配仍在规划中。CloudLink 包已经提供可独立启动的实验性 MQTT 入口生命周期，由严格编解码器和应用用例支撑，但尚无生产环境装配或持久 PostgreSQL 适配器。工作线程组合根仍在规划中。实验性入口不得被描述为生产级 CloudLink 服务。

## 依赖关系方向
```text
domain <- ports <- application <- interfaces/composition roots
             ^
             +---- adapters
```

域代码拥有标识符、值、不变量和状态转换。端口描述诸如工件仓库或作业分类帐之类的功能。应用程序代码授权并协调用例。适配器使用PostgreSQL、对象存储、身份提供程序和网络传输来实现端口。

提供程序适配器还实现应用程序端口。它们公开声明的功能和提供者本机扩展，而不会将供应商 SDK 类型泄漏到域或应用程序模块中。 OpenTofu 是默认基础架构引擎，Terraform 通过同一引擎端口兼容。

具体基础设施类型绝不能进入领域命名。应使用 `JobLedger`，而不是通用数据库端口；应使用 `ArtifactStore`，而不是对象存储 SDK 包装器。

## 初始有界上下文

- 身份和访问
- 提供商目录和云连接
- 云清单和规范化资源图
- 放置和部署堆栈
- 舰队和拓扑
- 遥测和警报
- 工件和版本
- 部署和所需状态
- 能力作业
- 审核
- 集成和导出

操作可观察性是横切适配器问题而不是业务有界上下文。 OpenTelemetry SDK 类型不会进入域或应用程序包，其信号不会取代 IoT 遥测或审核。

共享代码故意较小。有界上下文导出合约，而不是将其内部记录暴露给另一个上下文。

## 数据和消息传送

PostgreSQL 是默认的事务性 AetherCloud 产品存储。第一个 Gateway Identity SQL 适配器和迁移已实施，而组合根连接和所有其他 PostgreSQL 有界上下文适配器仍在计划中。基础设施状态不同：每个提供商范围的部署堆栈都使用自己的远程锁定后端。工作线程在基础设施状态方面是无状态的，并使用保存的 JSON 计划，而不是抓取终端输出或原始状态文本。

第一个后台工作实现应使用事务发件箱和 PostgreSQL 支持的工作线程。仅当测量的吞吐量、保留或消费者隔离需要时才会引入代理。

第一个实验性 CloudLink 线合约在 MQTT 上使用版本化的严格 JSON，因此 TypeScript 和 Rust 可以执行相同的装置。更高版本的二进制编码需要联合 AetherCloud/AetherEdge 审核，并且无法更改业务身份或确认语义。序列字段保留规范的十进制字符串，无需不安全的JavaScript-数字转换。

请参阅[多云融合](/aethercloud/concepts/multi-cloud-fusion)了解提供程序、状态、凭证和执行边界。

## 服务提取规则

仅当至少测量以下其中一项时，模块才会成为单独拥有的服务：

- 显着不同的扩展特征
- 安全或故障隔离边界
- 另一个团队拥有的独立部署节奏
- 无法在流程中满足的持久性或区域放置要求

在更改部署拓扑之前，提取必须保留应用程序契约并添加故障模式测试。

## IoT 上下文协作

完整的有界上下文映射在 [IoT 云功能映射](/aethercloud/concepts/iot-cloud-capability-map) 中维护。上下文公开应用程序契约或事件而不是数据库记录：
```text
Identity and Access -> authorization decisions
Fleet Identity -> CloudLink credential verification
Runtime Catalog -> deployment compatibility and Job eligibility
Artifact Registry -> immutable Deployment revision references
CloudLink -> application ingestion use cases
Application transactions -> audit + outbox
Outbox -> projections, notifications, webhooks, exports, SSE, and WebSocket
```

HTTP、CLI、MCP 和 CloudLink 接口均解码外部输入并调用相同的命令/查询应用程序 API。即使源自边缘的事实是描述性的，改变云投影的边缘观察也是经过验证的摄取命令。查询并不意味着“可以安全写入，因为数据来自边缘”。

## HTTP 和 CloudLink 边界

HTTP 对人员和服务帐户进行身份验证、评估租户授权并创建云意图，例如所需状态或受治理作业。它保留了该意图和发件箱记录；它从不通过实时边缘套接字发送。

CloudLink 验证网关凭据并拥有会话防护、协议协商、流序列、持久确认、恢复和反压。它读取应用程序拥有的邮箱并调用拥有的有界上下文以进行遥测、清单、部署或收据摄取。它不会实现这些业务状态机或公开任意 RPC。

这就是为什么即使产品仍然是模块化整体，API 和 CloudLink 组合根也是独立的。请求/响应流量和长期连接具有不同的扩展、耗尽和故障行为。工作进程是发件箱传送、预测、重试、保留和受控后台执行的第三个独立根。

## 权限和数据放置

| 存储 | 职责 | 明确不具权威性的内容 |
| -------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------- |
| PostgreSQL | 租户／IAM、舰队身份、最新清单、会话游标、工件元数据、期望／报告／应用元数据、作业账本、收据索引、审计、收件箱、发件箱、配额 | 边缘实时点状态；提供商资源；基础设施状态 |
| 对象存储 | 不可变工件、签名、出处、原始/冷遥测批次、大量证据、导出 | 可变聚合状态或凭证 |
| 时间序列或分析存储 | 历史遥测/事件投影、下采样、聚合 | 当前边缘点值或警报真相 |
| 部署堆栈远程后端 | 一个提供商范围的锁定基础设施状态 | 物联网队列、遥测或跨提供商全局状态 |

PostgreSQL最初可能会实现历史存储适配器，但应用程序端口保持独立，因此测量的摄取或分析要求可以稍后选择不同的适配器。

操作跟踪和指标离开组合通过可选的 OpenTelemetry 适配器和 OTLP 导出器根源。他们的后端没有事务或权限角色。导出器失败无法更改命令、持久确认或工作结果。

所需、报告和应用是单独持久的事实。投影总是带有源观察时间和新鲜度。任何数据库选择都不会更改边缘/云/提供商边界定义的权限。

## 事务收件箱和发件箱

命令事务以原子方式写入其聚合更改、所需的审核记录和发件箱消息。边缘摄取在 CloudLink 确认之前自动记录其重复数据删除身份和接受的业务事实。工作进程租用工作，在有界策略内重试，并使死信状态可见。

PostgreSQL支持的交付是默认设置。 Kafka 或其他代理仅在测量吞吐量、保留或消费者隔离要求后才引入，并且永远不会更改命令幂等性或持久确认语义。出站 Webhook 尝试意图在网络 I/O 之前保留，在有界重试中使用一个稳定的传递身份，并在耗尽时变成可见的死信。数据导出发布不可变的对象引用，而不是通过 API 进程返回无限制的历史记录。请参阅[审核和集成](/aethercloud/concepts/audit-and-integrations)。

多云可移植性是通过可独立部署的单元和提供商数据库配置文件来表达的，而不是通用的最低公分母数据库API。一个单元具有一个权威的 PostgreSQL 写入器拓扑；租户有一个明确的家庭单元。跨云备份、灾难恢复和租户迁移需要防护和受管控的工作流程。读取[PostgreSQL持久性和多云单元](/aethercloud/concepts/persistence-and-multi-cloud-cells)。

## 租户隔离

每个租户拥有的聚合、事件、唯一约束、对象前缀、收件箱、发件箱、缓存密钥和分析分区都带有 `TenantId`。用户或服务帐户接口从经过身份验证的身份解析租户上下文；网关接口根据已验证的声明或活动凭证解析它。正文和路径标识符仅在该上下文存在后才选择资源。

计划的 PostgreSQL 适配器将应用程序强制作用域、复合租户密钥和行级安全性结合起来作为深度防御。跨租户访问只能通过具有单独权限和审核证据的显式平台用例来实现。

实现 CloudLink 集成前，请阅读 [CloudLink MQTT 参考](/aethercloud/reference/cloudlink-mqtt-v1)和 [CloudLink 可靠传输与生命周期](/aethercloud/concepts/cloudlink-and-core-state-machines)。

在增加历史记录或数据摄取前，请阅读 [IoT 业务遥测](/aethercloud/concepts/iot-telemetry)；在增加监测手段前，请阅读[运行可观测性](/aethercloud/concepts/operational-observability)。当前可用能力和仍在规划中的部分以[物联网云路线图](/aethercloud/guides/iot-cloud-roadmap)为准。
