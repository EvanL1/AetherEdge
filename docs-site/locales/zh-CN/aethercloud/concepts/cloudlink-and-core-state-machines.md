---
title: "CloudLink 可靠传输与生命周期"
description: "了解消息交付、重试、确认、离线恢复和当前生产限制"
updated: 2026-07-16
status: mixed
---

# CloudLink 可靠传输与生命周期

> 权威来源：[AetherCloud](https://github.com/EvanL1/AetherCloud/blob/main/docs/concepts/cloudlink-and-core-state-machines.md)。此页面镜像到统一的 AetherIoT 文档中。

实验性 JSON/MQTT v1 编解码器、应用桥、可独立启动的入口进程、结构定义、测试夹具和真实消息代理测试工具已经存在，公开的 AetherContracts alpha.3 版本是唯一的共享协议权威。这些能力属于可执行的 alpha 证据，还不是生产级服务。达到生产就绪状态仍需依次完成身份验证、统一线路配置、共享测试夹具、边缘与云双端测试、故障注入、崩溃耐久持久化和明确的旧版本切换。

## CloudLink 责任

CloudLink 验证网关凭据、协商协议版本、隔离会话、传输版本化信封、维护每个流序列和持久确认游标，应用背压，并在断开连接后恢复。它调用应用程序摄取和交付用例。

CloudLink 不拥有遥测含义、部署策略、作业执行或警报生命周期。不能直接写入SHM、点或设备寄存器；调用任意运行时方法；激活未发布的工件；绕过作业确认；或清除边缘权威警报。

HTTP 在事务存储和发件箱中创建所需状态和受管作业。 CloudLink 读取应用程序拥有的出站邮箱。 HTTP 处理程序永远不会写入实时边缘套接字。

## 网关注册

实现的基础：
```text
registered -> awaiting-claim -> claimed
                    |
                 expired
```

计划扩建生产：
```text
claimed -> credential-pending -> active <-> suspended -> revoked
                                      network loss      |
                                      changes no state  v
                                                recovery-pending
                                                       |
                                              active (new generation)
```

- `now >= claim.expiresAt` 拒绝声明。
- 相同的声明请求和凭据请求指纹是幂等重放。不同的请求无法重新绑定已声明的网关。
- 在证书颁发之前成功的声明可以恢复凭据绑定，而无需消耗新令牌。
- 断开连接不会撤销身份。撤销会隔离凭证生成；恢复永远不会恢复它。
- 注册或声明成功并不能证明 CloudLink 身份验证处于活动状态。

## CloudLink 会话

域/应用程序/内存基础实现凭据验证、服务器偏好协议协商、单调会话纪元、旧会话防护、无损每流恢复游标、经过身份验证的心跳和租户范围的当前会话查询。实验性 MQTT 层添加了严格的有线解码、主题/会话绑定、非保留 QoS-1 实施、应用程序桥接和生命周期组合。 PostgreSQL、多实例套接字所有权、超时调度、信用流控制、持久收件箱/发件箱和生产配置仍在计划中。

现有结构签名证据不足以证明共享消息代理具备生产级身份验证。Alpha.3 先使用已签名的云端质询和网关签名，再用网关签名将通用消息代理上的每条上行消息绑定到该会话。消息代理专用适配器可以为每次发布提供经过验证的带外发布者证明。载荷自带的证明、主题身份或一次成功的 `hello` 都不能验证后续消息。云端质询使用提案中的签名记录；alpha.3 持久应用 ACK 明确尚未签名。
```text
authenticating -> negotiating -> resuming -> active -> draining -> closed
       |              |             |          |
    rejected       rejected      rejected    suspect -> active or closed
```

- 每个经过身份验证的连接都会获得一个单调会话纪元。较新的纪元会隔离旧连接，包括来自旧套接字的迟到消息。
- 从每个流的 AetherCloud 的最后一个持久确认开始恢复。
- 相同的身份和摘要是重复的，并接收相同的确认。具有不同摘要的相同身份被隔离为安全冲突。
- 序列间隙请求重播并且不会推进光标。无序信封可以在有界窗口内持久缓冲。
- 心跳超时将连接投影移动到 `suspect` 或 `closed`；它不能证明网关或下游设备已停止。
- 显式信用/窗口流控制和有界队列可防止快速发送方耗尽进程或租户资源。

## 运行时清单报告

实现的传输中性报告命令仅在凭证身份验证后接受封闭的 AetherEdge 运行时清单 v1 形状。它验证与 AetherEdge 相同的规范 JSON SHA-256 合约，将其生成表示为规范的无符号 64 位十进制字符串，并从已验证的凭据（而不是有效负载标识符）派生租户、项目和网关范围。
```text
no current -> accepted-latest
older generation -> accepted-late (history only)
same generation + same digest -> replayed
same generation + different digest -> rejected conflict
newer generation -> accepted-latest
```

最新的观察结果仍然是可查询的历史，但不能向后移动最新的能力预测。功能报告是兼容性证据，而不是调用边缘方法的授权。实验性 CloudLink MQTT 报告信封已实施。 PostgreSQL 投影、公共查询传输、持久审计和发件箱仍在计划中。

## 工件发布
```text
draft -> validated -> published -> deprecated -> withdrawn
                         \---------------------> withdrawn
```

- 一旦内容摘要识别出修订版，其字节和兼容性元数据就是不可变的。更正会创建新的修订版。
- 发布需要成功的验证、出处、兼容性和签名策略。部分验证不会发布。
- 通道是单独审核的可移动引用；移动它不会编辑修订版。
- 撤回会阻止新的部署，但会保留审计和恢复所需的 blob、签名和应用证据。
- 使用相同的摘要和幂等密钥重复上传或发布会返回原始结果；冲突的内容被拒绝。

实现了冻结域转换、受控发布/查询应用程序用例以及原子内存内容/签名/元数据/审核/发件箱适配器。上传/最终确定、生产对象存储和签名者策略、PostgreSQL 元数据、持久审计/发件箱、通道移动、生命周期 HTTP 和边缘工件交付仍在计划中。请参阅[工件注册表](/aethercloud/concepts/artifact-registry) 了解确切的图层状态。

## 遥测批量摄取
```text
received -> decoded -> validated -> persisted -> acknowledged -> projected
    |          |           |           |
 rejected   rejected    quarantined  pending-gap
```

- 批次标识是租户、项目、网关、逻辑流、流纪元和第一个位置。 JSON 将协议 64 位位置编码为规范十进制字符串。
- MVP 批次是原子的。在任何业务记录被接受之前，一个无效记录会拒绝该批次。
- 相同的身份和业务内容摘要返回先前的持久确认。具有不同内容的相同标识是冲突的重复项并被隔离。
- 间隙可能会在有界持久重新排序窗口中保留批次，但无法推进连续光标。溢出成为明确的丢失标记。
- 持久性失败不会返回成功的确认。不明确的提交由重播时的相同身份解决。
- 持久接受后的投影失败不会撤销收据；工作进程从发件箱重试投影。
- 断开连接不会更改批处理状态，并且边缘可能会重播每个未确认的位置。
- 入口后不支持批量取消。保留和删除是单独的受控生命周期操作，而不是取消历史事实。

## 所需部署、报告和应用

部署状态：
```text
planned -> running <-> paused -> completed
              |                 completed-with-failures
              v
          cancelling -> cancelled
```

目标状态：
```text
pending -> offered -> accepted -> fetching -> validated -> applied
             |          |           |            |
          expired    rejected     failed       failed or unknown
```

- 期望的生成是单调的云意图。报告和应用的是具有自己的代、时间和证据的单独边缘观测。
- 保留较老代的后期观测，但无法向后滚动最新投影。
- `offered`、`accepted`、`fetching` 和 `validated` 绝不意味着 `applied`。
- 网络超时产生`unknown`。较晚的边缘接收可能会在稍后解决该问题。
- 取消会停止尚未跨越相关边缘接收边界的工作。它无法删除已经执行的工作。
- 回滚会创建一个新的所需生成，指向较旧的不可变修订版本；它从不编辑历史记录。
- 部分成功是一流的推出结果。薪酬是一项新的、可审核的部署。

## 受监管的能力工作
```text
created -> awaiting-confirmation -> authorized -> queued -> offered
   |              |                    |            |
 expired       rejected              expired     accepted -> running
                                                        |      |      |
                                                   succeeded failed partial
                                                        |
                                                     unknown
```

- 权限、风险策略、确认、先决条件、幂等性、过期和审核要求未能关闭。
- 边缘可能会因为调试、兼容性、授权或安全策略而拒绝工作。
- 提供或执行后的超时为 `unknown`，不是失败，也不允许创建新的非幂等物理操作。
- 重试使用相同的作业身份，并且仅当功能满足时才允许重试声明安全重播语义。
- 取消是一个请求。如果工作已经开始或完成，其实际收据仍然具有权威性。
- 迟到的收据可以解决`unknown`；它无法重写早期的审核事件。

## 命令收据
```text
ingressed -> authenticated -> persisted -> acknowledged -> projected
     |             |             |
  rejected      quarantined    pending-predecessor
```

- 收据是仅附加事实，包含身份、有效负载摘要、作业因果关系、边缘序列、观察时间和可选证据摘要。
- 重复的身份和摘要是重放安全的。具有不同内容的相同身份将被隔离，并且永远不会被确认为成功。
- 在预期前任之前到达的收据将被持久保留并标记为待处理，而不是被丢弃。
- 仅在收据、收件箱重复数据删除和证据引用原子持久后才会发生确认。
- 大量证据在对象存储中按内容寻址。丢失或不匹配的证据仍然可见。
- 投影可以将早期作业从 `unknown` 移动到最终结果，同时保留完整的时间线。

## 警报投影

边缘发生事实：
```text
unseen -> active -> updated* -> cleared
                  \-> superseded or retracted correction
```

云工作流程叠加：
```text
unacknowledged -> acknowledged
freshness: complete | gap | stale
```

- 边缘警报身份、生成和顺序使摄取重放安全。
- 无序清除将保留为待处理，并标记投影未完成，直到重放填补空白为止。
- 断开连接标记新鲜度陈旧；它不会清除或解决警报。
- 存储转发溢出会创建可见的间隙或数据丢失标记。
- 云确认、评论、通知静音和分配是云工作流程事实。它们永远不会更改边缘的活动/清除事实。
- 重新引发的条件使用新的事件或显式生成，而不是默默地重新打开已清除的记录。

## Webhook 传递
```text
pending -> leased -> delivered
   |          |
 disabled   retry-wait -> leased
                  |
             dead-letter -> redrive-requested -> pending
```

- 一个集成事件和端点会产生稳定的交付身份。释放或重试租约永远不会创建新的业务事件。
- HTTP超时是未知的远程消费结果，因此重试会保留相同的事件和传送标识。
- 重试使用有界指数延迟和尝试上限。耗尽使死信状态可见，而不是默默地丢弃事件。
- 端点禁用会停止新的租约，但无法撤消远程副作用。删除会保留所需的审核和交付历史记录。
- Redrive 是具有权限、幂等性、过期和审核功能的受控命令；它会创建另一个尝试，而不是另一个事件。
- 部分扇出成功对每个端点仍然可见。订阅者之间不会声明分布式事务。

这些规则保持边缘优先的权限边界：边缘仍负责实时状态和物理控制，云端只记录已接受的事实和受治理的意图，不会把未经证明的设备结果声明为成功。
