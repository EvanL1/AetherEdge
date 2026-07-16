---
title: "使用 AI 代理进行构建"
description: "使用仓库合约和验证命令，而无需发明不受支持的 API"
updated: 2026-07-14
status: implemented
---

# 使用 AI 代理进行构建

> 权威来源：[AetherCloud](https://github.com/EvanL1/AetherCloud/blob/main/docs/guides/build-with-an-agent.md)。此页面已镜像到统一的 AetherIoT 文档中。

AetherCloud 将架构、产品语义和实现的表面记录为版本化仓库资产，因此编码代理可以进行小幅更改，而无需从框架代码重建意图。

## 提供代理上下文

将代理指向仓库根并要求其读取， order：

1. `AGENTS.md` 用于强制边界和验证
2. `skills/aether-cloud/SKILL.md` 用于任务路由
3. 从 `llms.txt`
4. 最近的软件包 README 或合约中选择的页面以获取实现详细信息

在更改应用程序端口或创建SDK 代码。

提示示例：
```text
Read AGENTS.md and the aether-cloud skill. Add a read-only provider capability
query using the existing application boundary. Keep provider SDK types outside
the application package, update the HTTP reference, and run pnpm check.
```

## 需要证据

代理应确定：

- 正在更改的事实来源
- 应用程序用例和权限边界
- 契约是否已实施或计划
- 意图是否与提供者中立或需要命名空间提供者功能
- 基础设施的提供者、部署堆栈、状态和凭证边界工作
- 在实现之前失败的狭隘行为测试
- 随后通过的验证命令

拒绝从路由处理程序写入适配器的生成代码、将云遥测视为实时权限、忽略租户上下文或发明版本化合约包中不存在的 CloudLink 消息。

还拒绝核心包中的固定供应商切换、提供商推断凭证内容、跨提供商状态、解析人工 IaC 输出或缺乏已保存计划和批准证据的基础设施应用。

## 文档契约

每个索引页面都有包含 `title`、`description`、`updated` 和 `status` 的 frontmatter。相同的标题、描述和状态出现在 `ai/docs-manifest.json` 中。状态为 `implemented`、`planned`、`mixed`、`normative` 或 `deprecated` 之一。仓库测试检查清单、本地链接、所需的入口点和未完成的占位符。

当行为发生变化时，更新代码、测试、狭窄的参考页面以及清单描述（如果其路由含义发生变化）。架构权限或依赖项更改也需要 ADR。

## 当前验证
```bash
pnpm check
```

这无需外部服务即可运行文档契约、单元和集成测试、类型检查、linting 和格式检查。

## 代理功能

仓库基础里程碑为编码代理提供了文档。在身份、租户授权和底层查询用例存在后，稍后规划远程 MCP 资源和操作工具。仓库文档不得指示代理在发布之前调用这些工具。
