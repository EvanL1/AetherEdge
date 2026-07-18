---
title: 接入 Home Assistant
description: 把 Home Assistant 作为委托设备来源，并单独启用受严格治理的语义开关控制
updated: 2026-07-17
---

# 接入 Home Assistant

Home Assistant 桥接扩展让 AetherEdge 复用现有 Home Assistant 安装已经覆盖的设备，
同时避免把 Home Assistant 凭据发送到 AetherCloud。它在边缘主机上运行，把区域、
设备、实体和状态转换为厂商中立的只读集成投影。

> **当前状态：** 这是面向源码构建的实验性可选集成。使用 `home-assistant` 构建特性
> 编译并通过环境变量显式启用后，`aether-io` 可以装配该扩展。另一个默认关闭的
> `home-assistant-cloudlink` 构建特性可以通过两条持久 CloudLink 流发布已经提交的
> 只读投影。第三个默认关闭的 `home-assistant-integration-control` 构建特性实现了
> 一项面向灯、开关和风扇的实验性受治理 `is_on` 能力。预编译发行版和安装器尚未启用
> 这些路径；目前也没有公开命令行、HTTP/MCP 查询接口或生产级 OAuth 与密钥生命周期。

## 桥接扩展的位置

```text
设备与厂商服务
      |
      v
本地 Home Assistant
      |
      | 本地 WebSocket 连接
      v
AetherEdge Home Assistant 桥接扩展
      |
      v
只读的委托设备投影
      |
      | 可选、持久、云端先行
      v
AetherCloud 集成数据消费方
```

通过这条路径接入的设备仍以 Home Assistant 为状态权威。桥接扩展不会把 Home
Assistant 状态写入 AetherEdge 的权威共享内存点位平面；AetherCloud 也不会直接连接
Home Assistant 或保存它的令牌。即使 Home Assistant 不可用，AetherEdge 原生采集、
本地规则、安全联锁和设备控制仍会独立运行。

它是一个适配器，不是一种新的设备协议。Matter、Zigbee、蓝牙、局域网协议和厂商云
接入仍由 Home Assistant 管理；AetherEdge 只消费 Home Assistant 归一化后的结果。

## 当前交付边界

| 源码中已经具备 | 尚未交付 |
|---|---|
| 经过认证的 Home Assistant WebSocket 连接 | 已发布的支持承诺与版本兼容基线 |
| 源码可选装配，以及默认关闭的容器编排参数与持久路径接线 | 包含相应特性的预编译产物，以及由安装器管理的注册、密钥和消息代理权限 |
| 进程内存中的只读投影 | 持久投影或公开查询接口 |
| 可跨重启恢复的“拓扑摘要—世代号”本地账本 | 由安装器管理的状态路径和备份策略 |
| 区域、设备、实体和当前状态快照 | 面向用户的命令行、HTTP、MCP 或生成式应用查询接口 |
| 稳定的注册表标识与可变实体别名 | 任意 Home Assistant 服务调用 |
| 有序的 `state_changed` 状态观察 | 通用的受治理能力发现 |
| 类型化主状态与经过筛选的有限属性 | OAuth 授权、刷新、撤销与轮换 |
| 状态流缺口或注册表变化后的完整重同步要求 | 生产级密钥管理适配器 |
| 有上限的消息、集合、队列和超时 | 由安装器管理的 CloudLink 注册和消息代理权限模板 |
| 默认关闭的 CloudLink 发布，以及相互独立的拓扑和观测文件队列 | 集成扩展的正式发行启用 |
| MQTT 重连、保留记录重放、传输发布证据和按应用确认删除 | 生产级 OAuth 和已发布的支持承诺 |
| 默认关闭且绑定会话的 `device.power.set.v1` 请求，以及持久幂等、审计和回执 | 通用设备能力或任意服务调用 |
| 固定的灯、开关、风扇开启与关闭映射 | 物理完成证明；提供方接纳后的物理结果仍为未知 |
| 模拟服务端、运行时装配与提供方一致性测试 | 楼层、标签、配置项和完整服务元数据 |
| 正确保留 `unknown` 与 `unavailable`，不伪造数值 | 把 AetherEdge 原生设备反向暴露给 Home Assistant |
| | 已发布的 Home Assistant 版本兼容矩阵和真实实例验证 |

该集成不读取 `io.yaml`，`aether sync` 也不会激活它。当前源码构建通过明确的进程环境
变量启用。

## 在源码构建中启用

先按[快速开始](/guides/getting-started)准备好源码检出和运行配置，再使用可选适配器构建并启动
`aether-io`：

```bash
export AETHER_HOME_ASSISTANT_ENABLED=true
export AETHER_HOME_ASSISTANT_ORIGIN='https://homeassistant.example.lan:8123'
export AETHER_HOME_ASSISTANT_ACCESS_TOKEN_REF='env:AETHER_HOME_ASSISTANT_TOKEN'
export AETHER_HOME_ASSISTANT_TOKEN='<访问令牌>'
export AETHER_GATEWAY_ID='home-edge'
export AETHER_HOME_ASSISTANT_INTEGRATION_ID='home-assistant-main'
export AETHER_HOME_ASSISTANT_GENERATION_STORE_PATH="$PWD/data/home-assistant-topology-generations.json"

cargo run -p aether-io --features home-assistant
```

`AETHER_HOME_ASSISTANT_INTEGRATION_ID` 可以省略，默认值为 `home-assistant`。启用集成时，
上面其余设置均为必填项。世代号账本必须填写绝对文件路径；示例中的 `$PWD` 会展开为当前
源码检出的绝对路径。未设置启用开关时默认为关闭；显式值只接受 `true`、`1`、`false`
和 `0`。

如果启用了 Home Assistant，但当前二进制没有编译 `home-assistant` 构建特性，或者缺少
必填设置、站点根地址或密钥引用无效，进程会拒绝启动。系统明确禁止使用明文设置
`AETHER_HOME_ASSISTANT_ACCESS_TOKEN`。令牌只能保存在
`AETHER_HOME_ASSISTANT_ACCESS_TOKEN_REF` 所引用的环境变量中。

启动后，该集成在 `aether-io` 内维护一份内存投影；进程重启后会通过完整快照重新填充。
本地世代号账本只保存“拓扑摘要—递增世代号”的对应关系，不保存设备状态、Home Assistant
凭据或内存投影。拓扑摘要不变时，重启后仍使用原世代号；摘要变化时，系统会先持久化下一
世代号，再发布新拓扑。

同一时刻只能有一个进程打开该账本。账本损坏、不可写或已经被其他进程锁定时，服务会拒绝
启动。应把它放在可靠的本地存储中，不要在 `aether-io` 运行期间手工编辑，并把它纳入现场
备份策略。目前没有对外读取内存投影的正式接口，因此仅启用本地投影的模式适合开发集成和
验证真实 Home Assistant 实例，还不能作为最终用户自动化入口。

## 可选发布到 CloudLink

CloudLink 发布需要单独启用。仅启用 Home Assistant 数据来源不会声明或启动云端发布。
`aether-io` 必须使用 `home-assistant-cloudlink` 构建特性编译，同一产物经过验证的
`runtime-manifest.json` 必须声明 `aether.cloudlink.integration.v1alpha1`，而且兼容的
云端消费方必须先启用。

先为同一源码构建产物生成运行时清单：

```bash
mkdir -p "$PWD/data/runtime"
cargo run -p aether-runtime-catalog --bin aether-runtime-manifest -- \
  generate \
  --output "$PWD/data/runtime/runtime-manifest.json" \
  --target aarch64-unknown-linux-musl \
  --io-features home-assistant-cloudlink
```

然后设置明确的持久文件路径和经过认证的 TLS MQTT 连接参数：

```bash
export AETHER_GATEWAY_ID='33333333-3333-4333-8333-333333333333'
export AETHER_HOME_ASSISTANT_CLOUDLINK_ENABLED=true
export AETHER_HOME_ASSISTANT_CLOUDLINK_ORIGIN_MODEL='gateway-signed'
export AETHER_HOME_ASSISTANT_CLOUDLINK_CLOUD_KEY_ID='cloud-session-key-1'
export AETHER_HOME_ASSISTANT_CLOUDLINK_CLOUD_PUBLIC_KEY_REF='env:AETHER_CLOUDLINK_CLOUD_PUBLIC_KEY'
export AETHER_CLOUDLINK_CLOUD_PUBLIC_KEY='<无填充Base64url编码的32字节Ed25519公钥>'
export AETHER_HOME_ASSISTANT_CLOUDLINK_GATEWAY_KEY_ID='edge-session-key-1'
export AETHER_HOME_ASSISTANT_CLOUDLINK_GATEWAY_SIGNING_KEY_REF='env:AETHER_CLOUDLINK_GATEWAY_SIGNING_KEY'
export AETHER_CLOUDLINK_GATEWAY_SIGNING_KEY='<无填充Base64url编码的32字节Ed25519私钥种子>'
export AETHER_HOME_ASSISTANT_CLOUDLINK_CHALLENGE_LEDGER_PATH="$PWD/data/ha-challenges.json"
export AETHER_HOME_ASSISTANT_CLOUDLINK_RUNTIME_CONFIG_DIR="$PWD/data/runtime"
export AETHER_HOME_ASSISTANT_CLOUDLINK_CLOUD_EXTENSION='aether.cloudlink.integration.v1alpha1'
export AETHER_HOME_ASSISTANT_CLOUDLINK_TOPOLOGY_SPOOL_PATH="$PWD/data/ha-topology.spool"
export AETHER_HOME_ASSISTANT_CLOUDLINK_OBSERVATION_SPOOL_PATH="$PWD/data/ha-observations.spool"
export AETHER_HOME_ASSISTANT_CLOUDLINK_SPOOL_CAPACITY=4096
export AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_BROKER_HOST='broker.example.net'
export AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_BROKER_PORT=8883
export AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_CLIENT_ID='aether-edge-home'
export AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_TOPIC_PREFIX='aether'
export AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_USERNAME='edge-home'
export AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_PASSWORD_REF='env:AETHER_CLOUDLINK_MQTT_PASSWORD'
export AETHER_CLOUDLINK_MQTT_PASSWORD='<消息代理密码>'
export AETHER_HOME_ASSISTANT_CLOUDLINK_CREDENTIAL_ID='edge-home-connector'
export AETHER_HOME_ASSISTANT_CLOUDLINK_CREDENTIAL_GENERATION=1
export AETHER_HOME_ASSISTANT_CLOUDLINK_SESSION_EPOCH_PATH="$PWD/data/ha-session-epoch"

cargo run -p aether-io --features home-assistant-cloudlink
```

这套组合使用系统 TLS 根证书，并要求消息代理使用用户名和密码认证。消息代理与云端消费方
必须实施精确到网关主题的访问控制。云端发出有界且带签名的会话质询；边缘先验签，再签署
会话建立请求。只有会话代次在本地提交后，边缘才会发送保留记录。会话建立、心跳、拓扑、
观测、运行时清单、遥测、数据丢失声明和控制回执都使用同一个网关会话密钥标识。

可信连接器来源模型只用于明确的开发环境和跨仓库测试。它依赖消息负载之外的代理认证，
因此不会写入 `message_authentication`，也不会制造占位签名。

拓扑和观测分别使用可跨崩溃恢复的文件队列。MQTT 发布确认只表示传输成功；记录必须继续
留在队列中，直到收到严格绑定当前会话的 CloudLink 应用确认。进程重启后，待发送记录保留
原始消息类型、发送与过期时间、流身份、批次标识、业务摘要和负载。同一会话内重试使用完全
相同的字节；新会话只更新会话标识、递增会话代次并重新签名。云端不可用不会停止已经投运
的边缘原生采集、规则、安全联锁或控制。

缺少构建特性、经过验证的运行时清单声明、云端确认、凭据引用、TLS MQTT 配置或任一绝对
队列路径时，CloudLink 会在启动数据同步任务前拒绝启动。

## 实验性受治理开关控制

受治理控制是第三条独立的可选路径。启用 Home Assistant 或只读 CloudLink 发布，都不会
顺带启用控制。同一个运行时必须显式打开三个开关：

```bash
export AETHER_HOME_ASSISTANT_ENABLED=true
export AETHER_HOME_ASSISTANT_CLOUDLINK_ENABLED=true
export AETHER_HOME_ASSISTANT_CONTROL_ENABLED=true
```

使用 `home-assistant-integration-control` 构建 `aether-io`，并用同一个输入生成运行时
清单。特性归一化会同时记录只读集成协议和集成控制协议：

```bash
cargo run -p aether-runtime-catalog --bin aether-runtime-manifest -- \
  generate \
  --output "$PWD/data/runtime/runtime-manifest.json" \
  --target aarch64-unknown-linux-musl \
  --io-features home-assistant-integration-control

cargo run -p aether-io --features home-assistant-integration-control
```

经过校验的清单必须同时声明 `aether.cloudlink.integration.v1alpha1` 和
`aether.cloudlink.integration-control.v1alpha1`。缺少任意声明、信任材料或持久化输入时，
边缘运行时都会拒绝启动：

```bash
export AETHER_HOME_ASSISTANT_CONTROL_CLOUD_EXTENSION='aether.cloudlink.integration-control.v1alpha1'
export AETHER_HOME_ASSISTANT_CONTROL_LEDGER_PATH="$PWD/data/control/jobs-and-receipts.json"
export AETHER_HOME_ASSISTANT_CONTROL_POLICY_PATH="$PWD/data/control/policy.json"
export AETHER_HOME_ASSISTANT_CONTROL_AUDIT_PATH="$PWD/data/control/audit.jsonl"

export AETHER_HOME_ASSISTANT_CONTROL_CLOUD_KEY_ID='cloud-control-key-1'
export AETHER_HOME_ASSISTANT_CONTROL_CLOUD_PUBLIC_KEY_REF='env:AETHER_CONTROL_CLOUD_PUBLIC_KEY'
export AETHER_CONTROL_CLOUD_PUBLIC_KEY='<无填充Base64url编码的32字节Ed25519公钥>'

export AETHER_HOME_ASSISTANT_CONTROL_PROVIDER_TIMEOUT_MS=5000
```

提供方调用超时可省略，默认是 5000 毫秒，允许范围为 1 至 30000 毫秒。密钥材料必须使用
规范的无填充 Base64url 编码。云端下发的控制请求使用单独配置的云端验签公钥；边缘回执
复用当前 CloudLink 网关会话签名密钥，并且密钥标识必须完全一致。生产组合不存在第二把
回执私钥；轮换、撤销、硬件密钥存储和生产注册流程仍在规划中。

不应再设置 `AETHER_HOME_ASSISTANT_CONTROL_EDGE_KEY_ID` 和
`AETHER_HOME_ASSISTANT_CONTROL_EDGE_SIGNING_KEY_REF`。在网关签名模式下，这两个旧名称
只有同时存在并与 CloudLink 网关密钥标识和引用完全一致时才会被接受。明确启用的可信
连接器测试仍可同时设置它们，作为独立的旧测试回执签名密钥；该密钥不会被表示成会话认证。

本地策略是结构封闭且默认拒绝的 JSON 文档。每个实体都必须分别列入已投运清单和已委托
清单，请求主体也必须在允许清单内：

```json
{
  "schema": "aether.edge.integration-control-policy.v1",
  "gateway_id": "33333333-3333-4333-8333-333333333333",
  "integration_id": "home-assistant.home",
  "permission": "integration.device.control",
  "commissioned_entities": ["entity-registry-light-bedroom"],
  "delegated_entities": ["entity-registry-light-bedroom"],
  "allowed_subjects": ["user-homeowner"]
}
```

策略、账本、审计文件和同级锁文件应放在权限为 0700 的真实目录中。在 Unix 系统上，已有
账本、审计或锁文件只要向组用户或其他用户开放了任意权限就会被拒绝；新建敏感文件使用
0600。系统也会拒绝符号链接文件和作为直接父目录的符号链接。非 Unix 系统仍会执行普通
文件、非符号链接、进程锁和独占替换检查，但 Unix 权限位本身不可移植。

控制主题不属于基础 MQTT 订阅。只有当前 CloudLink 会话已经接纳、并且新的会话代次已经
持久保存后，运行时才会订阅唯一的控制下行主题。随后每个请求都必须绑定当前网关、会话
标识、会话代次和凭据代次，通过配置的云端 Ed25519 公钥验签，解析到当前拓扑的精确世代，
再由本地策略作最终决定。

公共协议不能携带 Home Assistant 域名、服务名、`service_data`、网址、令牌或提供方实体
地址。边缘根据稳定的实体注册表标识在本地解析地址，并且只把灯、开关和风扇的布尔
`is_on` 映射为固定的开启或关闭调用。提供方接纳只会生成回执，不代表物理设备已经完成，
也不代表作业成功；物理结果始终为未知。

运行时会在订阅控制主题之前打开作业账本。调用提供方前必须先持久认领作业；相同的
`gateway_id`、`job_id` 与 `intent_digest` 组合绝不会再次调用 Home Assistant。进程在
认领后中断时，重启会生成结果未知的回执，不会重试提供方。回执在断线和重启后保留原始
消息类型、发送时间、流位置、批次标识、业务摘要和负载。进入新会话时，只会使用同一把
网关会话密钥对这些不变事实重新签名。MQTT 发布确认不会删除回执，只有严格绑定当前会话
的 CloudLink 应用持久确认才可以删除。

网关心跳和所有持久上行都签署协议冻结的十三字段规范对象，不适用的投递字段使用 JSON
空值。当前会话接纳、心跳确认和应用持久确认都没有签名对象，因此保持无签名；解码器会
拒绝携带 `message_authentication` 的心跳确认。这些云端到边缘的签名缺口仍是生产就绪
声明的明确阻塞项。

## 连接参数

连接参数需要填写 Home Assistant 的**站点根地址**，而不是完整接口地址。

| 设置 | 示例 | 规则 |
|---|---|---|
| 站点根地址 | `https://homeassistant.example.lan:8123` | 只能是 HTTP 或 HTTPS 根地址，不能包含用户名、密码、路径、查询参数或片段 |
| 推导出的 WebSocket 地址 | `wss://homeassistant.example.lan:8123/api/websocket` | 由站点根地址自动生成 |
| 令牌引用 | `env:AETHER_HOME_ASSISTANT_TOKEN` | 这里只保存引用，不能写入令牌原文 |
| 拓扑世代号账本 | `/var/lib/aether/home-assistant-topology-generations.json` | 必填的本地绝对文件路径；`aether-io` 必须能够写入并独占锁定 |

以 `http://` 开头的站点根地址会转换为
`ws://<主机>/api/websocket`。这种明文连接只适合隔离且可信的本地投运网络。
正常运行应正确配置 TLS，并使用 `https://` 根地址，让 WebSocket 使用 `wss://`。

当前本地解析器只接受 `env:` 引用。变量名必须以大写字母或下划线开头，并且只能包含
大写字母、数字和下划线：

```bash
export AETHER_HOME_ASSISTANT_TOKEN='<访问令牌>'
```

只把这个变量提供给承载桥接扩展的进程。不要把令牌提交到 YAML、环境变量模板、脚本、
容器镜像、日志或智能体提示词中。生产部署应从受保护的进程环境或密钥存储中注入，并限制
能够读取该进程环境的系统身份。

## 认证边界

Home Assistant 使用访问令牌认证 WebSocket 客户端。它的
[认证文档](https://developers.home-assistant.io/docs/auth_api/)同时说明了两种方式：
一种是通过 OAuth 获得短期访问令牌和刷新令牌，另一种是人工创建长期访问令牌。

当前 AetherEdge 桥接扩展只解析一个访问令牌，尚未实现授权跳转、刷新令牌保存、
自动刷新或撤销。因此：

- 人工创建的长期令牌只应用于本地开发和受控投运；
- 在条件允许时创建专用 Home Assistant 用户，并只授予读取注册表和状态流所需的权限；
- 把令牌视为高价值密钥；怀疑泄露时立即轮换，停用桥接时从 Home Assistant 中删除；
- 不要把未认证或明文的 Home Assistant 接口暴露到不可信网络；
- 不要把当前环境变量令牌解析方式称为生产级 OAuth 集成。

正式发行前还需要面向操作员的授权流程、刷新令牌生命周期、安全持久化、撤销机制和兼容性
测试，之后才能替换人工令牌路径。

## 首次快照

建立新连接时，桥接扩展按以下顺序工作：

1. 打开 Home Assistant WebSocket 地址并完成认证；
2. 启用有上限的消息合并并订阅事件；
3. 读取区域、设备和实体注册表；
4. 读取当前状态集合；
5. 发布一份内部一致的拓扑与状态快照；
6. 按顺序应用快照期间缓存的事件和后续状态变化。

先订阅、后读取快照，可以避免注册表读取期间发生的变化被静默遗漏。快照命令执行期间收到的
事件会进入有上限的缓存，并在快照之后处理。

`light`、`switch` 和 `fan` 的主开关状态使用稳定的布尔点 `is_on`；其他实体类型仍使用
主点 `state`。这是明确的公共映射，不会为了兼容而静默复制同一个 Home Assistant 状态。
已知实体类型会映射为更精确的数据类型，未知实体类型则保留为长度受限的字符串，不会被
丢弃。当前只投影以下属性：

| Home Assistant 实体类型 | 额外投影属性 |
|---|---|
| `light` | `brightness`、`color_temp_kelvin` |
| `climate` | `current_temperature`、`temperature`、`current_humidity`、`hvac_action` |
| `cover` | `current_position`、`current_tilt_position` |
| `fan` | `percentage`、`preset_mode` |
| `vacuum` | `battery_level` |
| `media_player` | `volume_level`、`is_volume_muted` |
| `event` | `event_type` |

Home Assistant 返回其他属性，并不代表这些属性会自动进入投影。默认筛选可以阻止密钥、
超大数据和高基数厂商字段进入 AetherEdge。当前媒体标题等内容也默认视为私密数据；在
未来具备明确的隐私与出站策略前，不会进入投影。

如果 Home Assistant 为上表中的已声明点返回类型错误、无法解析或非有限的值，桥接扩展
会把该点标记为未知，并且不携带任何伪造值。同一快照或事件中的其他合法点继续正常处理；
后续合法事件也可以把该点恢复为正常质量。提供方明确报告的未知与不可用状态仍保留原有
语义。这种容错只适用于单个点值；身份冲突、拓扑变化、状态流缺口和队列溢出仍会拒绝继续
处理，并要求完整重同步。

## 断线与完整重同步

Home Assistant 不为这条连接提供可持久恢复的事件游标。发生断线、队列溢出、实体删除、
注册表更新、未知实体、序列缺口或拓扑冲突后，增量处理必须停止。调用方必须先取得一份新的
完整快照，才能继续接收状态观察。

桥接扩展不会编造丢失状态，也不会把较新的事件直接套用到旧拓扑上。完整快照成功之前，
消费方必须把投影视为过期。Home Assistant 故障不会停止已经投运的 AetherEdge 原生行为，
但故障期间不能把委托接入设备的状态视为最新状态。

## 故障排查

| 现象 | 检查方法 |
|---|---|
| 站点根地址立即被拒绝 | 只填写 `http://` 或 `https://` 根地址；去掉 `/api`、`/api/websocket`、凭据、查询参数和片段 |
| 令牌引用被拒绝 | 使用 `env:变量名`，变量名遵循可移植的大写命名规则 |
| 找不到凭据 | 确认变量存在于桥接进程环境中，而不只是当前登录终端中 |
| 认证被拒绝 | 重新创建或轮换令牌，确认 Home Assistant 用户仍然有效，并检查令牌是否完整复制 |
| 读取注册表时提示无权访问 | 检查专用用户是否能读取区域、设备和实体注册表；不要把管理员令牌直接写进源码来绕过错误 |
| TLS 连接失败 | 使用边缘主机信任的证书，并确保根地址中的主机名与证书一致 |
| 重连后状态不再更新 | 触发完整快照，不要沿用上一次内存中的序列 |
| 新实体或新区域没有出现 | 注册表变化需要完整快照，不能只依赖增量状态事件 |
| 某个属性没有出现 | 当前版本只投影上表列出的有限属性 |
| 启动提示未编译相应特性 | 使用 `--features home-assistant` 重新构建 `aether-io`；当前预编译发行版不包含该特性 |
| CloudLink 拒绝运行时清单 | 使用 `home-assistant-cloudlink` 构建同一产物，并生成明确列出该特性的运行时清单 |
| CloudLink 记录一直留在磁盘 | 确认云端消费方发送应用持久确认；MQTT PUBACK 不足以删除记录 |
| CloudLink 会话一直无法建立 | 检查 TLS 信任、消息代理凭据、网关主题权限，以及云端消费方是否发送质询和接纳消息 |
| 控制功能拒绝启动 | 检查三个启用开关、运行时清单中的两个协议标识、两项 Ed25519 密钥引用，以及互不相同的绝对账本、策略和审计路径 |
| 控制请求被拒绝 | 检查当前会话边界、签名密钥标识、有效期、精确拓扑世代、已投运和已委托实体清单，以及已确认主体 |
| 收到 MQTT 发布确认后仍然重发回执 | 这是预期行为；只有严格绑定当前会话的 CloudLink 应用持久确认才能删除回执 |
| 找不到启用或查询桥接扩展的 `aether` 命令 | 当前源码构建通过进程环境变量启用，且投影尚无公开查询接口，这是预期限制 |

上游传输协议见 Home Assistant 官方
[WebSocket API](https://developers.home-assistant.io/docs/api/websocket/)；AetherEdge
控制安全边界见[安全操作](/guides/safe-operations)。
