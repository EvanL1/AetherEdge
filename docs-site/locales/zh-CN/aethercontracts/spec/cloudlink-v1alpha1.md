---
title: "CloudLink v1 alpha 1"
description: "说明 CloudLink alpha.4 开发目标中的线协议候选、认证提案、累计持久确认与显式 Integration 扩展。"
updated: 2026-07-17
id: cloudlink-v1alpha1
status: experimental-auth-proposal
version: 0.1.0-alpha.4
normative: true
---

# CloudLink v1 alpha 1

> 本页帮助中文用户理解协议。带标签版本中的英文规范、JSON Schema、测试夹具和 TCK 才是规范性依据。

AetherContracts 是这一协议范围的唯一互操作权威。产品仓库中的线协议配置档、清单、门槛和证据文件都只是非规范实现叠加层，不能新增或重新定义线字段。历史联合核心来源记录只说明 alpha.2 输入字节来自哪里，不代表产品仓库继续共同拥有协议定义权。

最新发布版本是 `v0.1.0-alpha.3`。本页同时说明尚未发布的 `0.1.0-alpha.4` 开发目标；它仍处于实验阶段，不是生产 CloudLink 切换版本，旧传输仍为默认值。

## 核心范围与 alpha.4 扩展

alpha.3 冻结了封闭核心信封和时间、身份字段，以及会话质询、会话问候、会话接纳、心跳、Runtime Manifest 上报、遥测批次、数据丢失证据、重放请求和未签名的应用持久确认。TypeScript、Rust、C99 与 C++17 执行相同的公开夹具清单和稳定失败字符串。这些夹具范围是实验性的，不是完整生产传输编解码器。

alpha.4 新增一个必须显式启用的 Integration 扩展，但不会把它的消息类型塞进 alpha.3 核心消息结构定义。扩展复用同一个 `aether.cloudlink` 1.0 会话、上行字段、认证投影、业务摘要、持久位置、重放、数据丢失和持久确认语义。扩展入口结构定义引用可复用的 `envelope.schema.json#/$defs/uplinkEnvelope` 结构。

因此，只用封闭核心 `envelope.schema.json` 验证 Integration 消息一定不够，并且应当失败。使用方必须根据消息类型选择对应的扩展入口结构定义。

## 认证提案

认证配置档区分两种来源模型：

- `gateway-signed`：云端发出一次性签名质询；网关对精确会话建立对象签名，并对每条上行消息的精确对象签名。
- `trusted-connector-broker-attestation`：经过配置的可信入口适配器在载荷之外提供来源证据，并绑定实际收到的 MQTT 载荷字节。

载荷不能为自己作证。主题名称、载荷中的身份和 MQTT 凭据本身都不能构成网关认证。

`profiles/cloudlink/v1alpha1/authentication.json` 精确定义 Ed25519、不带填充的 Base64url、RFC 8785 JCS 签名对象、缺失值规则和重放边界。生产密钥签发、轮换、撤销、验证方归属与生产签名验证仍在规划中，因此认证门槛仍是提案，生产切换继续阻塞。

普通日志和公开证据必须排除签名、随机数、凭据标识和原始认证交互记录。

## 累计应用持久确认

alpha.4 的持久确认 JSON 结构明确不带签名。成功表示应用事实和回执已经在发布确认前持久提交；但是 alpha.4 不包含生产存储或发件箱实现，也不声明崩溃持久性。未来的签名确认属于独立命令或配置档，需要云端密钥生命周期和生产重启证据。MQTT PUBACK 永远不是应用层持久回执。

`acknowledged_position = N` 只在一个精确的 `stream_epoch` 内累计。它证明从上一个已确认游标之后到 `N` 的每个位置都已解决。一个位置只有在满足下列条件之一时才算解决：

- 应用事实与重放回执都已持久提交；
- 同一网关、同一流、同一流代次先前已经持久接纳的有效数据丢失声明明确覆盖该位置。

其他流或其他代次的证据不能填补当前空洞。确认中的批次和摘要仍把位置 `N` 绑定到精确持久消息；数据丢失证据只能填补中间空洞。

允许乱序持久化，但未解决的位置会阻止累计游标。云端不能越过空洞确认，否则返回 `ACK_PREFIX_GAP`，并且游标保持不变。缺失消息后来持久提交，或其有效数据丢失证据后来被接纳后，云端重新计算连续前缀并可以继续推进。网关只有在验证当前会话、精确流代次的累计确认后，才可以删除位置 `<= N` 的本地队列内容。

相同重放身份和摘要的重复消息是幂等消息。相同身份携带不同摘要虽然线结构有效，但上下文无效；它以 `DIGEST_CONFLICT` 隔离，并且不能收到成功回执。数据丢失必须以明确证据表示，云端不能伪造样本。

数据丢失证据必须满足：

```text
first_lost_position <= last_lost_position < earliest_retained_position
```

心跳与恢复游标数组对每个 `(stream_id, stream_epoch)` 最多只能有一项。违反这些规则的消息上下文无效，不能改变业务事实，也不能得到成功应用回执。

唯一持久位置身份是：

```text
(gateway_id, stream_id, stream_epoch, position)
```

`batch_id` 与 `digest` 是该位置的稳定绑定字段，不构成另一套身份；修改它们不能绕过冲突检测。业务摘要是对精确对象 `{protocol_version,message_kind,payload}` 执行 RFC 8785 JCS 后计算的小写 SHA-256。摘要用于内容完整性和重放比较，不用于认证发布方。机器可读规则见 `profiles/cloudlink/v1alpha1/core.json`。

## 到期时间、清单摘要与入口结构定义

`expires_at_ms` 可以省略；省略时，该字段不设置消息截止时间。存在时必须大于或等于 `sent_at_ms`，否则上下文失败为 `INVALID_EXPIRY_WINDOW`。

到期检查使用显式规范 `uint64` 值 `evaluation_time_ms`。只有 `evaluation_time_ms < expires_at_ms` 时消息仍有效；二者相等时已经到期，返回 `MESSAGE_EXPIRED`。可移植 TCK 从场景输入取得评估时间，绝不读取环境挂钟。两种到期失败都不能改变业务状态，也不能产生成功应用回执。

嵌入式 Runtime Manifest 校验值是对完整清单对象去掉顶层 `checksum` 字段后执行 RFC 8785 JCS，再计算小写 SHA-256。摘要不带 `sha256:` 前缀，因为外层校验值对象已经声明算法。

`envelope.schema.json` 只是可复用结构基础。使用方必须用具体消息类型的入口结构定义验证上行消息，例如 `runtime-manifest-report`、`telemetry-batch` 或 `data-loss`。只用基础结构定义不能验证消息判别字段与载荷之间的关系。

## Integration 扩展

扩展配置档位于 `profiles/cloudlink/v1alpha1/integration.json`，只新增两种上行消息：

- `integration-topology-snapshot`：`payload` 是完整、未改动的 `aether.integration.topology-snapshot.v1alpha1` 对象；
- `integration-observation-batch`：`payload` 是完整、未改动的 `aether.integration.observation-batch.v1alpha1` 对象。

外层 `gateway_id` 由已经认证的 CloudLink 会话绑定，不会在提供方中立载荷中重复。提供方地址、令牌、Cookie、凭据引用、任意提供方对象和认证材料都被禁止。主题名称和载荷身份仍不能认证网关。

每个 Integration 实例分别使用拓扑流和观测流。在一个流代次中，下列绑定不可改变：

```text
(gateway_id, stream_id, stream_epoch, message_kind, integration_id)
```

把同一绑定复用于另一个 Integration 或另一消息类型会返回 `STREAM_BINDING_CONFLICT`。位置继续使用既有 CloudLink 身份和重放规则；扩展不会创建另一套位置或确认协议。

### 拓扑消息

拓扑消息必须满足：

```text
delivery.batch_id = "topology-" + payload.snapshot_generation
```

一个已接纳代次只绑定一个精确 CloudLink 位置和摘要。精确位置重放是幂等的。较低代次出现在新位置时返回 `TOPOLOGY_GENERATION_STALE`；已接纳代次复用于新位置时返回 `TOPOLOGY_GENERATION_CONFLICT`。拓扑载荷必须是完整原子替换，禁止分片。

### 观测消息

观测消息必须满足 `delivery.batch_id == payload.batch_id`。在同一网关、Integration 与拓扑代次内，一个载荷批次身份只能绑定一个精确流位置和摘要。复用于其他位置会返回 `BATCH_ID_CONFLICT`；外层和载荷批次不一致会返回 `BATCH_ID_MISMATCH`。

云端必须已经接纳完全相同的 `integration_id` 与 `snapshot_generation`。较旧代次返回 `TOPOLOGY_GENERATION_STALE`，尚未见过的新代次返回 `REFERENCE_NOT_FOUND`。只有通过这些检查后，才能依据该拓扑验证实体、点位、质量和值类型引用。

业务摘要继续覆盖完整、原样的 Integration 对象。只有规范化 Integration 事实与重放回执都已持久提交，既有 `durable-ack` 才能成功发布。确认必须重复精确的网关、会话、流代次、位置、批次与摘要绑定。MQTT PUBACK 仍然只是传输证据。

### 消息大小与发布顺序

MQTT 的 262144 字节上限适用于完整 UTF-8 消息。超大完整拓扑返回 `FIELD_BOUND`，并且禁止发布局部拓扑。生产方只能在观测边界上拆分观测，形成具有不同批次身份的独立公共批次；每个批次都必须单独满足完整消息上限。

仅仅接受基础协议版本 `1.0` 并不会启用 Integration 扩展。发送消息前，当前 Runtime Manifest 必须在 `protocols` 中声明 `aether.cloudlink.integration.v1alpha1`，云端使用方还必须显式启用精确扩展。发布顺序必须先升级云端，再启用边缘上报。

alpha.3 使用方会正确拒绝未知消息类型。混合版本部署必须关闭 Integration 上报，不能降级消息、不能把拓扑曲解为数值遥测，也不能通过发送业务数据探测支持情况。

## 仍未覆盖的范围

alpha 遥测范围目前只携带遥测或状态点的有限 JSON 数字。非数值 Thing Model 值类型、事件，以及拓扑到模型点位的解析契约仍在规划中，不能静默推断。可选 `model` 值只是投运提示，不足以成为映射权威。

核心遥测范围没有冻结精确的有符号 `int64`、`uint64`、十进制、字节或字符串样本编码。JSON 数字不能替代精确 64 位整数契约。

CloudLink 核心和只读 Integration 扩展都不包含物理控制、直接共享内存或直接寄存器操作。独立版本、默认关闭的 `aether.cloudlink.integration-control.v1alpha1` 由 [Integration Control 规范](/aethercontracts/spec/integration-control-v1alpha1/)定义；它只公开一个受治理的语义电源设定动作，不提供任意远程调用。
