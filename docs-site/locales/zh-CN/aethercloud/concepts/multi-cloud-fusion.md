---
title: "多云融合"
description: "统一云生命周期和策略，无需消除提供商功能或共享一个全局状态"
updated: 2026-07-14
status: mixed
---

# 多云融合

> 权威来源：[AetherCloud](https://github.com/EvanL1/AetherCloud/blob/main/docs/concepts/multi-cloud-fusion.md)。此页面镜像到统一的 AetherIoT 文档中。

多云融合是 AetherCloud 的主要云功能。它为租户提供跨公共云、私有云、主权环境和本地基础设施的资源图、策略模型、计划工作流程和审计跟踪。它不会假装这些提供商公开相同的资源。

## 公共层

AetherCloud拥有一个小型的、功能驱动的词汇表：

- 提供商和云连接身份
- 区域、专区和放置约束
- 计算、对象存储、专用网络、托管 Kubernetes、托管PostgreSQL和 IoT 等功能入口
- 部署堆栈、所需拓扑、保存的计划、策略决策、作业和标准化资源观察
- 成本、配额、运行状况、出处和新鲜度元数据

提供商宣传其可用功能。放置仅选择满足所请求的功能和租户策略的提供商连接。选择是明确的；凭证形状绝不会用于推断提供程序。

## 当前实现

仓库当前实现提供程序描述符、CloudConnection、ProviderRegion 观察、动态 ProviderAdapter 注册表以及授权租户/项目范围的 `DiscoverProviderRegions` 应用程序查询。输入和适配器输出在成为标准化观察之前经过运行时验证。可重用的一致性套件和内存适配器使合约无需云帐户即可执行。

该域还实现了 DeploymentStack 定义，其租户、项目、提供商、连接和状态密钥均派生自一个活动的 CloudConnection。 State后端是一个不透明的远程引用；锁定和加密是强制性的，而不是调用者选择的选项。放置观察必须匹配相同的提供者和连接，然后才能选择其区域，并且堆栈会记录规范观察时间，以便后续策略可以拒绝过时的放置证据。

应用程序现在实现受治理的、租户/项目范围的 `PlanDeploymentStack` 命令、动态 OpenTofu/Terraform `InfrastructureEngine` 注册表、运行时引擎输出验证、更改摘要、策略接收和幂等规划仓库端口。共享一致性套件、内存引擎和内存计划仓库使合约无需云帐户即可执行。真正的 OpenTofu CLI 适配器动态探测其版本，验证不可变的源工件，在有界临时工作区中运行仅 argv 保存的计划，解析版本化 JSON，通过加密工件端口存储计划，并证明注入的状态锁租约已释放。该端口没有Apply操作。

真实提供商 SDK 适配器、凭证解析、连接和 DeploymentStack 持久性、清单 API、放置策略、生产远程状态和分布式锁定、加密对象存储、沙盒工作线程部署、Terraform CLI 适配器和 Apply 仍在计划中。 Discovery 和 Plan 都还没有 HTTP 路线。遵循[添加提供商适配器](/aethercloud/guides/add-provider-adapter)进行发现，并[安全地规划基础设施](/aethercloud/guides/plan-infrastructure)进行引擎工作，而不是发明新的边界。

## 特定于提供商的层

每个提供商适配器拥有：

- 身份验证和短期凭据
- 地区、地区、配额、SKU 和价格发现
- 特定于提供者的基础设施模块
- 提供者本机功能扩展
- 从引擎 JSON 和提供者 API 到规范化观察的转换
- 特定于提供者的发现和规范化的一致性测试夹具

命名空间扩展保留不属于通用模型的功能。可移植性意味着意图和策略可以进行比较；这并不意味着将每个提供程序功能隐藏在虚假的通用 API 后面。

## 基础设施执行

OpenTofu 是默认引擎。通过相同的 `InfrastructureEngine` 端口支持 Terraform。实现的应用程序流选择不可变的版本化提供程序模块和拓扑，向引擎请求保存的计划，验证版本化的 JSON 派生更改和状态锁定证据，然后评估策略并记录加密的工件引用。真正的本地 OpenTofu CLI 工作线程已实现；其生产远程状态、凭证、工件存储和工作部署适配器以及每个应用路径均已规划。
```text
DesiredTopology
      ↓
PlacementPolicy
      ↓
Provider Adapter + versioned module
      ↓
OpenTofu/Terraform plan -out
      ↓
show -json → validation, summary, policy
      ↓
encrypted saved Plan receipt

future separate workflow:
approved exact Plan → Apply → refresh → NormalizedResourceProjection
```

基础设施引擎计算依赖性和建议的漂移。 AetherCloud 拥有工作流程、权限、策略、审核和规范化资源图。经政策批准的规划不属于批准凭证，不能通过当前口岸申请。直接提供程序SDK补充了发现、定价、配额、临时凭证和不属于基础设施生命周期变化的操作的引擎。

## 状态隔离

一个状态属于一个部署堆栈。堆栈只属于一个租户、提供商连接、帐户或订阅以及主要区域。跨区域模块可能存在于提供商支持的地方，但一个状态永远不会跨越多个提供商连接，也永远不是租户全局的。

状态使用具有锁定、加密、版本控制、备份和保留功能的远程后端。实施的计划合约需要精确的状态密钥关联以及获取和释放的锁定证据。持久沿袭、序列、提供商版本、备份和保留仍在计划中。工作进程保持无状态，并且永远不会将本地工作区视为持久存储。

## 凭据

云连接包含元数据和秘密引用，而不是纯文本凭据。工作进程更喜欢工作负载身份或短期凭证，仅为工作接收它们，通过提供商支持的环境或凭证机制注入它们，然后销毁工作空间。凭证不得出现在生成的模块、保存的计划、日志、提示或标准化资源记录中。

## 多云操作

融合发生在各个状态文件之上：

- 跨提供商的库存和搜索
- 跨成本、区域、主权、能力和配额的策略和布局
- 使用多个独立可审核的协调部署作业
- 跨边缘站点和云目的地的数据路由意图
- 规范化运行状况和漂移视图，并提供提供者本机详细信息

跨云部署是提供者范围内的作业的传奇，而不是分布式事务。部分成功仍然可见，补偿也很明确。

## 控制平面数据库可移植性

`managed-postgresql`是可移植的提供商功能，而不是固定的供应商产品。未来的提供商配置文件将保留本机引擎变体、支持的版本和扩展、HA、备份、加密、专用连接、复制、区域、主权、RPO/RTO、价格证据和命名空间扩展。核心持久性代码仍然是一个 PostgreSQL 适配器，并且绝不会在提供程序枚举上分支。

每个 AetherCloud 控制平面单元都有一个权威的 PostgreSQL 编写器拓扑。跨多个提供商管理资源不需要同步跨云数据库写入。租户主单元放置、副本升级、灾难恢复和迁移是显式管理的工作流程。请参阅[PostgreSQL持久性和多云单元](/aethercloud/concepts/persistence-and-multi-cloud-cells) 和 [ADR-0013](https://github.com/EvanL1/AetherCloud/blob/main/docs/adr/0013-postgresql-control-plane-persistence.md)。

## HPC-NOW 中保留的经验教训

HPC-NOW 表明，有用的公共生命周期可以位于单独的提供程序 IaC 模板之上，并且可以针对统一的多集群视图规范化提供程序状态。 AetherCloud 保留了这些想法，同时用注册表替换硬编码的供应商分支，需要显式提供程序身份，解析版本化 JSON 而不是原始状态文本，以及将机密与模块分开。
