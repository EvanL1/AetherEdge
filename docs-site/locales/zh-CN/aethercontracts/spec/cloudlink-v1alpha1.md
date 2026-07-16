---
title: "CloudLink v1 alpha 1 说明"
description: "AetherContracts 是该协议切片唯一的互操作性权威；产品仓库仅提供实现配置、清单、门槛和证据"
updated: 2026-07-15
id: cloudlink-v1alpha1
status: experimental-auth-proposal
version: 0.1.0-alpha.3
normative: true
---

# CloudLink v1 alpha 1 说明

> 本中文页面用于帮助理解。规范性定义、Schema、测试夹具和 TCK 以 AetherContracts 对应版本的英文发布内容为唯一权威。

> 权威来源：[AetherContracts](https://github.com/EvanL1/AetherContracts/blob/main/spec/cloudlink-v1alpha1.md)。此页面镜像到统一的 AetherIoT 文档中。

AetherContracts 是此协议切片的唯一互操作性权威机构。产品仓库接线配置文件、清单、门和证据文件是非权威实施覆盖，不得添加或重新定义接线字段。历史联合核心出处仅记录 alpha.2 输入字节的来源；它不授予持续的共同所有权。

Alpha.3 冻结封闭的信封和时间/身份字段、会话质询/问候/接受、心跳、运行时清单报告、遥测批处理、数据丢失证据、重播请求和未签名的持久应用程序 ACK。 TypeScript、Rust、C99 和 C++17 执行相同的公共固定清单和稳定的故障字符串。这些夹具表面是实验性的，不是完整的生产传输编解码器。

身份验证配置文件区分两个原始模型。在 `gateway-signed` 中，云发出一次性签名质询，网关对准确的会话建立对象进行签名，每个上行链路对准确的每上行链路对象进行签名。在 `trusted-connector-broker-attestation` 中，配置的受信任入口适配器在负载外部提供源证据，并绑定准确接收的 MQTT 负载字节。有效负载无法证明其自身。主题名称、负载身份和 MQTT 凭证本身绝不能作为网关身份验证。

该提案在 `profiles/cloudlink/v1alpha1/authentication.json` 中准确指定了 Ed25519 算法、unpadded-base64url 编码、RFC 8785 JCS 签名对象、缺席值规则和重播边界。生产密钥配置、轮换、撤销、验证者所有权和生产签名验证仍在计划中，因此身份验证门仍然是一个提案，并且切换被阻止。普通日志和公共证据必须排除签名、随机数、凭证标识符和原始身份验证记录。

alpha.3 持久 ACK JSON 形状明确未签名。成功意味着应用程序事实和收据在 ACK 发布之前持久提交，但 alpha.3 不包含生产存储/发件箱实现，并且没有崩溃持久性声明。未来签名的 ACK 是一个单独的命令/配置文件，需要云密钥生命周期以及生产重启证据。 MQTT PUBACK 绝不是应用程序持久收据。

使用相同重播身份和摘要的重复传递是幂等的。具有不同摘要的身份的重用是线路有效的，但上下文无效的；它被隔离为 `DIGEST_CONFLICT`，并且没有收到成功的收据。数据丢失是明确的证据，绝不会导致 Cloud 伪造样本。

数据丢失证据满足 `first_lost_position <= last_lost_position < earliest_retained_position`。心跳和恢复游标数组对于每个 `(stream_id, stream_epoch)` 最多包含一个条目。违规行为在上下文中无效，不会更改业务事实，也不允许成功接收申请。

唯一持久的职位身份是 `(gateway_id, stream_id, stream_epoch, position)`。 `batch_id` 和 `digest` 是该位置的稳定绑定，而不是创建第二个身份的字段；更改任何一个都无法绕过冲突检测。业务摘要是 RFC 8785 JCS 的小写 SHA-256，正好位于 `{protocol_version,message_kind,payload}` 上。它提供内容完整性和重播比较，而不是发布者身份验证。机器可读的形式是`profiles/cloudlink/v1alpha1/core.json`。

`expires_at_ms`是可选的；当省略时，该字段不规定消息截止日期。如果存在，它必须大于或等于 `sent_at_ms`，否则该消息对于 `INVALID_EXPIRY_WINDOW` 来说是上下文无效的。过期是根据显式规范 uint64 `evaluation_time_ms` 进行评估的：检查仅在 `evaluation_time_ms < expires_at_ms` 时通过，并且相等性已在 `MESSAGE_EXPIRED` 过期。便携式 TCK 提供此评估时间作为场景输入，并且从不参考环境挂钟。两次到期失败都会使业务状态保持不变，并禁止成功接收应用程序。

嵌入式运行时清单校验和是完整清单对象的 RFC 8785 JCS 上的小写 SHA-256，并省略了其顶级 `checksum` 成员。它的摘要省略了 `sha256:` 前缀，因为封闭的校验和对象已经声明了算法。

`envelope.schema.json` 是可重用的结构基础。消费者必须使用其消息类型条目 Schema（`runtime-manifest-report`、`telemetry-batch` 或 `data-loss`）验证上行链路；单独使用基数并不能验证鉴别器与有效负载的关系。

alpha 遥测切片当前仅携带遥测/状态点的有限 JSON 数值。非数字Thing Model值类型、事件和拓扑到模型的点解析契约是计划好的，而不是默默推断的。可选的示例 `model` 值只是一个调试提示，并不足以映射权限。

此切片不会冻结精确的有符号 `int64`、`uint64`、十进制、字节或字符串示例编码。 JSON 数字不能替代精确的 64 位整数合约。

传统传输仍然是默认设置。 CloudLink 不包含物理控制、直接 SHM 或直接寄存器操作。
