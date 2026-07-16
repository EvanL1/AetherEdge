---
title: "产品版本兼容性"
description: "该矩阵将已发布的兼容性证据与计划的产品组合区分开来。绿色本地测试永远不会升级实验公共……"
updated: 2026-07-16
---

# 产品版本兼容性

此矩阵将已发布的兼容性证据与计划的产品组合区分开来。绿色本地测试永远不会将实验性公共合约升级到生产状态。

## 当前测试基线

| AetherEdge | AetherContracts | AetherCloud | 状态 | 证据 |
| --- | --- | --- | --- | --- |
| `v0.5.0`加上alpha.3 使用方变更 | `v0.1.0-alpha.3` | 未发布的 alpha.3 使用方变更 | 实验性集成基线 | 相同的完整使用方锁定、53 个精确导入、无待处理导入和 25 个共享固定结果 |
| `v0.5.0` 旧版 MQTT路径 | 旧版线路不需要 | 现有旧版摄取 | 兼容性默认值 | 现有产品行为； CloudLink 不会默默地重新解释旧主题 |
| 未来 AetherEdge 版本 | 未来生产契约版本 | 未来生产 CloudLink 版本 | 计划 | 需要联合身份验证、签名确认、崩溃持久性、一致性、回滚和已过的支持窗口证据 |

第一行是分布和固定证据。它不是生产传输、身份验证、状态机或持久性一致性。

## 命名兼容性

| Surface | 迁移行为 |
| --- | --- |
| GitHub 仓库 | `EvanL1/AetherIot` 变为 `EvanL1/AetherEdge`；即使 GitHub 重定向旧 URL，消费者也应更新遥控器 |
| Rust SDK 软件包 | `aether-edge-sdk` 保持不变 |
| Rust 导入 | `aether_sdk` 保持不变 |
| CLI 和服务二进制文件 | `aether` 和`aether-*` 保持不变 |
| 安装程序 | `AetherEdge-<arch>-<version>.run` 保持不变 |
| 配置和环境密钥 | 现有 `aether` 和 `AETHER_*` 标识符保持不变 |
| CloudLink 和契约标识符 | 保留不变 |
| AetherContracts alpha.3 工件 | 保持逐字节不可变，并可能保留历史 AetherIot 名称 |

## 发布规则

每个未来的产品版本都应发布一个兼容性行，其中固定确切的版本或提交并链接到其可执行证据。浮动分支、`latest` 和隐含的兼容性不是公认的证据。
