---
title: "网关身份和注册"
description: "通过注册和短期引导声明构建租户范围的网关身份"
updated: 2026-07-16
status: mixed
---

# 网关身份和注册

> 权威来源：[AetherCloud](https://github.com/EvanL1/AetherCloud/blob/main/docs/concepts/gateway-identity-and-enrollment.md)。此页面已镜像到统一的 AetherIoT 文档中。

网关是一个 AetherEdge 运行时的 AetherCloud 租户范围身份。它不是设备会话、实时状态所有者或证书记录。注册逐步将注册的云资源绑定到运行时持有的证明。

## 已实现的基础

当前的 TypeScript 基础实现：

- 运行时验证、品牌 `TenantId`、`ProjectId` 和 `GatewayId` 值
- 不可变网关注册转换`registered -> awaiting-claim -> claimed`
- 单独的 `RegisterGateway`、`IssueGatewayEnrollment` 和 `ClaimGatewayEnrollment` 应用程序命令
- 租户范围的 `GetGatewayEnrollment` 查询
- 包含权限、风险、确认、幂等性、到期、审核和授权策略的命令定义
- 类型化的应用程序故障和运行时解码不受信任的输入
- 乐观的`GatewayIdentityRepository`插入和替换操作
- 携带经过身份验证的参与者、命令治理和集成事件身份的突变证据
- 读取和写入路径的类型化存储不可用结果
- 用于一致性和本地测试的内存仓库和令牌服务
- PostgreSQL仓库、显式迁移、租户 RLS、节点 `pg` 池边界、乐观修订检查和原子网关/审核/发件箱
- 使用受限应用程序角色写入选择加入 PostgreSQL 18 集成测试

这些是应用程序和适配器合约。未实施队列 HTTP 路由、CloudLink 消息、生产数据库组合/迁移运行程序、CA 或 KMS 集成。 SQL 适配器可执行但未部署。

## 命令边界

| 功能 | 授权 | 风险 | 确认 | 租户中的幂等性和到期 |
| -------------------------------- | -------------------------------------------------- | ------ | ------------ | ---------------------- |
| `fleet.gateway.register` | `fleet.gateway.create`租户中的上下文 | 低 | 不需要 | 必需 |
| `fleet.gateway.enrollment.issue` | `fleet.gateway.enrollment.issue`上下文 | 高 | 显式 | 必需 |
| `fleet.gateway.enrollment.claim` | 绑定注册令牌 | 中 | 不需要 | 必需 |

所有三个命令都需要按策略审核。在经过身份验证的租户上下文、事务持久性和持久审核可用之前，它们不会通过 HTTP 组合根公开。

issue 命令返回原始注册令牌一次。相同的重试仅返回公共网关和声明状态。使用相同幂等键和不同输入重试失败。网关聚合存储令牌摘要，而不是原始令牌。

声明命令通过应用程序拥有的令牌服务比较令牌材料。它绑定凭证请求指纹，但不颁发或激活证书。具有相同请求和指纹的有效重复索赔是重播；不同的声明者无法重新绑定网关。

## 查询边界

`fleet.gateway.enrollment.get` 需要 `fleet.gateway.enrollment.read`。该查询使用经过身份验证的上下文中的租户和项目范围，并返回该范围之外的 `gateway-not-found`。它从不返回令牌、令牌摘要或命令幂等密钥。

## 实施失败

- `invalid-input`
- `permission-denied`
- `confirmation-required`
- `command-expired`
- `idempotency-conflict`
- `gateway-already-exists`
- `gateway-not-found`
- `gateway-storage-unavailable`
- `invalid-enrollment-token`
- `enrollment-claim-expired`
- `invalid-gateway-enrollment-transition`
- `concurrent-modification`

运输错误信封仍在计划中。这些代码当前是库结果，而不是已发布的 HTTP 状态映射。

## 计划生命周期

生产生命周期在不改变其权限的情况下扩展了基础：
```text
registered -> awaiting-claim -> claimed -> credential-pending -> active
                    |                              |             |
                 expired                        failed       suspended
                                                                  |
                                                               revoked
                                                                  |
                                                        recovery-pending
                                                                  |
                                                   active (new generation)
```

索赔是短暂的并且可以更换；活动凭证是单独版本的。撤销将永久隔离凭证生成。恢复需要显式授权并创建新一代，而不是重新激活已撤销的材料。

阅读 [ADR-0005](https://github.com/EvanL1/AetherCloud/blob/main/docs/adr/0005-gateway-identity-and-enrollment.md) 了解决策，阅读 [ADR-0013](https://github.com/EvanL1/AetherCloud/blob/main/docs/adr/0013-postgresql-control-plane-persistence.md) 了解其持久事务边界。 [物联网云路线图](/aethercloud/guides/iot-cloud-roadmap) 定义了剩余的交付门。
