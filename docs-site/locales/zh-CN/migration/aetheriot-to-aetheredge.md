---
title: "从 AetherIot 迁移到 AetherEdge"
description: "边缘产品和仓库从 AetherIot 重命名为 AetherEdge。 AetherIoT 成为 AetherEdge、AetherCloud 和...的总括项目名称"
updated: 2026-07-16
---

# 从 AetherIot 迁移到 AetherEdge

边缘产品和仓库已从 AetherIot 重命名为 AetherEdge。 AetherIoT 成为 AetherEdge、AetherCloud 和 AetherContracts 的总括项目名称。这是产品身份更改，而不是协议或包命名空间重写。

## 更改内容

- 仓库 URL：`https://github.com/EvanL1/AetherEdge`。
- 源签出目录和新源存档前缀：`AetherEdge`。
- 面向产品的文档、徽章、发布链接、CI 示例和网站导航使用AetherEdge。
- AetherCloud 和未来的 AetherContracts 文档将边缘产品称为 AetherEdge。

## 保持稳定的内容

- `aether`、CLI 和 `aether-*` 二进制文件。
- Rust 箱和导入名称，包括`aether-edge-sdk` 和 `aether_sdk`。
- 配置密钥、环境变量、服务标识和磁盘路径，除非单独的兼容性决策更改了它们。
- 安装程序名称 `AetherEdge-<arch>-<version>.run`。
- CloudLink、Thing Model、结构定义、TCK 和失败代码标识符。
- 已发布标签、发布资产、证明和摘要固定的 AetherContracts alpha.3 工件。

## 更新现有克隆

GitHub 仓库重命名后：
```bash
git remote set-url origin https://github.com/EvanL1/AetherEdge.git
git remote -v
```

现有的 GitHub 重定向只是过渡辅助工具，而不是永久配置。将 Git 依赖项、子模块、徽章、发布自动化、证明命令和白名单更新为新 URL。

## 维护者部署清单

1. 发布产品系列概述、统一文档结构、ADR、兼容性矩阵、状态页面和本迁移指南。
2. 更新面向产品的引用，同时排除不可变的版本、证据、出处记录和摘要固定契约导入。
3. 验证 AetherEdge 文档和发布工作流程、AetherCloud 文档、网站和 AetherContracts 迁移通知。
4. 在 GitHub 上将 `EvanL1/AetherIot` 重命名为 `EvanL1/AetherEdge`。
5. 更新本地远程、仓库描述、网站链接、默认分支引用、发布徽章和证明示例。
6. 通过新地址重新运行检查并检查打开的拉取请求和发布链接。
7. 宣布兼容性窗口并保留旧名称说明，直到下游引用不再依赖重定向。

## 回滚

如果版本、CI、程序包使用方或文档部署无法解析新的仓库标识，请暂停推出并恢复以前的 GitHub 仓库名称。保留 AetherIoT 产品系列文档和稳定软件标识符；仅恢复面向仓库的 URL 和源存档示例。切勿重写已发布的工件来模拟回滚。
