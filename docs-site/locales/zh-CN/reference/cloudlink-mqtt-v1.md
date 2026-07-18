---
title: "实验性 CloudLink MQTT v1 边缘合约"
description: "本文档描述了实验性 CloudLink 候选者。公开AetherContracts`v0.1.0-alpha.3`发布是唯一的权威，并且AetherEdge..."
updated: 2026-07-17
---

# 实验性 CloudLink MQTT v1 边缘合约

## 状态和范围

本文档描述实验性 CloudLink 候选方案。公共 AetherContracts `v0.1.0-alpha.3` 版本是唯一协议权威，AetherEdge 和 AetherCloud 使用相同的摘要固定使用方锁。当前证据覆盖完整分发、全部测试夹具和可选的边缘与云双端 alpha 测试，但尚不能证明生产级互操作性。本页只说明 AetherEdge 的接入方式，共享协议和兼容性以 AetherContracts 发布内容为准。

锁导入了精确的 alpha.3 采用闭包。严格运行时清单 SemVer 和重播/摘要/光标上下文向量在这两种产品中执行。本地线路、身份验证、门、场景和夹具清单文件是迁移或产品集成材料，不能覆盖 AetherContracts。

由此片实现：

- 严格的传输中立 JSON 值和规范业务摘要；
- 会话/版本/纪元验证；
- 运行时清单和真实的 `PointSample` 业务映射；
- 仅通过应用程序 ACK 删除内存和可崩溃恢复的文件持久队列；
- MQTT v3.1.1/QoS 1 绑定用户选择的代理；
- 确定性假传输测试、仅限 Edge 的代理工具以及选择加入的真实 Mosquitto AetherEdge/AetherCloud 双

存在于导入的实验子集中：

- 每个线域、主题、收据和重播/数据丢失交换；
- 确切的 alpha.3 挑战、网关签名和可信连接器原始提案形状；生产密钥生命周期仍未解决；
- 边缘原生遥测，无需强制 Thing Model 修订。

不兼容的仓库本地 AetherEdge 和 AetherCloud 词汇表已被公共 alpha.3 取代；迁移结果保留在 `contracts/cloudlink/v1/MIGRATION.md` 中。

在此部分之外计划：

- 生产 AetherCloud 身份验证和持久存储；
- 生产注册/CA/KMS 生命周期；
- MQTT 5 项增强功能和私有代理桥/站点连接器；
- 联合发布的 ACL 模板和生产启用；
- 专用 CloudLink 流上的警报和操作遥测；
- 在云接收之前过期的记录的显式过期到数据丢失生命周期；过期内容已关闭失败，并且永远不会提供。

已弃用但保留：

- 未版本化的 `property/{productSN}/{deviceSN}` 和相关的旧命名空间；
- 在通过通用发件箱接受 MQTT 客户端后删除；
- 旧 MQTT 设备控制主题。

CloudLink v1 不公开任何物理控制或任意 RPC。

## 实验性集成扩展

`aether.cloudlink.integration.v1alpha1` 用于通过 CloudLink 传送集成拓扑快照和观测批次。
该扩展仍处于实验阶段，默认关闭，并采用“先云后边”的启用顺序：AetherCloud 必须先明确启用
兼容的接收方，AetherEdge 当前运行时清单也必须明确声明完全一致的扩展标识。缺少任一条件时，
边缘端都会拒绝启用。仅启用数据源并不会自动开启云端发布。

该扩展使用两条相互独立的持久流和消息主题：

- 拓扑快照使用 `up/integration/topology`；
- 观测批次使用 `up/integration/observations`。

每份拓扑快照都是一条不可拆分的原子记录；完整消息超过 256 KiB 时会拒绝发布。观测批次在
大小允许时保持不变，超限时只能在两条完整观测之间拆分，绝不拆开单条观测。拆分后的每个批次
都必须能够独立验证，并且包含 CloudLink 交付信封在内的每条完整 MQTT 消息负载都不得超过
256 KiB。

两条流共同复用既有的应用级持久确认、重放、稳定交付身份和摘要规则。消息代理的传输确认不能
清除持久记录。该扩展只传送只读的集成投影事实，不提供命令、任意远程调用或物理设备控制能力。

## 兼容性矩阵

| 此切片之前的关注 | AetherEdge | AetherCloud 参考 | 候选解析 |
|---|---|---|---|
| 线格式包 | 未版本化的旧版 JSON | 严格的 TypeScript 编解码器和入口 | 字节一致的 alpha.3 结构定义、夹具以及匹配的 Rust／TypeScript 词汇 |
| 交付删除 | 本地 `AsyncClient::publish` 接受后 | 需要持久应用确认；当前仅有内存基础 | 专用持久队列仅在验证持久 ACK 后删除 |
| 会话 | 无 CloudLink 会话/纪元 | 域/应用程序内存实现 | 候选 hello/accepted 和单调纪元绑定 |
| 身份验证 | MQTT用户名/密码或 mTLS；产品/设备主题身份 | 会话验证程序消耗 alpha 结构证据 | Hello 携带确切的质询和网关签名对象，或者在有效负载之外需要可信适配器元数据；生产密钥生命周期仍然建议 |
| 恢复 | 仅代理重新连接 | 服务器光标作为权威 | 服务器光标驱动稳定的身份/摘要重播 |
| 遥测身份 | 旧逻辑映射丢失时间戳/质量/地址 | 云编解码器现在接受边缘字段和可选模型 | 保留边缘`PointAddress`、时间戳、质量和批次位置，而无需构建模型；云多样本内部索引保持开放 |
| 拓扑 | 一致的SHM发布纪元和快照摘要 | 无等效批次字段 | 每批次携带发布纪元和拓扑摘要 |
| Manifest | 封闭v1，JCS SHA-256，已实施 | 匹配运行时清单域形状 | 嵌入精确验证的清单和校验和 |
| 代理 | 可配置的旧端点 | Alpha MQTT入口实现 | 用户选择的共享MQTT v3.1.1代理由双线束执行；生产身份验证仍然受阻 |
| 控制 | 旧版写入/调用主题到达受管理的应用程序边界 | CloudLink 单独计划的命令 | 无 CloudLink v1 命令主题或负载 |

## 主题策略

所有候选发布和订阅均使用 QoS 1 和 `retain = false`。主题前缀可以配置；前缀和网关段拒绝空值、`+`、`#`、NUL、控制字符和路径遍历式空段。网关只接收自己的 `down/session`、`down/ack` 和 `down/replay` 主题。

网关 ID 不是授权证据。云入口必须将其与经过验证的发布者身份进行比较。使用通用共享代理，单独的云订阅者通常无法看到原始发布者经过身份验证的代理主体。 Alpha.3 冻结实验质询/会话/上行链路签名对象。生产通用代理模式仍然需要密钥配置、轮换、撤销和验证者所有权。备用源模型在每次发布的有效负载外部配置可信适配器证据。

## 时间、整数、边界和摘要

- 协议 `uint64` 值是规范的十进制字符串：`0` 或非零数字后跟数字，没有符号、空格、指数或前导零。
- 协议时间戳是 Unix编码为规范十进制字符串的毫秒。嵌入式运行时清单保留其现有字段格式不变。
- 一条编码消息最多为 256 KiB，一个点批次最多包含 256 个样本。
- 标识符和元数据有长度限制；未知对象字段无法关闭。
- 交付摘要仅通过版本化业务内容的 RFC 8785 规范 JSON 进行 `sha256:<64 lowercase hex>`。会话数据、跟踪上下文、重试计数、MQTT 数据包标识符/属性和传输时间戳均被排除。

具有相同稳定 `batch_id` 和摘要绑定的相等 `(gateway_id, stream_id, stream_epoch, position)` 是重播。更改该身份的任一绑定都会造成安全冲突。

## 持久队列和恢复行为

持久队列会保留流纪元、下一个位置、记录、传送状态、最后一个持久 ACK 和数据丢失证据。客户或经纪人交付无法删除记录。持久 ACK 验证以下所有内容：

- 当前会话 ID 和会话纪元；
- 流 ID 和流纪元；
- 连续确认位置；
- 终端批次身份和摘要；
- 非空接收身份。

重复 ACK 返回先前的幂等结果。较旧的会话、超过间隙的头寸、错误的批次/摘要值以及冲突无法关闭。重新连接在新的会话信封下提供相同的存储内容；它从不分配其他位置、批次 ID 或摘要。

配置的容量限制为 1–65,536 条保留记录；每个记录有效负载独立限制为 256 KiB。

在达到容量时，适配器会记录准确的逐出位置范围。如果云的权威光标请求不可用的位置，则边缘会发送该证据，并仅在云处理间隙后从最早保留的记录恢复。损坏的最终日志记录在恢复期间会被截断。任何完整日志突变的损坏都会阻止打开线轴。文件适配器使用有效负载一次增量突变，并在 256 次突变后接受工作之前以原子方式压缩实时记录和游标元数据。

## 遥测映射

| 边缘值 | 候选字段 | 注释 |
|---|---|---|
| `PointAddress::instance_id` | `instance_id` | 规范十进制标识 |
| `PointAddress::kind` | `point_kind` | `telemetry`、`status`、 `command` 或 `action`；业务集合仅使用收购拥有的类型 |
| `PointAddress::point_id` | `point_id` | 规范十进制标识 |
| `PointSample::value` | `value` | 必须是有限 |
| `PointSample::timestamp` | `source_timestamp_ms` | 源 Unix 毫秒 |
| `PointSample::quality` | `quality` | 由合约保留 |
| SHM 发布epoch | `topology.publication_epoch` | 相干点/运行状况生成见证 |
| 拓扑快照摘要 | `topology.snapshot_digest` | 标识确切的已发布路由快照 |

当前 SHM 插槽不会对采集质量进行编码，因此其读取适配器报告接受有限值 `good`。这是一个实现限制，并不能证明源提供的 `good` 质量。

没有伪造 Thing Model 修订版。仅当可选的 `model` 绑定源自委托的、经过验证的配置时，才会接受该绑定。云丰富可以在摄取后映射边缘点地址。

## 运行时清单映射

报告嵌入当前运行时清单生成器的确切结果。它的 `checksum.algorithm` 仍然是 `sha256`；其摘要是根据除 `checksum` 之外的每个清单字段的 RFC 8785 规范 JSON 计算的。 CloudLink 验证并传输该校验和，而不是发明另一个组合模型。

## 共享代理利用

除非明确启用，否则仅边缘集成将被禁用。它需要 MQTT v3.1.1 代理并故意使用假的云对等点，因此它不是联合互操作证据：
```bash
AETHER_CLOUDLINK_RUN_INTEGRATION=1 \
AETHER_CLOUDLINK_BROKER_HOST=127.0.0.1 \
AETHER_CLOUDLINK_BROKER_PORT=1883 \
cargo test -p aether-cloudlink-mqtt --test shared_broker -- --nocapture
```

可选凭据使用 `AETHER_CLOUDLINK_BROKER_USERNAME` 和 `AETHER_CLOUDLINK_BROKER_PASSWORD`。 TLS 使用 `AETHER_CLOUDLINK_BROKER_CA`，以及可选的客户端证书/密钥变量。测试和错误永远不会打印凭据值。

最终的 alpha 证据使用真实的 Mosquitto、此仓库的 rumqttc 传输和 `FileCloudLinkSpool`，以及 AetherCloud 的真实 MQTT 适配器和应用程序用例。从 AetherCloud 运行它：
```bash
pnpm test:cloudlink-dual
```

它在 `artifacts/cloudlink-alpha/evidence.json` 下写入 `AetherCloud/evidence/cloudlink-alpha3-dual-harness.json` 和兼容性副本，包括故障矩阵。该工具可实现代理重新连接、ACK 丢失、恢复文件持久队列的第二个边缘进程、云拥有的 `manifest/1/1` 恢复游标、第二个云入口进程、重复/幂等重播、冲突、过期、乱序、非持久部分结果和显式数据丢失。遥测在丢失 ACK 后仍会重播。 PostgreSQL 进程崩溃持久性仍处于阻塞状态，云重启结果确实是 `unknown-reaccepted`。
