---
title: "数据库备份与恢复"
description: "在不虚构生产备份自动化或跨单元权威的前提下，规划受控的 PostgreSQL 单元恢复"
updated: 2026-07-18
status: planned
---

# 数据库备份与恢复

> 权威来源：[AetherCloud](https://github.com/EvanL1/AetherCloud/blob/main/docs/recovery/database-backup-and-restore.md)。此页面镜像到统一的 AetherIoT 文档中。

本页用于分析一个 AetherCloud PostgreSQL 控制平面单元的恢复。由于 AetherCloud 尚无生产备份或恢复能力，本页只给出安全要求，不提供数据库执行命令。

## 当前已经实现

- 网关、CloudLink 会话、遥测、集成投影和集成控制已经具备部分 PostgreSQL 适配器或迁移基础。
- 相关集成测试覆盖事务、租户行级安全、回滚、不确定提交后的恢复以及部分重启行为。
- 一个独立部署的单元是其所拥有数据的事务权威。

这些事实只能证明适配器行为，不能证明备份已经存在或能够恢复。

## 尚未实现

生产数据库组合、迁移编排、备份调度、加密备份保留、恢复验证、恢复点目标、恢复时间目标、故障转移和受治理租户迁移仍在规划中。`infrastructure.stack.plan` 不能应用基础设施变更，也不能恢复数据库。

## 安全处置

1. 明确具体的租户、单元、提供商连接、数据库版本、结构版本、备份身份、加密权威和恢复点。
2. 在恢复副本取得写入权威之前，必须先隔离原写入方。
3. 先按照已批准的提供商或数据库流程恢复到隔离环境，该流程位于 AetherCloud 之外。
4. 验证迁移、约束、强制行级安全、审计与发件箱连续性、重放身份、游标和待交付工作。
5. 接受新写入前，核对边缘端会话和投影。
6. 只能提升一个写入权威。跨云副本不会因为可访问或更新时间较新就自动成为权威。

## 人工升级

所有恢复、故障转移、时间点回退或可能丢弃已提交证据的操作，都必须由数据库与安全人员批准。如果无法确定旧写入方已经隔离、备份身份尚未验证或租户隔离尚未通过，就不得让恢复后的数据库接受写入。

继续阅读[PostgreSQL 持久化与多云单元](/aethercloud/concepts/persistence-and-multi-cloud-cells)、[术语表](/aethercloud/reference/terminology)和[物联网云路线图](/aethercloud/guides/iot-cloud-roadmap)。
