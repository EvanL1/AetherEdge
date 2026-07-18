---
title: "部署"
description: "使用 Docker Compose 运行或为边缘设备构建独立的安装程序"
updated: 2026-07-17
---

# 部署

Aether 部署为一组 Docker 容器，或者对于无 Docker 的目标，部署为原生 systemd 服务。有三种路径：直接在可以构建映像的计算机上运行 Docker Compose 堆栈，将所有内容打包到一个基于 Docker 的自解压安装程序中并将其传送到边缘设备，或者构建一个裸机安装程序来传送静态链接的二进制文件和 systemd 单元。

## Docker Compose
```bash
cp .env.example .env    # then edit: AETHER_BASE_PATH, HOST_UID/HOST_GID, RUST_LOG, ...

docker compose up -d
docker compose ps
```

默认 Compose 应用程序仅启动六个 Rust 服务，全部带有 `network_mode: host`。Redis 和 TimescaleDB 仅在显式选择 `redis` 和 `postgres-storage` 配置文件时启动。基本文件只通过可变的 `data-processing-dev` 配置文件公开预测伴生服务；生产环境需要显式覆盖，如下所示。

AetherEdge 是无头内核发行版。它不构建或安装浏览器客户端。 EMS 操作员控制台属于独立的 [AetherEMS](https://github.com/EvanL1/AetherEMS) 发行版，并通过经过身份验证的应用程序 API 连接。

| 容器 | 镜像 | 职责 |
|-----------|-------|------|
| aether-redis | redis:8-alpine | 可选非权威状态镜像基础设施（`redis` 配置文件） |
| aether-timescaledb | timescale/timescaledb:2.25.2-pg17 | 可选 PostgreSQL 历史记录后端 (`postgres-storage`配置文件） |
| aether-load-forecasting-processor | 操作员提供的、摘要固定的图像 | 可选的请求驱动处理器（`data-processing` 配置文件） |
| aether-io | aetherems:latest | 通信服务（特权、挂载） `/dev` 用于现场总线） |
| aether-automation | aetherems:latest | 模型服务和规则引擎 |
| aether-history | aetherems:latest | 具有嵌入式 SQLite 历史记录的 SHM 采样器默认 |
| aether-api | aetherems:latest | REST API、WebSocket、JWT 身份验证 |
| aether-uplink | aetherems:latest | MQTT 云上行链路、TLS证书 |
| aether-alarm | aetherems:latest | 警报规则和通知 |

生产预测配置文件需要从现有负载预测服务构建的不可变映像以及 Aether 的适配器、委托工件包、匹配的承载令牌以及 `${AETHER_BASE_PATH}/config/data-processing/runtime.yaml` 下经过验证的运行时 YAML：

复制将仓库的合成 [`runtime.example.yaml`](https://github.com/EvanL1/AetherEdge/blob/main/packs/energy/data-processing/runtime.example.yaml) 和 [`covariates.example.json`](https://github.com/EvanL1/AetherEdge/blob/main/packs/energy/data-processing/covariates.example.json) 复制到该部署拥有的目录中，替换每个逻辑/物理映射、工件摘要和协变量行，并根据站点数据库验证它们。这些示例不是生产值。
```bash
export AETHER_LOAD_FORECASTING_IMAGE=registry.example/load-forecasting@sha256:<digest>
export AETHER_LOAD_FORECASTING_BEARER_TOKEN='<unique secret>'
export AETHER_LOAD_FORECASTING_ARTIFACT_BUNDLES='<commissioned JSON array>'
integrations/load-forecasting/deploy/validate-production-env.sh
docker compose \
  -f docker-compose.yml \
  -f integrations/load-forecasting/deploy/docker-compose.data-processing.yaml \
  --profile data-processing \
  up -d aether-load-forecasting-processor aether-api
```

对于此记录的生产路径，预检是强制性的。在 Compose 评估覆盖之前，它会拒绝非`@sha256`图像引用、弱或格式错误的令牌、超出范围的并发性以及非严格的工件捆绑 JSON。

历史记录权限需要单独的操作检查。存储`PUT /hisApi/storage`保存设置但不重新连接活动写入器。在存储更改期间保持数据处理处于禁用状态，重新连接或重新启动 `aether-history`，验证其活动 SQLite 后端和委托的哨兵系列，然后使用与所应用的后端匹配的运行时 `history.path` 重新启动 `aether-api`。仅保留的 `history_config.storage_*` 行并不能证明实时写入目标。

直接 SQLite 历史记录还需要文件系统边界。 API 必须通过专用只读目录挂载（或独立许可的只读操作系统帐户/ACL）接收历史数据库、WAL 和 SHM；它自己的 `aether.db` 和审核写入保留在单独的可写路径上。基础 Compose 当前将整个 `/app/data` 目录读写安装到 `aether-api` 中，因此记录的生产数据处理路由将被阻止，直到站点覆盖提供并验证该分离。仅 SQLite `mode=ro` 和 `query_only=ON` 不足以遏制。

`/api/v1/data-processing/process` 调用是非幂等的，即使工作被拒绝，也会写入强制审核记录。当前的 API 不提供参与者/IP 请求速率限制器或审核保留配额。因此，生产入口必须强制执行经过身份验证的参与者和源 IP 速率以及运行中的上限，而运营则监控 `command_audit_events` 增长并应用保留所需证据的保留/导出策略。如果没有这些控制，生产支持就会受到阻碍；每个路由处理器信号量本身不会绑定拒绝调用审核写入。

它绑定到环回并且不接收 Aether 数据目录、配置、设备、历史数据库或 SHM 安装。应用程序通过处理器端口发送完整的、有界的 `ProcessingFrame`。请参阅 [`../../integrations/load-forecasting/deploy/README.md`](https://github.com/EvanL1/AetherEdge/blob/main/integrations/load-forecasting/deploy/README.md) 了解独立 systemd 单元和调试要求。

Compose 伴生服务仅加入专用的 `data-processing-local` 网络，该网络声明为 `internal: true`；配合主机环回端口发布，可以阻止容器访问外部网络并限制入站访问。本机或 systemd 部署仍然需要主机防火墙或等效的出口策略。这些示例还保留了部署专用的 CPU、内存和 PID 配额；应根据实际工件基准设置 cgroup/systemd 限制，避免处理器负载耗尽确定性服务所需的资源。

六个 Rust 服务共享一个 `aetherems:latest` 兼容性映像，每个服务都以自己的命令启动。当下游发布消费者迁移时，镜像名称被保留；它并不意味着此仓库拥有 EMS 产品或控制台。

主机网络不会将未经身份验证的进程公开：IO、自动化、历史记录、上行链路和警报仅绑定到 `127.0.0.1`。远程客户端必须通过端口 6005 上的 `aether-api` 进入，该端口适用 JWT 和角色检查。可选的 Redis 和 TimescaleDB 侦听器也是仅环回的。

两个挂载类对于运行时很重要：

- **共享内存和本地事件套接字** - 主机的 `/dev/shm` 在所有六个 Rust 服务中绑定挂载在 `/shm/rtdb` 处。挂载是可读写的，因为 SHM 所有者写入点槽，而隔离的消费者在段旁边创建自己的订阅位图和 UDS 端点。挂载目录还可以避免 Docker 自动创建过时的文件条目。
- **可选的外部存储** — 没有核心服务挂载 Redis 套接字、导出 `REDIS_URL` 或等待 Redis。 `docker compose --profile redis up -d` 为显式连接 `aether-redis-bridge` 的主机启动镜像基础设施。 PostgreSQL 历史记录仍然通过 `--profile postgres-storage` 和启用 PostgreSQL 的历史记录构建选择加入。在选择该配置文件之前设置一个唯一的非空 `TIMESCALEDB_PASSWORD`；打包的扩展安装程序会生成一个而不打印它。

所有 Rust 容器都从 `${AETHER_BASE_PATH:-./data}/aether.db`（安装在 `/app/data/aether.db`）读取共享配置 SQLite 数据库，并将日志写入 `${AETHER_LOG_PATH:-./logs}`。 aether-history 将样本存储在 `/app/data/aether-history.db` 中，除非明确选择启用 PostgreSQL 的构建和后端配置。

服务仍然是六个独立的进程。 SHM/UDS 取代了强制性的实时数据代理；它不会折叠其重新启动或故障隔离边界。

## 边缘安装程序

`scripts/build-installer.sh` 会生成一个自解压 `.run` 文件，其中包含离线边缘设备所需的所有内容 — Docker 映像存档、撰写文件、配置模板、`aether` CLI 二进制文件和安装脚本：
```text
./scripts/build-installer.sh [VERSION] [ARCH] [TARGET] [--services=...] [--io-features=...] [--enable-swagger]
```

- `VERSION` — 版本字符串，默认为今天的日期 (`YYYYMMDD`)
- `ARCH` — `arm64`（默认）或 `amd64`
- `TARGET` — Rust 目标三元组；对于arm64，默认为`aarch64-unknown-linux-musl`；对于amd64，默认为`x86_64-unknown-linux-musl`
- `--services` / `-s` - 要包含的以逗号分隔的子集（服务名称：`aether-io`、`aether-automation`、`aether-history`、`aether-api`、`aether-uplink`、 `aether-alarm`、`redis`、`timescaledb` 组快捷方式 `rust` 扩展到所有六个 Rust 服务）。每个全新安装的包都必须包含 Rust 核心；选择扩展变体：`-s rust,redis`、`-s rust,timescaledb` 或 `-s rust,redis,timescaledb`。默认包仅包含 Rust 边缘运行时映像；必须显式选择外部存储图像。
- `--enable-swagger` — 编译 Rust 服务并启用其功能门控 Swagger UI
- `--io-features` — 用显式的逗号分隔列表替换 `aether-io` 默认特性。
  构建器会拒绝未知特性，只展开一次依赖，并将同一份规范化结果同时用于
  二进制文件和安装包中的 `runtime-manifest.json`。
```bash
# Full installer for an ARM64 edge device
./scripts/build-installer.sh

# All Rust services only, with Swagger UI
./scripts/build-installer.sh v1.2.0 arm64 -s rust --enable-swagger

# 包含本地只读 Home Assistant 桥接的 Docker 安装包
./scripts/build-installer.sh v1.2.0 arm64 \
  --io-features=home-assistant

# 包含 Home Assistant、CloudLink 和受治理电源控制的 Docker 安装包
./scripts/build-installer.sh v1.2.0 arm64 \
  --io-features=home-assistant-integration-control
```

该脚本将六个服务和 `aether` CLI 与 `cargo zigbuild` 交叉编译为目标三元组，从这些二进制文件构建 `aetherems` Docker 映像，使用 `docker save` 保存映像（选择时还包括 Redis 和 TimescaleDB 映像），并将结果与 `makeself` 一起打包到 `release/AetherEdge-<arch>-<version>.run` 中（通过以下方式构建子集） `--services` 将服务列表后缀附加到文件名，`--enable-swagger` 附加 `-swagger`）。构建主机需要 Docker、`cargo-zigbuild`（如果缺少，则通过 `cargo install` 自动安装）和 `makeself`（在 macOS 上通过 Homebrew 自动安装）。

交付并运行：
```bash
scp release/AetherEdge-arm64-<version>.run root@192.168.30.21:/tmp/
ssh root@192.168.30.21 'chmod +x /tmp/AetherEdge-arm64-<version>.run && /tmp/AetherEdge-arm64-<version>.run'
```

嵌入式安装程序仅支持**全新部署**。它的第一步是只读预检：如果它找到 Aether 安装根、安装上下文、站点配置或数据库、Aether 容器或 Aether systemd 单元，它会在停止服务、加载映像或写入文件之前退出。在接受的干净主机上，它安装到 `/opt/AetherEdge`，使用 `docker load` 加载捆绑的映像，激活 `/opt/AetherEdge/data/config` 处的故障安全模板，记录 `/etc/aether/install.yaml` 中的布局，初始化新数据库，并使用 Docker Compose 启动六个容器。该部署基于 Docker - 安装程序提供映像和组成配置，而不是独立的服务二进制文件。

此版本不支持就地升级、回滚到旧版本以及导入旧数据库或安装布局。要替换安装，请首先导出并备份必须保留的所有内容，运行特定于部署的卸载过程，并在调用新安装程序之前手动重新定位或删除每个保留的 Aether 足迹。目前，将保留的数据转换为新版本是由操作员在安装程序之外管理的迁移；不要将新安装程序指向旧站点目录。

`/opt/AetherEdge` 已针对此版本进行了有意修复，因为打包的服务管理路径假定组合根目录。安装程序会拒绝 `AETHER_INSTALL_DIR` 覆盖，而不是完成其后续生命周期操作将针对不同根目录的安装。 `AETHER_BASE_PATH` 可以将**新的、空的**数据/配置树放置在另一个磁盘上的专用子目录中，但必须在安装前选择它，并且它不是迁移开关。在进行任何递归权限操作之前，安装程序会拒绝 `/`、系统根目录、通用挂载根目录、符号链接路径、安装根目录以及任何包含 Aether 站点的目标。路径也仅限于通过 Docker Compose `.env` 安全往返的字符。

站点根外部的 `AETHER_TIMESCALE_DATA_PATH` 和 Docker 的可选 `redis-data` 命名卷是扩展拥有的存储。它们还必须是空的才能进行新的部署。重用或迁移扩展存储超出了安装程序支持的工作流程。

安装程序生成 `AETHER_BOOTSTRAP_ADMIN_PASSWORD`，仅将其保留在模式 0600 `.env` 中，并且从不打印该值。完成消息提供本地检索命令。以 `admin` 身份登录，立即更改密码，然后删除引导变量。除非明确设置 `AETHER_ALLOW_PUBLIC_REGISTRATION=true`，否则匿名注册将保持禁用状态。

API 容器作为 `HOST_UID:HOST_GID` 运行。它既没有安装 Docker 套接字，也没有安装安装根目录，并且 `/etc/systemd/network` 是只读的。因此，主机网络突变请求无法关闭。不支持远程运行时升级；安装另一个版本需要上面的显式全新部署工作流程，而不是扩展 API 进程的权限。

### Docker 安装包中的 Home Assistant

只有使用上述 `--io-features` 选项构建的安装包才包含 Home Assistant。
当前官方发布工作流没有传入这个选项，因此官方预编译 `.run` 安装包尚未包含
Home Assistant。不要在默认官方安装包上开启下列配置；运行时会因为缺少编译
特性而拒绝启动该集成。

安装包中的 Compose 文件只向 `aether-io` 显式传递文档列出的变量，不使用
`env_file` 把宿主机完整环境导入容器。三个开关默认都是 `false`：

```dotenv
AETHER_HOME_ASSISTANT_ENABLED=false
AETHER_HOME_ASSISTANT_CLOUDLINK_ENABLED=false
AETHER_HOME_ASSISTANT_CONTROL_ENABLED=false
```

安装自定义构建后，编辑 `/opt/AetherEdge/.env`，并保持文件权限为 0600。
只开启已经完成调试的层级。本地只读桥接至少需要站点地址、网关身份和令牌值：

```dotenv
AETHER_HOME_ASSISTANT_ENABLED=true
AETHER_HOME_ASSISTANT_ORIGIN=https://homeassistant.example.lan:8123
AETHER_GATEWAY_ID=33333333-3333-4333-8333-333333333333
AETHER_HOME_ASSISTANT_INTEGRATION_ID=home-assistant-main
AETHER_HOME_ASSISTANT_TOKEN=<访问令牌>
```

Compose 将每个 `*_REF` 固定到一个公开约定的值变量，并单独传递实际值：

| `aether-io` 读取的用途或引用 | `.env` 中的值变量 |
|---|---|
| `env:AETHER_HOME_ASSISTANT_TOKEN` | `AETHER_HOME_ASSISTANT_TOKEN` |
| `env:AETHER_HOME_ASSISTANT_CLOUDLINK_CLOUD_PUBLIC_KEY` | `AETHER_HOME_ASSISTANT_CLOUDLINK_CLOUD_PUBLIC_KEY` |
| `env:AETHER_HOME_ASSISTANT_CLOUDLINK_GATEWAY_SIGNING_KEY` | `AETHER_HOME_ASSISTANT_CLOUDLINK_GATEWAY_SIGNING_KEY` |
| `env:AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_PASSWORD_SECRET` | `AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_PASSWORD_SECRET` |
| `env:AETHER_HOME_ASSISTANT_CONTROL_CLOUD_PUBLIC_KEY` | `AETHER_HOME_ASSISTANT_CONTROL_CLOUD_PUBLIC_KEY` |
| 控制回执签名 | 使用当前 CloudLink 网关会话签名器，不注入第二个边缘身份、引用或私钥变量 |

Compose 不注入已废弃的迁移别名
`AETHER_HOME_ASSISTANT_CONTROL_EDGE_KEY_ID` 和
`AETHER_HOME_ASSISTANT_CONTROL_EDGE_SIGNING_KEY_REF`。

不要添加 `AETHER_HOME_ASSISTANT_ACCESS_TOKEN` 或
`AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_PASSWORD`；运行时明确拒绝这两个明文配置名。
CloudLink 与控制层还需要 `.env.example` 中对应的非机密身份、代理设置、扩展确认
和显式开启开关。

Compose 的可变状态路径全部固定在现有持久化挂载 `/app/data` 内。与镜像特性
精确对应的运行时清单是唯一的只读例外：

| 状态 | 容器内路径 |
|---|---|
| 拓扑代次账本 | `/app/data/home-assistant/topology-generations.json` |
| 已验证运行时清单目录 | `/app/config`（只读，固化在对应镜像内） |
| 挑战重放账本 | `/app/data/home-assistant/cloudlink/challenge-ledger.json` |
| 拓扑与观测暂存日志 | `/app/data/home-assistant/cloudlink/topology.spool`、`/app/data/home-assistant/cloudlink/observations.spool` |
| 会话代次 | `/app/data/home-assistant/cloudlink/session-epoch` |
| 控制账本、策略与审计 | `/app/data/home-assistant/control/jobs-and-receipts.json`、`/app/data/home-assistant/control/policy.json`、`/app/data/home-assistant/control/audit.jsonl` |

开启控制前，必须先创建并调试控制策略。随后只重启拥有该集成的组合根：

```bash
cd /opt/AetherEdge
chmod 600 .env
docker compose up -d --force-recreate aether-io
```

CloudLink 故障不会关闭本地 Home Assistant 投影；任何可选路径都不会取代已调试
原生边缘采集与安全行为的权威地位。

## 仅包工件

域 Pack 与全新安装 `.run` 包分开发布。 Pack 捆绑包仅包含 `pack-artifact.json` 和声明性 `pack/` 树，而不包含 `aether`、CLI、服务二进制文件或核心 crate。从为目标内核组合生成的确切运行时清单构建它：
```bash
./scripts/build-pack-artifact.sh \
  packs/<pack-id> \
  build/installer/runtime/runtime-manifest.json \
  release/<pack-id>.bundle
```

将该目录复制到已具有匹配内核的边缘主机，然后使用主机的 CLI 进行安装：
```bash
aether packs install --artifact /tmp/<pack-id>.bundle
```

该命令拒绝不同的内核版本、目标三元组或完整的运行时清单摘要。它还拒绝额外的顶级条目、符号链接、可执行文件/源代码树、有效负载篡改、无限文件和不兼容的 `pack.yaml`。验证后，它会将已安装数据目录下的数据发布为 `packs/<id>/<version>`，并仅在验证完整的候选活动 Pack 集后自动替换 `global.yaml`。激活失败将保留以前的配置并删除新发布的版本。

此命令不会重新启动服务或调试 Pack。单独计划任何维护重启，然后运行`aether doctor`；启用通道、实例、规则、处理器或物理控制仍然是一个独特的经过审计的调试操作。仓库可以构建和测试此本地格式，但尚未声明独立发布/签名的内核工件、Pack 工件或下游第二仓库发布入口。

## 裸机 Linux (systemd)

对于无法或不应运行 Docker 的边缘设备，`scripts/build-installer.sh --bare-metal` 生成第二种 `.run` 软件包：静态链接二进制文件和 systemd 单元的独立捆绑包，具有零容器对目标机器的运行时依赖。它包含六个 Rust 服务、`aether` CLI 和核心 systemd 单元。仅当选择 Redis 时，才会包含静态 `redis-server`/`redis-cli` 及其单位。 `scripts/build-static-deps.sh` 使用 `INCLUDE_REDIS=1` 作为该扩展包。核心服务按 `aether.target` 分组。固定的 Redis 版本还固定其源存档 SHA-256 值。覆盖版本需要其匹配的 `REDIS_SHA256`；缓存的二进制文件仅在具有匹配的出处标记并检查其静态 ELF 链接和目标架构后才会被重用。

裸机运行时根目录同样固定在 `/opt/aether`，与打包的 systemd 单元相匹配。 `AETHER_INSTALL_DIR` 覆盖被拒绝。其引导管理员凭据存储在 `/etc/aether/aether.env`（模式 0600）中，具有与 Docker 相同的检索-更改-删除生命周期。

构建：
```bash
# Core-only package (default)
./scripts/build-installer.sh --bare-metal [VERSION] [ARCH]

# Core plus optional Redis mirror infrastructure
./scripts/build-installer.sh --bare-metal [VERSION] [ARCH] -s rust,redis

# Home Assistant、CloudLink 与受治理电源控制
./scripts/build-installer.sh --bare-metal v1.2.0 arm64 \
  --io-features=home-assistant-integration-control
```

这遵循与 Docker 构建相同的 `[VERSION] [ARCH] [TARGET]` 位置约定 - `--bare-metal` 是一个添加的标志，其他参数的顺序保持不变。它交叉编译相同的六种服务以及 `aether` CLI，并将它们与 `makeself` 一起打包到 `release/AetherEdge-baremetal-<arch>-<version>.run` 中。选择 Redis 将 `-redis` 添加到文件名中。裸机包必须包含 Rust 核心。 TimescaleDB 是一个外部裸机扩展，不由该构建器捆绑。

以 root 身份发布和运行 — 安装程序拒绝在 PATH 上没有 `systemctl` 的情况下继续：
```bash
scp release/AetherEdge-baremetal-arm64-<version>.run root@192.168.30.21:/tmp/
ssh root@192.168.30.21 'chmod +x /tmp/AetherEdge-baremetal-arm64-<version>.run && /tmp/AetherEdge-baremetal-arm64-<version>.run'
```

`scripts/install-baremetal.sh`（`.run` 存档提取并运行的脚本）将安装布局为：

| 路径 | 内容 |
|------|----------|
| `/opt/aether/bin/` | 服务二进制文件和 `aether`CLI；仅在显式选择的扩展包中使用 Redis 工具 |
| `/etc/aether/config/` | 激活的配置（首次安装时的 `config.template/`） |
| `/etc/aether/aether.env` | 显式配置/数据/数据库路径、`AETHER_LOG_DIR`、`RUST_LOG` 和新生成的机密（模式600) |
| `/etc/aether/install.yaml` | CLI 使用的非秘密安装布局（`config_dir`、`data_dir`、运行时模式、发布通道和启用的包） |
| `/etc/aether/script-host/main.py` | 用于 aether-io 自定义转换的 Python 脚本宿主（与`services/io/src/protocols/core/script_runner.rs` 中的部署路径查找） |
| `/var/lib/aether/` | 服务日志 (`logs/`) 和可选的 Redis 数据 (`redis/`) |

它还将 `aether` 符号链接到 `/usr/local/bin` 并删除 `/etc/profile.d/aether.sh` PATH 条目，安装 systemd单元，针对 `/etc/aether/config` 运行 `aether init` 和 `aether sync`，并以 `systemctl enable --now aether.target` 结束。

包含 Home Assistant 的裸机安装包在安装后仍保持关闭。安装器不会生成 Home
Assistant、消息代理或签名密钥，也不会自动开启任何集成。先显式创建持久化状态
目录和已经调试的控制策略：

```bash
install -d -o root -g root -m 0700 \
  /var/lib/aether/home-assistant \
  /var/lib/aether/home-assistant/cloudlink \
  /var/lib/aether/home-assistant/control
```

把所选配置以及上表相同的固定引用和值变量写入
`/etc/aether/aether.env`。将
`AETHER_HOME_ASSISTANT_CLOUDLINK_RUNTIME_CONFIG_DIR` 设为
`/etc/aether/config`；所有可变账本、暂存日志、会话代次、策略和审计文件都只能
使用 `/var/lib/aether/home-assistant/...`。不要设置已经废弃的
`AETHER_HOME_ASSISTANT_CONTROL_EDGE_KEY_ID` 或
`AETHER_HOME_ASSISTANT_CONTROL_EDGE_SIGNING_KEY_REF`；回执统一使用当前
CloudLink 网关会话签名器。环境文件必须归 `root` 所有，权限为 0600：

```bash
chown root:root /etc/aether/aether.env
chmod 600 /etc/aether/aether.env
systemctl restart aether-io
```

安装包中的 `aether-io.service` 已通过
`EnvironmentFile=/etc/aether/aether.env` 读取该文件；目标设备不再需要源码仓库
或 `cargo run`。

日常操作是本机 systemd：
```bash
systemctl status aether.target
journalctl -u aether-io -f
```

`aether services` 和 `aether doctor` 会自动检测此模式——请参阅 [CLI 参考：aether services](/reference/cli#aether-服务) 和 [aether doctor](/reference/cli#aether-doctor)——无需额外标志。检测逻辑（`tools/aether/src/deploy_mode.rs`）会检查 `/etc/systemd/system/aether.target` 以及 PATH 中的 `systemctl`；任一条件不满足时，就回退到 Docker Compose 路径。在 systemd 模式下，`aether services start/stop/restart/status` 会把规范服务名（例如 `aether-io`）直接传给 `systemctl <verb>`；未指定服务时使用 `aether.target`。`aether services logs <service>` 则调用 `journalctl -u <service>`。`aether services build/pull/clean` 在此模式下都会返回错误，因为裸机安装不存在容器镜像，而 `.run` 安装包也不是原地升级器。`aether services refresh --smart` 会退化为普通的 `systemctl restart`，并提示没有镜像可供比较，因此 `--smart` 不产生额外效果。Redis 不属于默认健康检查合约；启用扩展的运维人员可以单独检查其单元或配置文件。

六个 Rust 服务单元均未声明 `Requires=aether-redis.service`。默认目标启动并保持其SHM/SQLite独立工作；启用的 Redis 镜像不能成为服务可用性依赖项。

裸机安装程序具有与 Docker 安装程序相同的仅限新鲜合约。在停止 `aether.target` 或替换文件之前，在只读预检期间，在具有 `/opt/aether`、`/etc/aether`、`/var/lib/aether`、已安装单元或运行时数据的主机上重新运行 `.run` 软件包会失败。没有自动二进制替换、配置合并、可选单元迁移或先前版本回滚路径。备份/导出所需的状态，卸载旧的运行时，并在安装新版本之前手动重新定位每个保留的足迹；安装程序当前不支持将该状态导入到新版本中。这不会删除安装程序对部分完成的全新安装的失败清理。

使用安装程序编写的脚本进行卸载：
```bash
/opt/aether/uninstall.sh
```

它会停止并禁用 `aether.target`，删除 systemd 单元、`aether` 符号链接、PATH 条目和 `/opt/aether` 本身。 `/etc/aether` 和 `/var/lib/aether`（配置和运行时数据）保留在原处。这些保留的目录故意使以后的全新安装失败，直到操作员导出、重新定位或删除它们。

## 运行时路径

共享内存段路径按此顺序解析 (`crates/aether-dataplane/src/core/config.rs`)：

1. `AETHER_SHM_PATH` 环境变量，如果已设置
2. `/shm/rtdb/aether-rtdb.shm`，如果 `/shm/rtdb` 目录存在（ Docker 挂载点）
3. `/dev/shm/aether-rtdb.shm`（Linux 上）
4. `/tmp/aether-rtdb.shm`（macOS 开发）

在容器内，`/shm/rtdb` 是主机的 `/dev/shm`，因此两个视图命名相同的文件。 Docker 还通过 `AETHER_M2C_SOCKET` 和 `AETHER_AUTOMATION_POINT_WATCH_SOCKET` 将 aether-automation 命令套接字和 PointWatch 套接字放置在该目录中；本机部署保留 `/tmp` 默认值。外设 PointWatch 套接字名称源自已解析的 SHM 路径，因此每个进程绑定一个不同的端点。

其他状态：

- **SQLite** — `aether.db` 位于数据目录中：已安装设备上的 `/opt/AetherEdge/data`、撰写签出 (`AETHER_BASE_PATH`) 中的 `./data`；容器将其视为 `/app/data/aether.db` (`AETHER_DB_PATH`)。
- **嵌入式历史记录** — aether-history 默认将 `aether-history.db` 写入同一数据目录 (`AETHER_HISTORY_DB_PATH`)。 PostgreSQL/TimescaleDB 是一个选择加入的存储适配器，而不是基本运行时先决条件。
- **配置** — `aether` CLI 首先遵循标志和 `AETHER_*_PATH` 覆盖，然后读取 `/etc/aether/install.yaml`。如果没有安装上下文，源签出将使用 `./data/config` 和 `./data`；从未隐式采用未注册的旧安装目录。
- **日志** — `${AETHER_LOG_PATH:-./logs}` 在主机上，`/app/logs` 在容器中。

## 设备上的服务管理

安装的 `aether` CLI 包装 Docker Compose 进行日常操作：
```bash
aether services start      # start one or more services (or all)
aether services stop       # stop services
aether services status     # container status
aether services refresh    # recreate containers from the installed composition
aether services logs       # view service logs

aether doctor              # Docker, core services, SQLite, config files,
                           # shared memory
```

`aether services refresh` 从设备上已安装的合成和映像集重新创建容器。这是相同版本的恢复操作，不支持替换已安装版本的路径。请参阅[入门](/guides/getting-started) 了解健康的 `aether doctor` 运行涵盖的内容。

## 相关页面

- [入门](/guides/getting-started) — 构建、初始化和验证新的签出
- [连接设备](/guides/connect-devices) — 在堆栈建立后添加通道和映射点running
- [系统架构](/concepts/architecture) — 这些容器运行的服务
