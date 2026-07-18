---
title: "配置参考"
description: "YAML 配置架构、同步管道和环境变量"
updated: 2026-07-17
---

# 配置参考

操作配置位于 `config/` 目录下的 YAML、CSV 和 JSON 文件中，并由 `aether sync` 导入到 SQLite (`aether.db`) 中。一个启动时例外是 `global.yaml` 的 `packs` 列表：自动化和 `aether mcp` 直接读取同一条目，因此 Pack 身份和根无法在两个进程之间漂移。

## 同步管道
```
config/*.yaml, *.csv, *.json  →  aether sync  →  SQLite (aether.db)  →  services (at startup)
```

编辑 YAML 文件本身不会执行任何操作。脱机 `aether sync` 要求停止拥有配置的服务，在一个事务中写入所有所需状态头，并在下一次受监管服务启动时生效。在线通道、实例、路由和规则突变会输入其受管理的应用程序命令并自动协调其运行时预测。

`aether sync`（在 `tools/aether/src/core/syncer.rs` 中实现）在一个站点级 SQLite 事务中处理三个目标，因此任何目标发生故障都不会影响数据库：

- **global** — 将 `config/global.yaml` 解析为 `service_config` table.
- **aether-io** — 将 `config/io/io.yaml` 解析为 `channels` 表，并将每通道 CSV 文件解析为四个点表（`telemetry_points`、`signal_points`、`control_points`、`adjustment_points`）。重复的通道名称会中止同步。
- **aether-automation** — 将 `config/automation/automation.yaml`、`instances.yaml` 和 `rules/*.json` 解析到实例和规则表中，从 `instance_routing.csv` 导入测量 (`M`) 条目，并验证 `config/automation/products/` 下的任何外部产品 JSON 文件。没有独立的计算引擎同步路径 - 先前孤立的 `calculations.yaml` 模板、其未使用的表及其无效的 API 架构类型已被删除。派生数量改为用各个规则内的 `calculation` 节点表示（请参阅[作为规则的控制策略](https://github.com/EvanL1/AetherEdge/blob/main/docs/domain/control-strategies.md)）。

在编写之前，`aether sync` 会验证所有三个域。然后，它在一个 SQLite 事务中应用全局、IO 和自动化配置，因此稍后域中的错误会回滚所有早期更改。默认情况下，没有相应配置文件的行（例如通过 HTTP API 创建的规则）将被保留。对于 `--force`，托管表已被完全替换，但验证仍然是强制性的。操作 (`A`) 路由故意位于此兼容性导入器之外：它选择未来设备命令的物理目标，并且必须使用经过身份验证、确认、审核的操作路由应用命令。 `A` 行回滚整个同步。当任何操作路径存在时，`--force` 也会拒绝启动，因此它无法级联删除已委托的命令目标。在删除其实例、通道、控制点或调整点之前，通过受管路由 API 删除或迁移这些路由。测量路由仍然由同步管理。

两个相关命令很容易与同步混淆：

- `aether init` 仅初始化或升级 **数据库架构**（`CREATE TABLE IF NOT EXISTS`，仅迁移 - 它拒绝重置现有数据库）。它不会创建或复制任何配置文件。
- `config/` 目录本身在**部署时**搭建起来：Docker 安装程序 (`scripts/install.sh`) 将 `config.template/` 与二进制文件一起暂存，并仅在干净的主机上于 `<data-dir>/config/` 激活它。任何现有站点配置都会导致全新安装程序在写入之前失败。容器将新目录挂载在 `/app/config/` 处；安装程序不会合并、升级或导入操作员拥有的配置。在开发签出中，`aether setup` 仅计划并激活 `./data/config` 下的四个站点创作的安全文件，并在显式应用返回的计划 ID 后初始化 `./data/aether.db`。然后，开发人员必须提供如下所述的显式组合清单；安装程序永远不会猜测编译了哪些 IO 功能。

## 目录布局

仓库的 `config.template/` 目录是规范的故障安全起点。它不包含已委托的通道、设备实例或启用的控制规则。域示例是可选的；能源示例位于 `packs/energy/examples/config/` 下。注释：
```
config.template/
├── global.yaml                 # Shared settings: active Packs, API bind
│                               # host, log level/rotation, rule scheduler
│                               # tick interval (rules.tick_ms, default 100)
├── runtime-manifest.json       # Generated, checksummed build composition;
│                               # never inferred or edited by site setup
├── io/
│   ├── io.yaml                 # Empty channel list until commissioning
│   │                           # (modbus_tcp, modbus_rtu, can, mqtt, http,
│   │                           # di_do, ...), enabled flag, per-protocol
│   │                           # connection parameters, per-channel logging
│   └── <channel-id>/           # (expected by the syncer; not shipped in
│       │                       # the template) One directory per channel,
│       │                       # named by its numeric channel id (e.g. 1/)
│       ├── telemetry.csv       # T (telemetry) point definitions
│       ├── signal.csv          # S (signal) point definitions
│       ├── control.csv         # C (control) point definitions
│       ├── adjustment.csv      # A (adjustment) point definitions
│       └── mapping/            # Protocol register mappings, one CSV per
│                               # point type (telemetry_mapping.csv, ...)
└── automation/
    ├── automation.yaml         # Instance auto-load is disabled by default
    ├── instances.yaml          # Empty instance map until commissioning
    ├── instances/              # Optional per-instance directories, each
    │   └── <name>/instance.yaml  # holding one instance definition
    ├── rules/                  # One JSON file per control rule (Vue Flow
    │   └── *.json              # graph: nodes, edges, priority, enabled)
    └── products/               # (optional, not in the template) Site-owned
                                # product JSON files; when present they may
                                # override models from an active Pack
```

点类型简写：Aether 在其 API 和文件格式中使用 T（遥测）、S（信号）、C（控制）和 A（调整）作为四个点类。

`global.yaml` 中的故障安全默认值为 `packs: []`，因此新站点公开零域产品，并且没有 Pack 拥有的 MCP 知识。已安装的 Pack 通过一个身份绑定根激活：
```yaml
packs:
  - id: energy
    root: /opt/aether/packs/energy
```

清单身份必须匹配 `id`；兼容性、功能、协议、调试和资产限制检查都必须通过。相对 `root` 是从配置目录解析的，不能包含 `..`。如果 `automation.yaml` 设置 `products_path`，则该站点拥有的目录将最后加载，并且可能会故意覆盖活动 Pack 中的模型。运行时加载和 `aether sync` 都会拒绝符号链接、非常规/过大的 JSON、无效 JSON 以及一个目录中的重复产品名称。

`runtime-manifest.json` 在 `global.yaml` 旁边是强制性的。它由运行时组合或安装程序生成，而不是由 Pack 创作或由单个服务推断。封闭的 v1 文档记录了 Aether 版本、目标、包含的服务、精确的 `aether-io` 协议功能、派生适配器以及规范 SHA-256 校验和下的应用程序功能。在激活任何 Pack 之前，自动化和 MCP 会拒绝丢失、被篡改、版本不匹配、目标不匹配、未知、功能不一致、符号链接、非常规或过大的清单。对于明确的本地开发组合，请使用以下命令生成：
```bash
HOST_TARGET=$(rustc -vV | sed -n 's/^host: //p')
cargo run -p aether-runtime-catalog --bin aether-runtime-manifest -- \
  generate "$HOST_TARGET" data/config
```

将第三个逗号分隔的参数传递给 `generate` 以获得有意修剪的 IO 功能集；假设所有适配器都存在，则没有后备方案。使用 `aether runtime-manifest`（或 `--path <artifact>`）运行安装程序、自动化和 MCP 使用的相同验证程序。

## 环境变量

Docker Compose 和服务使用的关键变量（大多数可选值在 `.env.example` 中说明；部署覆盖添加所需的生产gates):

| 变量 | 默认 | 用途 |
|----------|---------|---------|
| `AETHER_BASE_PATH` | `./data` | 站点配置和数据库的基本路径；日志使用容器进程的`AETHER_LOG_PATH` |
| `HOST_UID` | `1000` | 用户 ID；必须与主机用户匹配以避免文件权限问题 |
| `HOST_GID` | `1000` | 容器进程的组 ID；与 `HOST_UID` |
| `DIALOUT_GID` | `20` | 串行端口访问的拨出组 ID 配对（仅限 Linux） |
| `INFLUXDB_URL`、`INFLUXDB_ORG`、`INFLUXDB_BUCKET`、`INFLUXDB_TOKEN`、 `INFLUXDB_PASSWORD` | 取消设置 | 仅限可选的 InfluxDB 历史记录适配器；未由默认运行时 |
| `AETHER_IO_URL` | `http://127.0.0.1:6001` | API 网关和 `aether` 的 io 基本 URL 未使用CLI |
| `AETHER_AUTOMATION_URL` | `http://127.0.0.1:6002` | API 网关和 `aether` 的自动化基本 URL CLI |
| `AETHER_SHM_PATH` | 平台选择的 tmpfs 路径 | io 和只读消费者共享的规范权威点状态段 |
| `AETHER_CHANNEL_HEALTH_SHM_PATH` | 同级 `*-health` 路径 | 单独的权威通道连接段；通常源自 `AETHER_SHM_PATH` |
| `SHM_WRITER_STALE_AFTER_MS` | `30000` | 读取端 SHM 适配器接受的最大写入心跳期限 |
| `SHM_IDENTITY_CHECK_INTERVAL_MS` | `250` | 用于检查规范 SHM inode 是否已替换的回退间隔；生成防护立即处理正常交换 |
| `SHM_TOPOLOGY_REFRESH_INTERVAL_MS` | `1000`（最小 `100`） | API、警报和自动化用于重新加载一个 SQLite 拓扑快照并以原子方式发布经过验证的点/运行状况/路由的时间间隔生成 |
| `JWT_SECRET_KEY` | 取消设置（必需） | 用于 aether-api 以及受控 io、自动化和警报操作的共享 32 字节或更长的访问 JWT 签名/验证密钥；安装程序生成它并将其保留在配置资产之外 |
| `AETHER_ACCESS_TOKEN` | 未设置 | 受治理的 CLI 通道调试/生命周期、设备命令、操作路由更改和自动化/警报策略操作（包括 MCP 的 22 个受治理写入工具）所需的签名管理员/工程师访问 JWT；查询命令在本地接口上不需要它 |
| `AETHER_UPLINK_CONTROL_TOKEN` | 未设置 | 单独的 32 字节或更长的服务凭据，仅用于上行链路到自动化设备命令；安装程序生成它并且从不打印它 |
| `AETHER_ALLOW_SIMULATION_WRITES` | `false` | 仅开发选择将 io T/S 模拟写入权威 SHM；在生产环境中保持禁用状态 |
| `AETHER_CONFIG_PATH` | 未设置 | 自动化和 `aether mcp` 使用的共享配置目录；CLI 路径解析可以通过部署上下文或 `--config-path` 覆盖它 |
| `AETHER_DATA_PATH` | 未设置 | 覆盖 `aether` CLI 安装上下文中的数据目录 |
| `AETHER_INSTALL_CONTEXT_PATH` | `/etc/aether/install.yaml` | 覆盖已安装的布局描述符； CLI 标志和两个路径变量优先 |
| `AETHER_BOOTSTRAP_ADMIN_PASSWORD` | 取消设置 | 仅当 `users` 为空时才需要；安装程序会在其 mode-0600 环境文件中生成一个强值，并且应在第一次更改密码后将其删除 |
| `AETHER_ALLOW_PUBLIC_REGISTRATION` | `false` | 显式选择加入匿名查看者注册；管理员创建永远无法通过公共注册 |
| `AETHER_DATA_PROCESSING_ENABLED` | `false` | 显式启用选择加入的数据处理应用程序和 HTTP 路由；如果启用的配置无效，则启动失败关闭 |
| `AETHER_DATA_PROCESSING_CONFIG` | `/app/data/config/data-processing/runtime.yaml` | 包含委托任务、绑定、历史记录、协变量、处理器和审核组合的严格运行时 YAML |
| `AETHER_LOAD_FORECASTING_BEARER_TOKEN` | 未设置 | 供 `aether-api` 验证负载预测伴生服务身份的共享部署密钥；生产部署必须覆盖开发值 |
| `AETHER_LOAD_FORECASTING_REQUIRE_AUTH` | `false`所需 | 处理器端启动门；生产覆盖将其修复为 `true` |
| `AETHER_LOAD_FORECASTING_MAX_CONCURRENCY` | `1` | 限制占用的模型执行槽；在后台工作实际完成之前，取消不会释放插槽 |
| `AETHER_LOAD_FORECASTING_ARTIFACT_BUNDLES` | 取消设置 | 严格的 JSON 数组固定每个实际委托的模型/缩放器/配置工件；生产准备就绪所需 |
| `AETHER_LOAD_FORECASTING_IMAGE` | 可变本地开发映像 | 生产必须通过显式 Compose 覆盖和预检验证器使用不可变 `@sha256` 映像引用 |
| `AETHER_LOAD_FORECASTING_PORT` | `8989` | Compose 发布到主机环回地址的处理器伴生服务端口 |
| `RUST_LOG` | `info` | Rust 服务的日志级别；支持过滤器语法，例如 `info,io=debug,automation=trace` |

### 实验性 Home Assistant 桥接设置

只有使用 `home-assistant` 构建特性从源码编译的 `aether-io` 才会读取这些设置。它们不代表
安装器已经支持生产部署。

| 环境变量 | 默认值 | 用途 |
|---|---|---|
| `AETHER_HOME_ASSISTANT_ENABLED` | `false` | 显式启用实验性的只读桥接扩展 |
| `AETHER_HOME_ASSISTANT_ORIGIN` | 未设置 | 必填的 HTTP(S) 站点根地址，不能包含凭据、路径、查询参数或片段 |
| `AETHER_HOME_ASSISTANT_ACCESS_TOKEN_REF` | 未设置 | 必填的 `env:变量名` 引用；令牌原文保存在普通配置之外 |
| `AETHER_GATEWAY_ID` | 未设置 | 必填的所属边缘网关标识 |
| `AETHER_HOME_ASSISTANT_INTEGRATION_ID` | `home-assistant` | 这条 Home Assistant 连接的稳定标识 |
| `AETHER_HOME_ASSISTANT_GENERATION_STORE_PATH` | 未设置 | 必填的绝对文件路径，用于保存可跨重启恢复且由进程独占锁定的拓扑世代号账本 |

系统禁止使用 `AETHER_HOME_ASSISTANT_ACCESS_TOKEN`，因为它会把凭据原文放入普通配置。完整的
源码构建和存储约束见[接入 Home Assistant](/guides/home-assistant)。

以下变量启用独立且默认关闭的只读 CloudLink 发布路径。只有包含
`home-assistant-cloudlink` 构建特性的二进制文件才会接受这组配置。

| 环境变量 | 默认值 | 用途 |
|---|---|---|
| `AETHER_HOME_ASSISTANT_CLOUDLINK_ENABLED` | `false` | 显式启用发布；只启用 Home Assistant 不会自动发布 |
| `AETHER_HOME_ASSISTANT_CLOUDLINK_RUNTIME_CONFIG_DIR` | 未设置 | 包含经过校验的 `runtime-manifest.json` 的绝对目录 |
| `AETHER_HOME_ASSISTANT_CLOUDLINK_CLOUD_EXTENSION` | 未设置 | 必须等于 `aether.cloudlink.integration.v1alpha1` |
| `AETHER_HOME_ASSISTANT_CLOUDLINK_TOPOLOGY_SPOOL_PATH` | 未设置 | 拓扑持久队列的绝对文件路径 |
| `AETHER_HOME_ASSISTANT_CLOUDLINK_OBSERVATION_SPOOL_PATH` | 未设置 | 与拓扑队列不同的观测持久队列绝对路径 |
| `AETHER_HOME_ASSISTANT_CLOUDLINK_SPOOL_CAPACITY` | `4096` | 每条流可保留的记录上限，允许范围为 1 至 65536 |
| `AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_BROKER_HOST` | 未设置 | 不带网址格式的 TLS MQTT 主机名或地址 |
| `AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_BROKER_PORT` | 未设置 | 必填的非零 MQTT TLS 端口 |
| `AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_CLIENT_ID` | 未设置 | 长度受限且稳定的消息代理客户端标识 |
| `AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_TOPIC_PREFIX` | 未设置 | 安全的主题前缀；系统拒绝通配符 |
| `AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_USERNAME` | 未设置 | 必填的消息代理认证主体 |
| `AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_PASSWORD_REF` | 未设置 | 必填的 `env:变量名` 消息代理密码引用 |
| `AETHER_HOME_ASSISTANT_CLOUDLINK_CREDENTIAL_ID` | 未设置 | 非敏感的 CloudLink 连接凭据标识 |
| `AETHER_HOME_ASSISTANT_CLOUDLINK_CREDENTIAL_GENERATION` | 未设置 | 必填的正数凭据代次 |
| `AETHER_HOME_ASSISTANT_CLOUDLINK_SESSION_EPOCH_PATH` | 未设置 | 单调递增会话代次检查点的绝对文件路径 |

系统禁止使用 `AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_PASSWORD`。MQTT 发布确认不会删除拓扑
或观测记录，只有严格的 CloudLink 应用确认可以删除。

以下变量启用实验性的受治理开关控制。二进制文件必须包含
`home-assistant-integration-control` 构建特性；Home Assistant、CloudLink 和控制三个
启用开关必须分别显式设为真；运行时清单还必须同时声明只读集成协议和集成控制协议。

| 环境变量 | 默认值 | 用途 |
|---|---|---|
| `AETHER_HOME_ASSISTANT_CONTROL_ENABLED` | `false` | 显式启用受治理控制；不会顺带启用 Home Assistant 或 CloudLink |
| `AETHER_HOME_ASSISTANT_CONTROL_CLOUD_EXTENSION` | 未设置 | 必须等于 `aether.cloudlink.integration-control.v1alpha1` |
| `AETHER_HOME_ASSISTANT_CONTROL_LEDGER_PATH` | 未设置 | 由进程独占的持久作业与回执账本绝对路径 |
| `AETHER_HOME_ASSISTANT_CONTROL_POLICY_PATH` | 未设置 | 默认拒绝且结构封闭的本地授权策略绝对路径 |
| `AETHER_HOME_ASSISTANT_CONTROL_AUDIT_PATH` | 未设置 | 与前两项不同的追加式本地审计文件绝对路径 |
| `AETHER_HOME_ASSISTANT_CONTROL_CLOUD_KEY_ID` | 未设置 | 受信任云端 Ed25519 验签公钥的精确标识 |
| `AETHER_HOME_ASSISTANT_CONTROL_CLOUD_PUBLIC_KEY_REF` | 未设置 | 指向规范无填充 Base64url 编码的 32 字节 Ed25519 公钥的 `env:变量名` 引用 |
| `AETHER_HOME_ASSISTANT_CONTROL_EDGE_KEY_ID` | 未设置 | 边缘 Ed25519 回执签名密钥标识 |
| `AETHER_HOME_ASSISTANT_CONTROL_EDGE_SIGNING_KEY_REF` | 未设置 | 指向规范无填充 Base64url 编码的 32 字节私钥种子的 `env:变量名` 引用 |
| `AETHER_HOME_ASSISTANT_CONTROL_PROVIDER_TIMEOUT_MS` | `5000` | 提供方调用期限，允许范围为 1 至 30000 毫秒 |

账本、策略和审计路径必须彼此不同。在 Unix 系统上，账本、审计和锁文件不能向组用户或
其他用户开放权限；新建敏感文件使用 0600。系统拒绝符号链接文件和作为直接父目录的符号
链接。MQTT 发布确认绝不会删除控制回执。策略格式和执行边界见
[接入 Home Assistant](/guides/home-assistant#实验性受治理开关控制)。

### 实验性 CloudLink MQTT 设置

当前的 `aether-uplink` 生产组合仍处于已弃用的 `legacy` 模式。实验性的 `aether-cloudlink-mqtt` 嵌入 API 公开了显式的 `legacy`、`cloudlink-v1` 和 `dual` 迁移值；它不会在现有安装中静默启用 CloudLink。第一个真实消息代理垂直切片是下面的可选测试工具。这些变量仅在 `AETHER_CLOUDLINK_RUN_INTEGRATION=1` 时读取：

| 变量 | 默认 | 用途 |
|---|---|---|
| `AETHER_CLOUDLINK_RUN_INTEGRATION` | 未设置 | 精确设置为 `1` 时运行外部消息代理测试工具 |
| `AETHER_CLOUDLINK_BROKER_HOST` | `127.0.0.1` | 用户选择的 MQTT 代理主机名/IP |
| `AETHER_CLOUDLINK_BROKER_PORT` | `1883` | 用户选择的代理端口 |
| `AETHER_CLOUDLINK_BROKER_USERNAME` | 取消设置 | 可选代理用户名 |
| `AETHER_CLOUDLINK_BROKER_PASSWORD` | 取消设置 | 可选的只写代理密码；从未打印或序列化 |
| `AETHER_CLOUDLINK_BROKER_TLS` | 未设置 | 设置为 `1` 时使用平台 TLS 根证书 |
| `AETHER_CLOUDLINK_BROKER_CA` | 未设置 | 自定义 PEM CA 路径；设置后启用自定义 TLS 信任 |
| `AETHER_CLOUDLINK_BROKER_CLIENT_CERT` | 取消设置 | 可选 mTLS 客户端证书，使用密钥配置 |
| `AETHER_CLOUDLINK_BROKER_CLIENT_KEY` | 未设置 | 可选的 mTLS PKCS#8 私钥，与客户端证书配套使用 |
| `AETHERCLOUD_ROOT` | 未设置 | 供边缘测试工具之外的联合编排使用的可选只读路径；测试不会修改或启动它 |

纯文本只允许在显式开发工具中使用，生产环境必须启用 TLS。实验性 CloudLink 配置固定使用 MQTT v3.1.1、QoS 1、非保留消息和精确的单网关主题；MQTT 5 仍然是可选项，不能成为正确运行的前提。

对于 MCP 写入，`--allow-write` 仅注册 22 工具写入允许列表。网桥发送 `AETHER_ACCESS_TOKEN` 作为 `Authorization: Bearer` 凭证并添加 `X-Request-ID`；每次调用仍需要 `confirmed: true`。保留返回的请求/命令 ID，并且不会自动重试超时或不完整的审核/发布结果。通道突变还会返回所需状态的修订，并且可能会在运行时投影降级的情况下成功；检查 `request_id`、`resulting_revision` 和 `reconciliation_required`，而不是自动重试。

### 数据处理和历史记录存储更改

数据处理运行时的 `history.path` 必须命名正在运行的历史记录实际写入的 SQLite 文件。 `history_config.storage_*` 下的值是保留的所需设置。特别是，`PUT /hisApi/storage` 保存它们但不重新连接活动后端，因此匹配这些行并不足以证明实时编写器。仅在禁用数据处理的情况下更改存储；重新连接或重新启动 `aether-history`，验证其活动后端/运行状况和已委托的哨兵系列，然后使用匹配的运行时路径重新启动 `aether-api`。

API 还需要对历史数据库/WAL/SHM 目录具有独立的只读操作系统权限。将该路径与 API 的可写配置/审核数据库分开。基础 Compose `/app/data:rw` 装载上的 SQLite `mode=ro` 不是完整的生产权限边界。

## 相关页面

- [入门](/guides/getting-started) — 首次设置和启动演练
- [连接设备](/guides/connect-devices) — 实践中的通道和点配置
- [写作Rules](/guides/writing-rules) — 位于 `automation/rules/`
- [HTTP API](/reference/http-api) 下的规则 JSON — 运行时 API 同步的配置源
- [系统架构](/concepts/architecture) — 每个服务适合的位置
