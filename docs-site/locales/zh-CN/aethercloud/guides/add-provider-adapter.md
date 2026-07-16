---
title: "添加提供商适配器"
description: "在应用程序端口后面实现提供者发现，并使用共享一致性套件进行证明"
updated: 2026-07-14
status: implemented
---

# 添加提供程序适配器

> 权威来源：[AetherCloud](https://github.com/EvanL1/AetherCloud/blob/main/docs/guides/add-provider-adapter.md)。此页面镜像到统一的 AetherIoT 文档中。

本指南介绍了已实施的只读提供程序发现合约。它不描述通用云 SDK 包装器。提供程序适配器将一个显式 CloudConnection 转换为提供程序观察，同时保留提供程序本机标识符和命名空间功能。

## 已实现的合约

当前基础包括：

- `CloudProviderDescriptor`、`CloudConnection` 和 `ProviderRegion` 域值
- 应用程序拥有的 `ProviderAdapter` 端口和动态`ProviderAdapterRegistry`
- 具有 `cloud-connections:read` 权限的租户/项目范围 `DiscoverProviderRegions` 查询
- 类型化身份验证、配置、权限、速率限制和可用性失败
- 针对提供程序身份、连接身份、观察时间、重复区域和未声明功能的应用程序边界检查
- `providerAdapterConformance`，可重用适配器测试套件
- `MemoryProviderAdapter`，一种确定性测试适配器

尚未实现提供程序 SDK、凭据解析程序、持久性适配器、公共发现端点或基础设施突变。

## 适配器工作流程

1. 使用稳定的小写提供程序身份、提供程序类型、可移植功能和命名空间提供程序定义一个描述符功能。
2. 在 `packages/domain` 和 `packages/application` 外部实现应用程序拥有的 `ProviderAdapter` 接口。
3. 仅接受为授权租户和项目范围解析的 CloudConnection。它的 `credentialSource` 是工作负载身份或秘密引用，而不是凭证值。
4. 解决适配器边界内的短期访问。不要将令牌放置在快照、错误、日志、测试夹具、提示或生成的模块中。
5. 将预期的提供程序故障映射到键入的结果。请勿将 SDK 错误泄漏为应用程序合约或将速率限制报告为空区域列表。
6. 返回规范的 UTC 观察时间、准确的提供程序和连接身份、唯一区域以及描述符声明的功能。
7. 运行共享一致性套件和提供程序特定的测试夹具。
8. 仅在组合根中注册适配器。

最小形状：
```ts
export class ExampleProviderAdapter implements ProviderAdapter {
  readonly descriptor = exampleProviderDescriptor;

  async discoverRegions(
    request: ProviderRegionDiscoveryRequest,
  ): Promise<ProviderRegionDiscoveryResult> {
    // Resolve short-lived access from request.connection.credentialSource.
    // Decode the provider response and return typed domain observations.
  }
}
```

## 所需的一致性

每个适配器测试都会从 `@aether-cloud/provider-conformance` 导入 `providerAdapterConformance`。提供一个新的适配器和属于该提供商的 CloudConnection。共享套件检查提供商和连接范围、规范观察时间、唯一区域身份、声明的功能以及拒绝不匹配的连接。

对于响应解码、分页、身份验证、限制、配额行为和格式错误的上游数据，仍然需要特定于提供商的测试。默认测试路径使用测试夹具，不得调用真实的云帐户。

## 单独的生命周期支持

基础设施规划现在在单独的仅计划 `InfrastructureEngine` 端口后面实施。实现真正的本地OpenTofu计划流程执行； Terraform 流程执行和基础设施变更仍在计划中。未来的提供程序适配器可以选择版本化的提供程序模块并标准化提供程序观察，但不要向只读发现方法添加 `plan`、`apply`、`destroy`、导入或状态修复。

读取[多云融合](/aethercloud/concepts/multi-cloud-fusion)以了解状态和权限规则，并读取[仓库布局](/aethercloud/reference/repository-layout)以了解依赖关系放置。
