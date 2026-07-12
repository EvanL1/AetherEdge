# Aether CLI 使用指南

> 此文件是迁移期旧版指南，不再作为命令真源。当前命令、路径优先级和
> Docker/systemd 行为以 [`reference/cli.md`](reference/cli.md) 为准。

Aether 是 AetherEMS 的统一管理工具，提供配置同步、服务管理和运维操作的一站式解决方案。

## 快速开始

```bash
# 首次使用：默认只生成不落盘的安全计划
aether --json setup

# 审阅 JSON 中的 plan_id 后，应用同一个未变化的计划
aether setup apply --plan-id <PLAN_ID>

# 启动服务
aether services start

# 验证系统状态
aether doctor
```

---

## 目录

- [全局选项](#全局选项)
- [配置管理命令](#配置管理命令)
  - [sync - 同步配置](#sync---同步配置)
  - [status - 查看状态](#status---查看状态)
  - [init - 初始化数据库](#init---初始化数据库)
  - [export - 导出配置](#export---导出配置)
- [服务管理命令](#服务管理命令)
  - [channels - 通道管理](#channels---通道管理)
  - [models - 模型管理](#models---模型管理)
  - [rules - 规则管理](#rules---规则管理)
  - [services - Docker 服务](#services---docker-服务)
  - [logs - 日志管理](#logs---日志管理)
- [调试工具](#调试工具)
  - [rtdb - 可选 Redis 镜像操作](#rtdb---可选-redis-镜像操作)
  - [shm - 共享内存](#shm---共享内存)
  - [doctor - 系统诊断](#doctor---系统诊断)
- [环境变量](#环境变量)
- [常见场景](#常见场景)

---

## 全局选项

所有命令都支持以下全局选项：

| 选项 | 短选项 | 说明 |
|------|--------|------|
| `--verbose` | `-v` | 启用详细日志输出 |
| `--no-color` | | 禁用彩色输出（用于脚本或日志记录） |
| `--config-path <PATH>` | `-c` | 配置文件目录（默认自动检测） |
| `--db-path <PATH>` | | 数据库文件目录（默认自动检测） |
| `--offline` | `-o` | 强制离线模式（使用本地库 API） |
| `--online` | | 强制在线模式（仅使用 HTTP API） |

### 路径自动检测

Aether 按以下优先级检测路径：

1. **命令行参数**：`--config-path` / `--db-path`
2. **环境变量**：`AETHER_CONFIG_PATH` / `AETHER_DATA_PATH`
3. **安装上下文**：`/etc/aether/install.yaml`（Docker 默认记录
   `/opt/AetherEdge/data/config` / `/opt/AetherEdge/data`，裸机安装记录
   `/etc/aether/config` / `/var/lib/aether`）
4. **开发路径**：`./data/config` / `./data`，与仓库 Compose 的站点挂载一致；
   不会因为旧安装目录存在就自动接管它

### 离线 vs 在线模式

- **离线模式** (`--offline`)：直接调用本地库，无需服务运行，响应更快
- **在线模式** (`--online`)：通过 HTTP API 调用运行中的服务
- **自动模式**（默认）：优先使用离线模式，失败时回退到在线

---

## 配置管理命令

### sync - 同步配置

将 YAML/CSV 配置文件同步到 SQLite 数据库。

```bash
# 同步所有配置
aether sync

# 验证配置（不实际写入数据库）
aether sync --dry-run

# 显示详细进度
aether sync --detailed

# 强制替换受管理的配置行（仍执行完整验证）
aether sync --force

# 同步后检查数据库一致性
aether sync --check
```

**选项：**

| 选项 | 短选项 | 说明 |
|------|--------|------|
| `--dry-run` | `-n` | 仅验证，不写入数据库 |
| `--force` | `-f` | 完整替换受管理的配置行；不会跳过验证 |
| `--detailed` | `-d` | 显示每个项目的同步进度 |
| `--check` | | 同步后检查重复 ID 和引用完整性 |

**同步顺序：** global → io → automation

### status - 查看状态

显示当前配置状态和数据库信息。

```bash
# 基本状态
aether status

# 详细状态（包含同步时间和项目数量）
aether status --detailed
```

### init - 初始化数据库

创建或升级数据库 schema。

```bash
# 初始化数据库（安全升级，不删除数据）
aether init

# 注意：--force 选项已禁用，防止意外数据丢失
```

**安全机制：**
- 使用 `CREATE TABLE IF NOT EXISTS` 确保安全升级
- 不会删除已有数据
- 如需重置数据库，需手动删除 `data/aether.db`

### export - 导出配置

从数据库导出配置到 YAML/CSV 文件。

```bash
# 导出到默认目录（config/）
aether export

# 导出到指定目录
aether export --output /path/to/backup/

# 显示详细导出进度
aether export --detailed
```

---

## 服务管理命令

### channels - 通道管理

管理通信通道（协议、设备连接）。

```bash
# 列出所有通道
aether channels list

# 查看通道状态
aether channels status <channel_id>

# 注入 T/S 仿真值（仅在 io 显式设置 AETHER_ALLOW_SIMULATION_WRITES=true 时可用）
aether channels write <channel_id> --type T --id <point_id> --value <value>

# 真实设备命令统一走实例 action（含签名身份、路由、确认与审计）
AETHER_ACCESS_TOKEN='<signed access JWT>' \
  aether models instances action <instance_id> --point-id <point_id> --value <value> --confirmed

# 重新加载通道配置
aether channels reload

# 检查服务健康状态
aether channels health
```

**示例：**

```bash
# 查看通道 1 状态
aether channels status 1

# 注入通道 1 的遥测仿真值
aether channels write 1 --type T --id 10 --value 50.5

# 通过实例动作下发真实设备命令
AETHER_ACCESS_TOKEN='<signed access JWT>' \
  aether models instances action 2 --point-id 1 --value 50.5 --confirmed
```

### models - 模型管理

管理产品模板和设备实例。

#### 产品管理

```bash
# 列出所有内置产品
aether models products list

# 查看可用产品定义（开发用）
aether models products available

# 获取产品详情
aether models products get <product_name>
```

**示例：**

```bash
aether models products list
aether models products get PCS
aether models products get Battery
```

> **注意**：产品名称区分大小写，必须与 `aether models products list` 返回的 `product_name` 完全匹配。

#### 实例管理

```bash
# 列出所有实例
aether models instances list

# 按产品类型筛选
aether models instances list --product PCS

# 创建新实例
aether models instances create <product> <name> [--props key=value...]

# 获取实例详情
aether models instances get <name>

# 更新实例属性
aether models instances update <name> --props key=value...

# 删除实例
aether models instances delete <name>
aether models instances delete <name> --force  # 跳过确认

# 获取实例运行时数据
aether models instances data <name>
aether models instances data <name> --point-type M  # 仅测量点
aether models instances data <name> --point-type A  # 仅动作点
```

**示例：**

```bash
# 创建 PCS 实例
aether models instances create PCS pcs_01 \
  --props rated_power=500.0 \
  --props manufacturer=Sungrow

# 更新实例属性
aether models instances update pcs_01 --props rated_power=600.0

# 查看实例运行时数据
aether models instances data pcs_01
```

### rules - 规则管理

管理业务规则（条件触发、定时任务）。

```bash
# 列出所有规则
aether rules list

# 仅显示已启用的规则
aether rules list --enabled

# 获取规则详情
aether rules get <rule_id>

# 启用/禁用规则
AETHER_ACCESS_TOKEN='<Admin 或 Engineer JWT>' \
  aether rules enable <rule_id> --confirmed
AETHER_ACCESS_TOKEN='<Admin 或 Engineer JWT>' \
  aether rules disable <rule_id> --confirmed

# 执行规则
AETHER_ACCESS_TOKEN='<Admin 或 Engineer JWT>' \
  aether rules execute <rule_id> --confirmed
```

> **注意**：没有独立的"测试"/"仅评估不执行"命令——`execute` 就是真实执行。
> 命令响应中的“成功”表示本地命令平面已接受，不代表物理设备已执行或达到目标值；应读取对应测点验证。
> 详细 `execution_path` 和每个动作的结果持久化在本地 SQLite `rule_history` 中，并可由
> aether-api 的 rule WebSocket 订阅读取。

**示例：**

```bash
# 列出所有已启用规则
aether rules list --enabled

# 启用规则 1001
AETHER_ACCESS_TOKEN='<Admin 或 Engineer JWT>' \
  aether rules enable 1001 --confirmed

# 手动执行规则（可能下发真实设备命令，必须显式确认）
AETHER_ACCESS_TOKEN='<Admin 或 Engineer JWT>' \
  aether rules execute 1001 --confirmed
```

### services - Docker 服务

管理 AetherEMS Docker 容器。

```bash
# 启动所有服务
aether services start

# 启动指定服务
aether services start aether-io aether-automation

# 停止服务
aether services stop
aether services stop aether-io

# 重启服务
aether services restart
aether services restart aether-automation

# 查看服务状态
aether services status

# 查看服务日志
aether services logs <service>
aether services logs aether-io --follow
aether services logs aether-automation --tail 200

# 重新加载配置（热加载）
aether services reload

# 构建 Docker 镜像
aether services build
aether services build aether-io aether-automation

# 拉取最新镜像
aether services pull

# 清理 Docker 资源
aether services clean
aether services clean --volumes  # 同时清理卷

# 刷新服务（重建容器）
aether services refresh
aether services refresh --pull   # 先拉取最新镜像
aether services refresh --smart  # 智能模式（仅更新变化的服务镜像）
```

**特殊命令：**

```bash
# 通过 M2C 路由执行动作
aether services set-action <instance_name> <point_id> <value>
aether services set-action pcs_01 1 100.0 --detailed

# 查看路由表
aether services routing-show
aether services routing-show --route-type c2m      # 仅上行路由
aether services routing-show --route-type m2c      # 仅下行路由
aether services routing-show --prefix "2:T:"       # 按前缀筛选
aether services routing-show --limit 50 --detailed
```

### logs - 日志管理

动态调整运行中服务的日志级别。

```bash
# 设置日志级别
aether logs level <service> <level>

# 获取当前日志级别
aether logs get <service>
```

**服务名称：** `aether-io`, `aether-automation`, `all`

**日志级别：** `trace`, `debug`, `info`, `warn`, `error`

**示例：**

```bash
# 切换所有服务到 debug 模式
aether logs level all debug

# 设置 aether-io 为 trace 级别
aether logs level aether-io trace

# 使用过滤器语法
aether logs level aether-io "info,aether_io::protocol=debug"

# 查看所有服务当前日志级别
aether logs get all
```

---

## 调试工具

### rtdb - 可选 Redis 镜像操作

仅用于检查显式启用的 Redis `StateMirror` 扩展。该镜像不是实时状态权威面；
实时值应通过 SHM 或服务 API 读取。

```bash
# 获取键值
aether rtdb get <key>
aether rtdb get <key> --field <field>  # Hash 字段

# 设置键值
aether rtdb set <key> <value>
aether rtdb set <key> <value> --field <field>

# 扫描键
aether rtdb scan <pattern> [--limit 100]

# 删除键
aether rtdb del <key1> [key2...]
aether rtdb del <key> --force

# 检查键类型和内容
aether rtdb inspect <key>
aether rtdb inspect <key> --full

# 显示常用键模式
aether rtdb patterns
```

**扩展常见镜像键模式：**

| 模式 | 说明 |
|------|------|
| `inst:<id>:M` | 实例测量点 Hash |
| `inst:<id>:A` | 实例动作点 Hash |
| `io:<ch_id>:T` | 通道遥测点 Hash |
| `io:<ch_id>:S` | 通道信号点 Hash |

具体键集合由镜像扩展配置决定，不应被核心服务用作路由或实时状态来源。

### shm - 共享内存

零延迟共享内存 CLI（类似 mysql-cli）。

```bash
# 一次性查询
aether shm get <key>

# 查看共享内存信息
aether shm info

# 实时监控键值变化
aether shm watch <key> [--interval-ms 500]

# TUI 实时仪表板（类似 htop）
aether shm top
```

**键格式：**

| 格式 | 说明 | 示例 |
|------|------|------|
| `inst:<id>:M:<point_id>` | 实例测量点 | `inst:1:M:10` |
| `inst:<id>:A:<point_id>` | 实例动作点 | `inst:1:A:5` |
| `ch:<id>:T:<point_id>` | 通道遥测点 | `ch:1001:T:1` |
| `ch:<id>:S:<point_id>` | 通道信号点 | `ch:1001:S:1` |
| `ch:<id>:C:<point_id>` | 通道控制点 | `ch:1001:C:1` |
| `ch:<id>:A:<point_id>` | 通道调节点 | `ch:1001:A:1` |

**示例：**

```bash
# 查询实例 1 的测量点 10
aether shm get inst:1:M:10

# 实时监控通道 1001 遥测点 1
aether shm watch ch:1001:T:1 --interval-ms 200

# 打开实时仪表板
aether shm top
```

### doctor - 系统诊断

检查系统健康状态并诊断问题。

```bash
# 基本健康检查
aether doctor

# 详细输出（包含响应时间）
aether doctor --verbose

# JSON 格式输出（用于脚本）
aether doctor --json
```

**检查项目：**

| 检查项 | 说明 |
|--------|------|
| Docker Engine | Docker 是否运行 |
| Redis（可选） | 已启用 `redis` profile 时的镜像容器状态和连接 |
| aether-io | 通信服务健康状态 |
| aether-automation | 模型服务健康状态 |
| Database | SQLite 数据库状态 |
| Config Files | 配置文件完整性 |
| Shared Memory | 共享内存可用性 |

**输出示例：**

```
✓ Docker Engine    Running (v24.0.7)
○ Redis            Optional profile not enabled
✓ aether-io       Healthy (port 6001)
✓ aether-automation Healthy (port 6002)
✓ Database         OK (last sync: 2024-01-15 10:30)
✓ Config Files     All present
✓ Shared Memory    Available
```

---

## 环境变量

Aether 支持通过环境变量配置，所有变量使用 `AETHER_` 前缀：

| 变量 | 说明 | 默认值 |
|------|------|--------|
| `AETHER_CONFIG_PATH` | 配置文件目录 | 自动检测 |
| `AETHER_DATA_PATH` | 数据文件目录 | 自动检测 |
| `AETHER_REDIS_URL` | 可选 Redis 镜像连接 URL | `redis://localhost:6379` |
| `AETHER_IO_URL` | Io 服务 URL | `http://localhost:6001` |
| `AETHER_AUTOMATION_URL` | Automation 服务 URL | `http://localhost:6002` |

---

## 常见场景

### 场景 1：首次部署

```bash
# 1. 初始化数据库
aether init

# 2. 同步配置（验证模式）
aether sync --dry-run

# 3. 正式同步
aether sync

# 4. 启动服务
aether services start

# 5. 验证系统
aether doctor
```

### 场景 2：更新配置

```bash
# 1. 编辑配置文件
vim config/io/io.yaml

# 2. 验证更改
aether sync --dry-run --detailed

# 3. 同步到数据库
aether sync

# 4. 热加载服务
aether services reload
```

### 场景 3：调试问题

```bash
# 1. 检查系统状态
aether doctor --verbose

# 2. 查看服务日志
aether services logs aether-io --follow

# 3. 切换到 debug 模式
aether logs level all debug

# 4. 如启用了可选 Redis 镜像，检查镜像数据
aether rtdb scan "inst:*"

# 5. 监控实时数据
aether shm top
```

### 场景 4：更新服务镜像

```bash
# 智能刷新（推荐）
aether services refresh --smart

# 或手动流程
aether services pull
aether services refresh
```

### 场景 5：备份和恢复

```bash
# 导出配置备份
aether export --output /backup/config-$(date +%Y%m%d)/

# 恢复配置
cp -r /backup/config-20240115/* config/
aether sync
```

---

## 故障排除

### 问题：aether 命令未找到

```bash
# 确保 aether 在 PATH 中
export PATH="$PATH:/opt/AetherEdge/bin"

# 或使用完整路径
/opt/AetherEdge/bin/aether doctor
```

### 问题：数据库连接失败

```bash
# 检查数据库文件
ls -la data/aether.db

# 重新初始化（如果损坏）
rm data/aether.db
aether init
aether sync
```

### 问题：服务无法启动

```bash
# 检查 Docker 状态
docker ps -a

# 查看服务日志
aether services logs aether-io --tail 200

# 检查端口占用
lsof -i :6001
```

### 问题：可选 Redis 镜像命令不可用

```bash
# 显式启动镜像 profile
docker compose --profile redis up -d

# 检查环境变量
echo $AETHER_REDIS_URL

# 检查镜像连接
aether --verbose rtdb scan "inst:*"
```
