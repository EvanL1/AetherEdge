---
title: "当前实施审计"
description: "区分可执行 AetherCloud 层、代理证据和 PostgreSQL 遥测 ACK 切片与缺失的生产表面"
updated: 2026-07-16
status: mixed
---

# 当前实施审核

> 权威来源：[AetherCloud](https://github.com/EvanL1/AetherCloud/blob/main/docs/concepts/current-state-audit.md)。此页面已镜像到统一的 AetherIoT 文档中。

截至 2026 年 7 月 16 日，此审核以证据为基础。下面的 `Implemented` 始终命名可执行层。它并不意味着公共 API、持久生产适配器或完整的 AetherEdge 集成，除非这些层已命名。

## 仓库基线

仓库位于 `main` 上。本次审核的变更前 HEAD 为 `d731ecb`；早期的引导基线是 `d698a97`。现有的跟踪和未跟踪工作区更改保留用户拥有的项目状态，并且无权重置、替换或删除它们。

工作区使用 Node.js 24、pnpm 11、TypeScript 5.9、ESM、严格类型检查、ESLint、Prettier、Vitest、80% 的覆盖率门以及用于代理文档契约的 Node 测试套件。证据位于 `package.json`、`tsconfig.json`、`eslint.config.mjs`、`vitest.config.ts` 和 `tests/ai-docs.test.mjs` 中。

在此功能扩展开始之前，基线 `pnpm check` 成功完成了 114 项 Vitest 测试和 8 项文档契约测试。

## 可执行产品层

| 能力 | 可执行证据 | 诚实状态 |
| ------------------------------------------ | -------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 边缘/云/提供商权威值 | `packages/domain/src/authority.ts`及其测试 | 实现的域值，而不是授权服务 |
| 提供商目录和区域发现 | `packages/domain`， `packages/application/src/discover-provider-regions.ts`、提供程序一致性和内存适配器 | 已实现域/应用程序/测试适配器；真实的提供商和HTTP计划的 |
| 部署堆栈和受管理的计划 | 域/应用程序计划模块、基础设施一致性、内存适配器和`adapters/infrastructure/opentofu` | 已实施的实际本地OpenTofu计划工作进程；生产远程状态、持久加密工件、审核和HTTP计划 |
| 网关注册和注册声明 | 网关域/应用程序模块以及`adapters/fleet/memory`和`adapters/fleet/postgres` | 域/应用程序、内存一致性、PostgreSQL SQL/迁移/驱动程序适配器以及已实现的原子网关/审核/发件箱写入；生产数据库组成、身份绑定、CA/KMS 和HTTP计划的 |
| CloudLink会话和心跳基础 | CloudLink域/应用程序模块和`adapters/cloudlink/memory` | 已实施的凭据身份验证用例、纪元防护、游标恢复和内存适配器； PostgreSQL 计划 |
| 实验 CloudLink MQTT 入口 | `adapters/cloudlink/mqtt`、`apps/cloudlink`、`contracts/cloudlink/v1` 和选择加入双代理测试 | 严格的 alpha.3 编解码器和故障类、MQTT.js 入口、应用程序实施桥接、真实 Mosquitto/AWS 证据和 alpha 故障矩阵；桥接受精确的持久遥测 ACK，而生产共享代理身份验证、多样本映射、持久数据丢失持久性、会话持久性和组合仍按计划进行 |
| 运行时清单和功能基础 | 已实现运行时域/应用程序模块和 `adapters/runtime/memory` | AetherEdge v1 校验和、单调历史记录、报告/查询、内存适配器和实验性 MQTT 映射； PostgreSQL/HTTP 已计划 |
| 物联网遥测摄取和历史记录 | 遥测域/应用程序模块以及 `adapters/telemetry/memory` 和 `adapters/telemetry/postgres` | 原子重放/间隙/光标/历史语义，PostgreSQL已实施收据/事实/审计/集成-发件箱/精确-ACK 事务、租赁交付用例、强制 RLS 和 PostgreSQL 18 项崩溃边界测试；公共HTTP、生产组合、数据丢失持久性和分析仍在计划中 |
| 警报事实和工作流程投影 | 警报域/应用程序模块和`adapters/alarm/memory` | 通过内存投影实现边缘事实排序和云确认；生产持久性和线路规划 |
| 可操作的 OpenTelemetry 基础 | `adapters/observability/opentelemetry` | 实现无操作、内存中、OTLP HTTP、有界队列、W3C 提取和摄取装饰器；计划的广泛根连接 |
| ArtifactRegistry基础 | Artifact域/应用程序模块和`adapters/artifacts/memory` | 实施不可变生命周期、摘要/内容/签名检查、通道冲突、查询和原子内存审计/发件箱；生产存储/HTTP计划 |
| 所需/报告/应用部署 | 边缘部署域/应用程序模块和`adapters/deployment/memory` | 已发布工件所需意图、报告/应用分离、暂停/恢复/取消/回滚/未知、查询和原子内存审计/发件箱已实现； Scheduler/wire/PostgreSQL/HTTP 计划的 |
| 受控功能作业和收据 | 受控作业域/应用程序模块和`adapters/jobs/memory` | 功能门控创建、确认、队列/提供、未知/取消意图、有序验证收据、查询和原子内存审核/发件箱已实施； Delivery/wire/PostgreSQL/HTTP/MCP 已计划 |
| 审核搜索 | 审核域/应用程序模块和`adapters/audit/memory` | 租户/项目范围的仅附加值、有界游标查询和已实现的内存适配器； PostgreSQL 持久性计划 |
| Webhook 订阅和交付 | 集成域/应用程序模块和 `adapters/integration/memory` | 已实现稳定目标引用、白名单、有界重试/死信/重新驱动和原子内存证据；生产发送者、机密、工作进程和PostgreSQL计划的 |
| 数据导出 | 数据导出域/应用程序模块和`adapters/integration/memory` | 实施的受控异步请求/结果/查询和不可变对象结果元数据；生产对象存储、工作线程、下载和PostgreSQL计划的 |
| MCP应用程序接口 | `apps/mcp/src/mcp-interface.ts`和行为测试 | 功能/审核资源以及数据导出和作业工具委托给具有完整治理元数据的应用程序用例； MCP SDK 传输/根、生产身份、速率限制和持久性计划 |
| API 流程 | `apps/api/src/app.ts` 和 `apps/api/test/app.test.ts` | 实施公共卫生/平台以及经过身份验证的审计 JSON 和有限可恢复 SSE 快照路由；已规划生产身份和持久审核适配器 |
| 代理文档契约 | `llms.txt`、清单、技能、不变量、ADR 和节点测试 | 已实现仓库接口 |

基础设施引擎端口特意仅供规划。不存在可执行的应用、销毁、导入或状态修复操作。

PostgreSQL 网关和遥测适配器是真正的 SQL/驱动程序边界，具有脚本化事务/迁移测试和使用受限应用程序角色选择加入 PostgreSQL 18 集成测试。遥测测试证明在提交之前没有 ACK，并且在不确定的提交之后恢复相同的 ACK。这并不是正在运行的托管数据库、生产迁移编排、凭据、工作线程部署或备份/恢复的证据。

## 审查了没有可执行产品表面的契约

以下内容在文档中设计或命名，但在此审核点没有相应的域/应用程序实现：

- 租户/用户/服务帐户 IAM、RBAC/ABAC、API 凭据和持久审计
- 站点、实例、点元数据、组、拓扑和动态查询
- 生产网关凭证、撤销、恢复、CA/KMS、持久凭证绑定、数据库组合和迁移执行；网关聚合 SQL 适配器仅涵盖注册和声明状态
- 生产 CloudLink 流程配置、PostgreSQL 会话/收件箱/发件箱/光标、多实例所有权、背压和实际边缘集成；传输中立会话基础和实验性 MQTT 编解码器/桥/入口和公共 alpha.3 固定解码是可执行的，而生产凭证生命周期和 Broker ACL 证据、完整的耐碰撞门、多样本批量索引、数据丢失持久性和生产过程崩溃布线仍然是计划中的门；遥测 ACK 事务/工作线程和选择加入的双线束/故障套件是可执行证据
- PostgreSQL运行时清单历史记录、公共队列查询、持久审核/发件箱以及实例/点目录；有界 v1 报告/查询和实验性 MQTT 信封是可执行的
- 生产 PostgreSQL 遥测组合和迁移、多实例 ACK 工作线程操作、多样本线路/应用程序映射、持久数据丢失事实、下采样、冷导出和公共 API；实现了 PostgreSQL 遥测仓库/ACK 事务和共享 alpha.3 夹具执行
- PostgreSQL 警报事实/投影/工作流程、CloudLink 警报线适配器、分配/注释/搜索以及公共 API
- PostgreSQL 工件元数据、生产对象存储/签名验证器、持久审核/发件箱、上传API、弃用/撤回命令和 HTTP；发布/查询和内存一致性基础是可执行的
- PostgreSQL 部署分类账、目标快照、金丝雀/批量调度程序、CloudLink 线路、公共 HTTP、持久审计/发件箱和 AetherEdge 对应项；单目标域/应用程序/内存基础是可执行的
- PostgreSQL管理的作业分类帐/收件箱、运行时清单目录来源、CloudLink交付、公共HTTP和剩余MCP暴露、调度/到期工作进程、大型证据存储和AetherEdge对应物；功能门控域/应用程序/内存基础是可执行的
- PostgreSQL审核/发件箱/交付/导出适配器、目标注册表和机密、强化的 Webhook 发送器/签名/SSRF 防御、重试和导出工作线程、实时 SSE/WebSocket 扇出、对象存储和授权导出下载、配额以及 MCP 线路/组合根；当前审计 JSON/有限 SSE、传输中性MCP 资源/工具和集成内存基础是可执行的
- 收集器部署和 OpenTelemetry 检测超出了已实现的遥测摄取装饰器

机器可读的[应用程序合约目录](/aethercloud/reference/application-contracts) 使用 `partial` 作为尚未形成生产产品的可执行内层Surface。

## AetherEdge边界证据

AetherEdge在 `contracts/runtime/runtime-manifest.v1.schema.json` 具有稳定的运行时清单 JSON Schema、收购拥有的 `PointSample`/`PointQuality` 模型以及具有至少一次转发功能的本地 `DurableOutbox`。其 SHM 仍然是实时 T/S 值的权威，其本地警报流仍然是警报事实的权威。

AetherEdge 还具有兼容性 MQTT 上行链路和实例导出端点。这些负载不定义 AetherCloud CloudLink 会话纪元、每个流的持久游标、摘要冲突行为或云持久性确认。新的 AetherCloud JSON/MQTT 实现是公共 alpha.3 版本的实验性使用方。因此，现有的旧版 MQTT 有效负载仍然是参考证据，不会默默地被视为 CloudLink v1。

两种产品都固定并执行完整的公共 alpha.3 固定清单，并且选择加入的真实 Mosquitto 线束会记录双边缘/云 alpha 故障证据。身份验证仍然是一个实验性提案，并且 ACK 仍然未签名。存在用于接受遥测的耐崩溃PostgreSQL ACK 存储/发件箱，但不存在完整的会话/凭证/丢失标记生成路径和组合。

## 材料差距和设计修正

现有的能力图正确地将物联网平台工作与多云基础设施分开，但它尚未提供专用的物联网遥测契约、操作可观察性边界或用于能力治理和实施层的机器可读目录。 ADR-0007 和 ADR-0008 添加了这些决策，但不更改 ADR-0001 的边缘优先权限。在审计 HTTP/SSE 和集成状态机基础变得可执行后，ADR-0012 添加了持久审计/出站事务边界。 ADR-0015 可防止CloudLink 传输进度绕过共享代理源身份验证、公共字节、故障注入或崩溃持久性。

任何变异用例的生产暴露仍然需要一个事务来实现聚合状态、所需的审核和发件箱交付。内存适配器是一致性工具，永远无法满足生产持久性要求。

## 验证证据

已于 2026 年 7 月 16 日使用仓库的默认无外部服务路径验证了已完成的基础：

- `pnpm check`：通过了 443 个 Vitest 行为测试和 18 个 Node 合约测试；通过了 TypeScript、ESLint 和 Prettier 检查
- `pnpm test:coverage`：87.40% 的语句、80.42% 的分支、96.86% 的函数和 88.97% 的行
- `pnpm test:mqtt-integration`：选择加入的 MQTT.js 传输测试交换了隔离的 QoS 1，通过 Eclipse Mosquitto 2 的非保留消息
- `pnpm test:postgres-integration`：使用非超级用户、非`BYPASSRLS`应用程序角色针对 PostgreSQL 18 传递的网关注册/声明流程和遥测提交/重播/崩溃边界案例
- `pnpm test:cloudlink-alpha-harness`：本地Mosquitto/AetherEdge/AetherCloud 双进程 ACK 丢失、重启、重放、冲突、间隙、过期、部分结果和数据丢失矩阵已通过；其组成仍然有意报告没有生产崩溃持久存储
- `pnpm audit --prod`：没有已知的生产依赖漏洞

这些结果涵盖可执行内层和内存一致性适配器； PostgreSQL 结果涵盖网关身份和接受遥测 ACK 片段。它们并不能证明存在其他计划的 PostgreSQL 适配器、对象存储集成、完整生产 CloudLink 身份验证/持久性、部署的工作线程或生产身份集成。
