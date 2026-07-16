---
title: "安全规划基础设施"
description: "运行已实施的受管理 OpenTofu 计划工作线程，而不会暗示基础设施发生变化或暴露敏感引擎输出"
updated: 2026-07-14
status: implemented
---

# 安全规划基础设施

> 权威来源：[AetherCloud](https://github.com/EvanL1/AetherCloud/blob/main/docs/guides/plan-infrastructure.md)。此页面镜像到统一的 AetherIoT 文档中。

本指南描述了 AetherCloud 实施的基础设施规划边界。它为一个部署堆栈生成一份不可变的、经过策略评估的收据。当前 `InfrastructureEngine` 端口中没有 Apply 操作，因此成功的或策略批准的计划无法改变提供商基础设施。

## 已实现的合约

`PlanDeploymentStack` 是传输中立的应用程序命令。它：

1. 运行时解码租户、项目、堆栈、引擎、幂等性、到期、模块、拓扑和内容摘要；
2. 在解析堆栈或调用引擎之前需要 `infrastructure.stack.plan` 权限；
3. 重放相同范围的幂等性请求并拒绝冲突的重用；
4. 准确解析来自动态 `InfrastructureEngineRegistry` 的一个 `opentofu` 或 `terraform` 实现；
5. 向 `InfrastructureEngine.plan` 提供一个生成的计划标识、一个部署堆栈以及不可变的模块和拓扑选择；
6. 除非结果针对精确的堆栈和状态，证明状态锁已获取并释放，并通过以下方式引用加密的敏感计划工件，否则关闭失败摘要；
7. 总结存储和策略的创建、更新、删除、替换和读取更改，而不在计划收据中返回提供商资源详细信息；
8. 将策略决策记录为 `policy-approved` 或 `policy-rejected` 并在 `not-requested` 保持批准。

计划仓库密钥是租户 + 项目 + 幂等性请求。一个租户中的请求无法检索另一租户的收据。
```text
authorized Plan request
        ↓
tenant/project Stack lookup → one remote State key
        ↓
InfrastructureEngine.plan → encrypted saved-Plan reference + lock evidence
        ↓
runtime contract checks → change summary → policy
        ↓
idempotent Plan receipt (never Apply)
```

## 敏感计划数据

将保存的计划二进制文件和原始版本化 JSON 视为秘密数据。 [OpenTofu `show`](https://opentofu.org/docs/v1.10/cli/commands/show/) 和 [Terraform `show`](https://developer.hashicorp.com/terraform/cli/commands/show) 都会警告敏感值可能出现在纯文本 JSON 输出中。原始计划 JSON 很敏感：切勿将其放入日志、审核负载、HTTP 响应、测试快照、提示或代理上下文中。

应用程序仅接收策略所需的经过验证的更改列表，然后保留摘要以及加密工件引用和 SHA-256 摘要。实际的 CLI 工作线程必须将保存的计划和 JSON 写入隔离的工作区，加密持久工件，并在上传后销毁其工作区。它绝不能将任何一个文件提交到源代码管理。

## 已实现的 OpenTofu 工作线程

`OpenTofuInfrastructureEngine` 是真正的仅计划 CLI 适配器。其异步工厂执行 `tofu version -json` 并从该响应中派生描述符版本，而不是硬编码版本。对于每个计划，它使用 `NodeOpenTofuProcessRunner` 通过 `shell: false` 将可执行文件和 argv 直接传递给 `child_process.spawn`：
```text
tofu init -input=false -no-color
tofu validate -json
tofu plan -json -input=false -lock=true -lock-timeout=… \
  -detailed-exitcode -no-color -out=<saved-plan>
tofu show -json <saved-plan>
```

计划退出代码 0 表示成功的空 diff，2 表示成功的非空 diff，所有其他退出都是类型化失败。版本化的 `show` JSON 将创建、更新、删除、读取、无操作以及两个替换操作订单映射到应用程序合约中。不受支持的 JSON 主要版本和未知操作无法关闭。

模块配置和拓扑变量通过注入的工件解析器输入。工作器在将 JSON 写入 `main.tf.json` 和 `aether.auto.tfvars.json` 之前验证每个选定的 SHA-256 摘要。注入的锁管理器必须返回准确的堆栈状态密钥的真实租约，并且只有在该租约报告成功释放后，结果才是成功的。 OpenTofu 也始终接收 `-lock=true`。

节点工作区工厂创建一个新的 0700 目录，并将源代码和保存的计划文件写入为 0600。它拒绝符号链接和过大的保存计划，限制捕获的子进程输出，处理截止日期和取消，并在成功或失败时删除整个工作区。子进程仅接收显式提供的环境；观察者事件仅包含阶段、结果、退出状态、持续时间和相关标识。

## 适配器一致性和测试

每个基础设施引擎适配器必须从 `@aether-cloud/infrastructure-conformance` 运行 `infrastructureEngineConformance`。共享套件验证 Plan、Stack 和 State 相关性；加密的敏感工件元数据；锁定证据； JSON格式版本；并且缺少 `apply`。

`MemoryInfrastructureEngine`、`InMemoryInfrastructurePlanRepository` 以及 OpenTofu 适配器周围的假边界使默认测试路径不包含 OpenTofu 二进制文件、网络、远程状态、机密提供程序和云帐户。真正的无提供商本地 CLI 集成是明确选择加入的：
```bash
pnpm test:opentofu-integration
```

它使用已安装的 OpenTofu 二进制文件、临时本地工作区、内存中锁租约和 AES-GCM 测试存储。它证明了真实的命令序列和清理，而无需声明生产状态或存储。

## 仍在计划中

生产远程状态集成、分布式锁实现、短期凭证解析、生产加密工件存储、持久计划仓库、审计接收器、成本模型、沙盒工作线程部署、Terraform CLI 适配器和公共 HTTP 端点仍在计划中。实现的节点适配器是一个真正的本地OpenTofu进程，但它不是真正的云帐户或生产远程状态的证明。

应用、刷新突变、导入和销毁仍然完全未实现，未来的命令具有自己的许可、确认、批准、审计和恢复合约。请勿将它们添加为 `PlanDeploymentStack` 上的模式或标志。

在扩展此工作流程之前，请阅读[多云融合](/aethercloud/concepts/multi-cloud-fusion)以了解提供商和州边界以及[AI 不变量](https://github.com/EvanL1/AetherCloud/blob/main/ai/invariants.md)。
