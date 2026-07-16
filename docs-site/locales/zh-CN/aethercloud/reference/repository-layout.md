---
title: "仓库布局"
description: "将代码放置在正确的应用程序、域、适配器或组合根包中"
updated: 2026-07-15
status: mixed
---

# 仓库布局

> 权威来源：[AetherCloud](https://github.com/EvanL1/AetherCloud/blob/main/docs/reference/repository-layout.md)。此页面镜像到统一的 AetherIoT 文档中。

工作区将稳定的产品逻辑与交付机制分开。一些目录是由仓库基础建立的，其他目录是在实现第一个垂直切片时添加的。
```text
apps/
  api/                  HTTP composition root
  mcp/                  implemented transport-neutral MCP application interface; wire root planned
  cloudlink/            experimental MQTT ingress composition; production wiring planned
  worker/               planned background-work composition root
  web/                  planned operator client
packages/
  domain/               values, identifiers, invariants
  application/          transport-neutral use cases
  provider-conformance/ reusable Provider Adapter contract tests
  infrastructure-conformance/ reusable Infrastructure Engine contract tests
adapters/
  fleet/memory/          implemented Gateway repository/token test adapters
  fleet/postgres/        implemented Gateway SQL/migration/driver adapter; production composition planned
  cloudlink/memory/      implemented session and heartbeat test adapter
  cloudlink/mqtt/        experimental strict codec and MQTT.js transport adapter
  runtime/memory/        implemented Runtime Manifest history test adapter
  telemetry/memory/      implemented atomic telemetry conformance adapter
  alarm/memory/          implemented alarm projection conformance adapter
  artifacts/memory/      implemented Artifact Registry conformance adapter
  deployment/memory/     implemented edge deployment conformance adapter
  jobs/memory/           implemented governed Job ledger conformance adapter
  audit/memory/          implemented append-only Audit query test adapter
  integration/memory/    implemented webhook subscription/delivery and export test adapters
  observability/opentelemetry/ optional implemented operational-signal adapter
  providers/memory/      deterministic implemented test adapter
  providers/<provider>/  planned SDK discovery and normalization adapters
  infrastructure/memory/ implemented deterministic Plan engine and repository
  infrastructure/opentofu/ implemented real local Plan-only CLI adapter
  infrastructure/terraform/ planned Terraform CLI adapter
  persistence/          planned shared migration runner and object-store adapters
infra/modules/           planned versioned provider-specific IaC modules
contracts/cloudlink/     experimental JSON Schemas and golden fixtures
ai/                      invariants and machine-readable documentation catalog
skills/aether-cloud/     coding-agent workflow and routing
docs/                    concepts, guides, reference, and decisions
tests/                   repository-wide contract tests
```

## 放置规则

当类型无需 I/O 表达业务含义时，将类型放入 `domain` 中。当接口描述用例所需的功能时，将接口放入端口拥有的应用程序模块中。将编排放入 `application` 中。将 Fastify 路由、进程信号、环境访问和依赖项构造放入应用中。

适配器可能依赖于端口和第三方SDK。端口不能依赖于其适配器。不要创建通用实用程序包；放置一个具有拥有其语义的概念的帮助器。

提供者中立的意图属于域和应用程序包。特定于提供商的模块、SDK 客户端、身份验证和规范化保留在适配器和 `infra/modules` 下。工作器组合根选择一个提供者适配器和一个 `InfrastructureEngine`；在域用例中都没有选择。

## 包导出

包公开故意的公共入口点。通过声明的导出导入另一个包，而不是进入其内部目录。避免分桶导出意外地使持久性记录或框架类型成为应用程序契约的一部分。

## 添加有界上下文

从一个可观察用例及其域语言开始。在选择持久性之前添加测试和应用程序契约。如果上下文需要适配器，请提供内存一致性实现，以便默认测试保持独立。

仅在上下文更改系统职责或依赖项时更新[架构](/aethercloud/concepts/architecture)；例行文件添加属于此参考页或包自述文件。
