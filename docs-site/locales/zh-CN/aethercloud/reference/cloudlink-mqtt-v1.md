---
title: "预发布 CloudLink MQTT v1"
description: "查看 alpha.3 代理证据和 PostgreSQL 遥测 ACK 切片，而不夸大完整的 CloudLink 身份验证或崩溃持久性"
updated: 2026-07-16
status: mixed
---

# 预发布 CloudLink MQTT v1

> 权威来源：[AetherCloud](https://github.com/EvanL1/AetherCloud/blob/main/docs/reference/cloudlink-mqtt-v1.md)。此页面镜像到统一的 AetherIoT 文档中。

这是一个实验性的预发布互操作性候选者。公共 AetherContracts `v0.1.0-alpha.3` 版本是唯一的合约授权机构，AetherCloud 和 AetherEdge 提交相同的摘要固定消费者锁。当前的声明是完全的分发采用和公共测试夹具执行，而不是生产协议一致性。 Shared-Broker 生产密钥生命周期仍未解决，并且 alpha.3 持久 ACK 未明确签名。

## 公开发布消费

离线消费者检查固定带注释的标签对象、提交、包大小和 SHA-256、发布清单 SHA-256 以及每个采用的目标字节。它导入精确的 alpha.3 采用闭包，包括规范规范、配置文件、门、故障分类、TCK 清单、Schema、测试夹具和验证器闭包。 `pending_imports` 为空。

本地 `contract-manifest.json`、`wire-profile.json`、身份验证 Schema、gate 文件和场景是集成历史记录或 AetherCloud 提案。它们不会覆盖 AetherContracts。分发完整性本身并不是编解码器或行为一致性。完整的公共设施和选择加入的双安全带现在提供了阿尔法证据，而身份验证和生产耐用商店的大门仍然开放。默认检查是离线的；选择加入 CI 检查仅下载确切的摘要固定版本。

## 已实现的云表面

- `adapters/cloudlink/mqtt` 严格解码固定的公共 Snake_case 会话、心跳、运行时清单、遥测和数据丢失上行链路，并对会话、心跳/持久 ACK 和重播下行链路进行编码。
- 编解码器验证主题/主体网关绑定、UUID 身份、规范整数/时间、JCS 业务摘要、运行时清单校验和、点类型、拓扑证据、限制和未知字段。
- MQTT.js 接受可配置端点、代理凭据、严格 CA/客户端证书/私钥文件 mTLS、MQTT 3.1.1 或 MQTT 5、QoS 1，以及`retain=false`；正确性仅使用 3.1.1 基线。 TLS 文件必须是有界的常规非符号链接文件，并且私钥必须拒绝组/其他访问。
- `apps/cloudlink` 通过现有应用程序用例进行路由，并且在没有应用程序收据的情况下不会生成 ACK。对于遥测，网桥可以使用 `PostgresTelemetryRepository` 提交的精确 ACK 投影；当前的 Broker 线束根仍然使用内存，并且是一致性证据，而不是生产持久性。
- 当前的云遥测模型仅安全地映射单样本线批次，因为它索引记录，而 alpha.3 定位原子批次。在云冻结该内部映射之前，较大的批次会显式失败。还计划了数据丢失持久性； alpha 线束记录了应用事实，但不声明生产耐久性。
- 默认测试不需要 Broker、Docker、PostgreSQL 或云帐户。选择加入的 alpha 工具会启动真正的 Mosquitto、AetherEdge 的 rumqttc 传输和文件持久队列，以及 AetherCloud 的 MQTT 入口和应用程序用例。

## 临时线路边界

规范公共核心位于锁定的 AetherContracts 版本中。本地`contracts/cloudlink/v1/wire-profile.json`记录了早期的云提案和迁移差异；它不是共享权限：

- 封闭的 Snake_case UTF-8 JSON，最大 256 KiB；
- MQTT 3.1.1，QoS 1，非保留消息；
- 规范 uint64 字符串和 Unix 纪元毫秒；
- 小写 UUID 网关/会话身份；
- 每个原子批次一个流位置，最多 256 个点样本；
- 仅在 `{protocol_version,message_kind,payload}` 上使用 RFC 8785 JCS SHA-256；
- 跨重播的稳定纪元、位置、批次 ID 和摘要；
- 绑定到会话、流、纪元、位置、批次、摘要和云的持久 ACK收据；
- 显式重播请求和数据丢失证据；
- 无 RPC、物理控制主题、SHM 写入、点/寄存器写入或伪造的 Thing Model 修订。

遥测保留边缘拥有的 `instance_id`、`point_kind`、`point_id`、值、源时间戳、质量和拓扑证据。 `model` 是可选的，仅在调试建立时才可能出现。

## 会话和共享代理边界

alpha.3 hello 声明 `gateway-signed` 或 `trusted-connector-broker-attestation` 来源。网关签名的 hello 携带准确的质询 ID、网关密钥 ID 和经过结构验证的 Ed25519 签名对象。这证明了冻结的编解码器形状，而不是生产消息来源或生产密钥生命周期。当不同的发布者稍后可以注入共享命名空间时，一次成功的问候是不够的。

因此，通用客户选择的代理模式需要重播限制的云质询和网关签名，然后在每个上行链路上添加会话限制的网关签名。云挑战使用实验性云签名记录； alpha.3 持久 ACK 未签名。另一种方法是由可信适配器在每次发布的有效负载之外提供可信代理证明。负载提供的证明是不可信的，主题身份也是不可信的。生产密钥配置、轮换、撤销和验证者所有权使身份验证门保持在提议状态。

| 模式 | 要求 | 状态 |
| ---------------------------------- | -------------------------------------------------------- | -------------------------------------------------- |
| 可访问的客户选择的代理 | 专用命名空间、TLS、质询/每条消息证明 | 已实现传输适配器；计划的原始身份验证 |
| AetherCloud管理的代理 | 相同的 CloudLink 应用程序/会话边界 | 计划的代理产品/主体集成 |
| 私人客户代理 | 保留身份、持久队列/ACK 和服务的站点连接器审核 | 计划 |
| 旧版 AetherEdge MQTT | 独立命名空间，从不静默解码为 CloudLink | 迁移期间保留 |

MQTT PUBACK 仅是传输证据，绝不会授权从 AetherEdge 中删除线轴。

## 证据和剩余的门

消费者锁定完整的公共清单：所有 25 个公共固定字节在两个产品中执行，并且挂起的导入为空。本地测试夹具/门文件仅是证据覆盖。

| 门 | 状态 |
| --------------------------------------------------------------------- | ------------------------------------------------------------------------------------- |
| 共享代理消息源身份验证 | 进行中 |
| 单个信封/时间/身份/摘要/未签名的ACK配置文件 | 已实现alpha.3；生产身份验证仍处于试验阶段 |
| 跨仓库测试夹具 | 已通过 alpha.3 公共清单 |
| Real-Broker 双边缘/云工具 | 本地 Mosquitto 和 AWS IoT Core alpha 证据；身份验证门保留提案 |
| ACK 丢失、重启、重复、冲突和数据丢失故障注入 | 实施了 alpha 证据；排除进程崩溃持久性 |
| PostgreSQL 持久崩溃 ACK/发件箱 | 遥测接受 PostgreSQL 切片已实施和验证；全门被阻止 |
| 传统切换 | 被阻止，直到每个前面的门都通过 |

遥测接受交易、精确的ACK发件箱和有界交付用例都已实现。完整的耐崩溃 CloudLink 门仍然受阻：生产组合已规划，凭证/会话持久性、数据丢失事实、多样本映射和 alpha.3 生产身份验证仍不完整。旧版仍为默认值。云中断无法更改边缘权限或停止采集、规则、警报、安全联锁或本地控制。此切片中没有物理控制。

## 选择双边缘/云 alpha 控制
```bash
pnpm test:cloudlink-dual
```

该命令选择唯一的主题前缀，启动并重新启动本地 Mosquitto，运行两个产品传输，并将机器可读的证据写入 `evidence/cloudlink-alpha3-dual-harness.json`（以及 `artifacts/cloudlink-alpha/evidence.json` 下的兼容性副本）。它涵盖 ACK 丢失、单独的边缘进程恢复、云拥有的 `manifest/1/1` 恢复游标、单独的云入口进程重新启动、精确重复重播、摘要/批次冲突、显式数据丢失、乱序、过期和非持久的部分应用程序结果。它将云重启连续性记录为未知；它不能证明生产过程的碰撞耐久性。阅读 [ADR-0014](https://github.com/EvanL1/AetherCloud/blob/main/docs/adr/0014-cloudlink-mqtt-transport-binding.md) 和 [ADR-0015](https://github.com/EvanL1/AetherCloud/blob/main/docs/adr/0015-cloudlink-interoperability-release-gates.md) 了解运输决策和命令释放门。 [ADR-0016](https://github.com/EvanL1/AetherCloud/blob/main/docs/adr/0016-pinned-aethercontracts-consumption.md) 定义公共发布权限和固定。

## 选择加入 AWS IoT Core mTLS 工具
```bash
pnpm test:cloudlink-aws-iot
```

此外部服务测试需要经过身份验证且具有 AWS IoT 权限的 AWS CLI 身份。它默认为 `us-west-2`，并且可以使用 `AETHER_CLOUDLINK_AWS_REGION` 显式移动。该命令配置两个独立的 X.509 客户端主体和两个最低权限主题策略，不创建任何事物，通过端口 8883 上的 AWS IoT Core MQTT 3.1.1 运行 AetherCloud MQTT.js 入口和 AetherEdge rumqttc/FileCloudLinkSpool，并将经过清理的证据写入`evidence/cloudlink-aws-iot-us-west-2.json`.

证书和私钥字节保留在 mode-0700 临时目录中；单个文件使用模式 0600，并且不输入 stdout、证据、Git 或应用程序日志。在每个正常的成功或失败路径上，该工具都会分离策略、停用和删除两个证书、删除两个策略并删除临时目录。

AWS 运行涵盖会话建立、心跳、运行时清单、遥测应用程序 ACK、ACK 丢失重播、重复幂等性、绑定冲突、过期、无序交付、不支持的部分批次、显式数据丢失和最终线轴耗尽。它通过托管代理证明了 alpha 传输互操作性。它没有通过生产身份验证门：应用程序尚未使用经过审核的带外 AWS 主体证明或验证计划的每上行链路网关签名。 Alpha.3 ACK 保持未签名状态，云持久性仍保留在进程本地内存中。
