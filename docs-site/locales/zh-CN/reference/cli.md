---
title: "CLI 参考"
description: "Aether CLI 的服务、同步、诊断、通道、规则等命令参考"
updated: 2026-07-12
---

# CLI 参考

`aether`（版本0.5.0）是Aether的统一管理工具。它涵盖配置管理（`setup`、`sync`、`status`、`init`、`export`）和服务操作（`channels`、`models`、`rules`、`services`、`logs` 等）。下面的每个部分都是从二进制文件自己的 `--help` 输出生成的。
```
Usage: aether [OPTIONS] <COMMAND>
```

在终端上使用 `aether <command> --help` 获取相同信息。

## 全局标志

每个命令都接受这些标志：

| 标志 | 说明 |
|------|-------------|
| `-v, --verbose` | 启用详细信息日志记录 |
| `--no-color` | 禁用彩色输出 |
| `--json` | 以 JSON 形式输出（抑制横幅和颜色；对于脚本和 AI 代理） |
| `--host <HOST>` | 远程操作的目标主机（覆盖 localhost）默认） |
| `-c, --config-path <CONFIG_PATH>` | 配置目录；覆盖环境和已安装的布局 |
| `--db-path <DB_PATH>` | 数据库目录；覆盖环境和已安装的布局 |
| `-h, --help` | 打印帮助 |
| `-V, --version` | 打印版本 |

使用 `--json` 时，结果会以 `{success, ...}` 信封写入标准输出（请参阅下文“退出代码和 JSON 模式”），诊断信息则写入标准错误。`mcp` 命令是例外：它通过 stdio 调用 MCP JSON-RPC，因此 `--json` 不会改变其输出。帮助信息没有声明环境变量；主机和路径的默认值来自上述标志。

## Aether设置

计划或应用保守的首次运行配置。如果没有子命令，`setup` 与 `setup plan` 相同，并且始终是只读的。
```
Usage: aether setup [COMMAND]

Commands:
  plan   Recompute and print the read-only setup plan
  apply  Apply an unchanged safe plan after explicit confirmation
```

```bash
# Human-readable, read-only plan
aether setup

# Structured plan for an AI agent or script
aether --json setup

# The only persistent setup operation
aether setup apply --plan-id <PLAN_ID>
```

SHA-256 计划 ID 绑定目标路径、安全文件指纹、检测到的额外文件和 SQLite 状态。 Apply 在写入之前重新计算它并拒绝过时的 ID。站点状态为：

| 状态 | 含义 | 应用行为 |
|-------|---------|----------------|
| `fresh` | 不存在配置或本地数据库 | 仅创建四个安全空文件和本地 SQLite 状态 |
| `safe_partial` | 安全的精确子集文件/数据库存在 | 保留现有文件并仅创建缺失的安全状态 |
| `safe_ready` | 安全空配置已同步 | 成功无操作 |
| `existing` | 检测到完整的自定义或委托站点 | 拒绝；零写入 |
| `blocked` | 检测到部分自定义、不可读或无法识别的站点 | 拒绝；零写入和显式阻止程序 |

即使成功应用也会报告`ready: false`：它永远不会启动服务、启用设备或规则、执行物理控制或安装域包。继续`aether services start`和`aether doctor`；设备调试是一项单独的审核操作。

## Aether运行时清单

在安装 Pack 或启动服务之前验证组合提供的运行时元数据。如果没有 `--path`，该命令将读取 `<config-path>/runtime-manifest.json`，并且还要求其目标操作系统和架构与当前进程匹配。显式工件路径可验证模式、Aether版本、已知功能/特性、精确的特征派生协议以及校验和，而无需将分阶段工件绑定到验证者主机。
```bash
aether runtime-manifest
aether --json runtime-manifest --path ./runtime-manifest.json
```

不存在完全分发后备：即使 `packs: []` 丢失、被篡改或不兼容的清单也会出现错误。

## Aether包

构建或安装仅限 Pack 的工件。这些是本地文件系统操作； `--host` 被忽略。
```text
Usage: aether packs [OPTIONS] <COMMAND>

Commands:
  build    Build a data-only Pack bundle bound to one Kernel runtime manifest
  install  Verify, publish, and atomically activate a Pack bundle
```

```bash
aether packs build \
  --pack-root ./packs/example \
  --runtime-manifest ./runtime-manifest.json \
  --output ./example.bundle

aether packs install --artifact ./example.bundle
```

`build` 根据提供的校验和运行时清单验证 `pack.yaml`，并拒绝内核/构建目录、源文件、可执行文件、符号链接和无限制的有效负载。 `install` 要求已安装的内核版本、目标和完整的运行时清单摘要相匹配，发布到 `<data-path>/packs/<id>/<version>`，并仅在验证完整的候选活动 Pack 集后以原子方式更新 `global.yaml`。它不会启动服务或调试设备。

## aethersync

将所有配置同步到 SQLite 数据库。
```
Usage: aether sync [OPTIONS]
```

| 标记 | 描述 |
|------|-------------|
| `-n, --dry-run` | 仅验证，不写入数据库（试运行） |
| `-f, --force` | 成功验证后替换同步管理的行；存在任何受控操作路线时被拒绝 |
| `-d, --detailed` | 显示每个项目的详细进度 |
| `--check` | 检查数据库一致性（重复、引用） |
```bash
aether sync --dry-run
```

## aether status

显示当前配置状态。
```
Usage: aether status [OPTIONS]
```

| 标记 | 描述 |
|------|-------------|
| `-d, --detailed` | 显示详细状态 |
```bash
aether status --detailed
```

## aether init

初始化数据库架构（仅迁移，安全升级）。没有特定于命令的标志。
```
Usage: aether init [OPTIONS]
```

```bash
aether init
```

## aether export

将配置从 SQLite 导出到 YAML/CSV。
```
Usage: aether export [OPTIONS]
```

| 标记 | 说明 |
|------|-------------|
| `-O, --output <OUTPUT>` | 输出目录（默认：`config/`） |
| `-d, --detailed` | 显示详细导出进度 |
```bash
aether export -O /tmp/config-backup
```

## Aether通道

管理通信通道和协议。
```
Usage: aether channels [OPTIONS] <COMMAND>
```

子命令：`list`、`status`、`control`、`adjust`、`reload`、`health`、`create`、`update`、`delete`、`enable`、`disable`、 `mappings`、`unmapped-points`、`write`、`points`。

### 通道列表

列出所有已配置的通信通道。
```
Usage: aether channels list [OPTIONS]
```

```bash
aether channels list --json
```

### 频道状态

获取特定频道的状态。
```
Usage: aether channels status [OPTIONS] <CHANNEL_ID>
```

```bash
aether channels status 1001
```

### 通道重新加载

根据权威的所需状态协调每个通道运行时。保留命令名称是为了实现兼容性，但它调用规范的受管 `POST /api/channels/reconcile` 端点，而不是旧的重新加载路由。
```
Usage: aether channels reload [OPTIONS] --confirmed
```

| 标记 | 描述 |
|------|-------------|
| `--confirmed` | 显式确认此高风险运行时协调；需要 `AETHER_ACCESS_TOKEN` |
```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' aether channels reload --confirmed
```

收据报告每个通道以及 `degraded_count`、`reconciliation_required` 和终端 `completion_audit` 的净化后的期望状态观察和运行时预测。保留其 UUID `request_id`；此操作可以重新连接协议会话并且是非幂等的，因此切勿自动重试，包括在不完整的终端审核之后。

### 通道运行状况

检查通信服务运行状况。
```
Usage: aether channels health [OPTIONS]
```

```bash
aether channels health --json
```

### 创建通道（`channels create`）

创建新的沟通渠道。
```
Usage: aether channels create [OPTIONS] --name <NAME> --protocol <PROTOCOL> --params <PARAMS> --confirmed
```

| 标志 | 描述 |
|------|-------------|
| `--name <NAME>` | 通道名称（必须是唯一的） |
| `--protocol <PROTOCOL>` | 协议类型（`modbus_tcp`、`modbus_rtu`、`virtual`、`di_do`、 `can`) |
| `--params <PARAMS>` | JSON 字符串形式的协议参数（例如 `'{"host":"192.168.1.10","port":502}'`） |
| `--description <DESCRIPTION>` | 通道描述 |
| `--enabled <ENABLED>` | 立即启动通道（默认值：false）[可能值：`true`、`false`] |
| `--id <ID>` | 覆盖通道 ID（如果省略则自动分配） |
| `--confirmed` | 明确确认此高风险调试突变；需要 `AETHER_ACCESS_TOKEN` |
```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' aether channels create \
  --name pcs-main --protocol modbus_tcp \
  --params '{"host":"192.168.1.10","port":502}' --confirmed
```

### 频道更新

更新现有频道的配置。
```
Usage: aether channels update [OPTIONS] <CHANNEL_ID>
```

| 标记 | 描述 |
|------|-------------|
| `--name <NAME>` | 新通道名称 |
| `--params <PARAMS>` | 更新了 JSON 字符串形式的协议参数 |
| `--description <DESCRIPTION>` | 已更新描述 |
| `--expected-revision <EXPECTED_REVISION>` | 来自最新通道读取的所需状态比较和设置保护；必须至少为 1 |
| `--confirmed` | 明确确认此高风险委托突变；需要 `AETHER_ACCESS_TOKEN` |
```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' aether channels update 1001 \
  --description "PCS main feed" --expected-revision 7 --confirmed
```

### 通道删除

删除通道及其测量拥有的点、映射和路由。当物理动作路线瞄准通道时，命令无法关闭；首先使用受控路由命令删除或迁移该路由。
```
Usage: aether channels delete [OPTIONS] <CHANNEL_ID>
```

| 标志 | 描述 |
|------|-------------|
| `-f, --force` | 仅跳过交互式提示；它永远不会取代`--confirmed` |
| `--expected-revision <EXPECTED_REVISION>` | 最新通道读取所需的所需状态比较和设置保护；必须至少为 1 |
| `--confirmed` | 明确确认此高风险委托突变；需要 `AETHER_ACCESS_TOKEN` |
```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' aether channels delete 1001 \
  --force --expected-revision 7 --confirmed
```

### 通道启用

启用通道。
```
Usage: aether channels enable [OPTIONS] <CHANNEL_ID>
```

| 标志 | 描述 |
|------|-------------|
| `--expected-revision <EXPECTED_REVISION>` | 来自最新通道读取的所需状态比较和设置保护；必须至少为 1 |
| `--confirmed` | 明确确认此高风险生命周期突变；需要 `AETHER_ACCESS_TOKEN` |
```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' aether channels enable 1001 \
  --expected-revision 7 --confirmed
```

### 通道禁用

禁用通道。
```
Usage: aether channels disable [OPTIONS] <CHANNEL_ID>
```

| 标志 | 描述 |
|------|-------------|
| `--expected-revision <EXPECTED_REVISION>` | 来自最新通道读取的所需状态比较和设置保护；必须至少为 1 |
| `--confirmed` | 明确确认此高风险生命周期突变；需要 `AETHER_ACCESS_TOKEN` |
```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' aether channels disable 1001 \
  --expected-revision 7 --confirmed
```

五个通道调试和生命周期突变称为受治理的 `io.channel.manage` 应用程序边界。在提交所需状态后，成功可能会报告降级的运行时投影。保留 `request_id`，检查 `resulting_revision` 和 `reconciliation_required`，并且不自动重试非幂等命令。更新、删除、启用和禁用需要最新通道读取返回的修订版本，如果不存在，则在 HTTP 之前失败。 `channels reload` 是第六个受管通道命令，单独映射到 `io.channel.reconcile`，同时需要相同的 `io.channel.manage` 权限、显式确认、承载令牌、UUID 请求 ID 和审核策略。

### 通道映射

显示通道的点映射。
```
Usage: aether channels mappings [OPTIONS] <CHANNEL_ID>
```

```bash
aether channels mappings 1001
```

### 查看未映射点位（`channels unmapped-points`）

列出通道上没有协议地址映射的点。
```
Usage: aether channels unmapped-points [OPTIONS] <CHANNEL_ID>
```

```bash
aether channels unmapped-points 1001
```

### 通道写入

将模拟遥测或信号值注入采集 SHM 平面。该命令仅接受 T/S 点；真正的 C/A 设备命令必须使用 `aether models instances action`，因此无法绕过路由、确认和审核。
```
Usage: aether channels write [OPTIONS] --type <POINT_TYPE> --id <ID> --value <VALUE> <CHANNEL_ID>
```

| 标志 | 描述 |
|------|-------------|
| `--type <POINT_TYPE>` | 模拟点类型：`T` \ | `S` |
| `--id <ID>` | 点 ID（数字或语义） |
| `--value <VALUE>` | 要写入的值 |
```bash
aether channels write 1001 --type T --id 3 --value 42.5
```

### 通道点列表

列出点（按 T/S/C/A 分组）。
```
Usage: aether channels points list [OPTIONS] <CHANNEL_ID>
```

| 标记 | 描述 |
|------|-------------|
| `--type <TYPE>` | 按点类型过滤：`T`、`S`、`C` 或 `A` |
```bash
aether channels points list 1001 --type T
```

### 通道点添加

将点添加到通道。位置参数：`<CHANNEL_ID>` `<POINT_TYPE>`（T 遥测、S 信号、C 控制、A 调整）`<POINT_ID>`。
```
Usage: aether channels points add [OPTIONS] --name <NAME> <CHANNEL_ID> <POINT_TYPE> <POINT_ID>
```

| 标志 | 描述 |
|------|-------------|
| `--name <NAME>` | 信号名称 |
| `--unit <UNIT>` | 单位（例如，V、A、kW） |
| `--scale <SCALE>` | 比例因子 |
| `--description <DESCRIPTION>` | 描述 |
| `--data-type <DATA_TYPE>` | 数据类型（默认：T/A 为 float32，S/C 为 bool） |
```bash
aether channels points add 1001 T 101 --name voltage --unit V --scale 0.1
```

### 通道点更新

更新点的属性。
```
Usage: aether channels points update [OPTIONS] <CHANNEL_ID> <POINT_TYPE> <POINT_ID>
```

| 标志 | 描述 |
|------|-------------|
| `--name <NAME>` | 信号名称 |
| `--unit <UNIT>` | 单位 |
| `--scale <SCALE>` | 比例因子 |
| `--description <DESCRIPTION>` | 描述 |
```bash
aether channels points update 1001 T 101 --scale 0.01
```

### 通道点删除

从通道中删除点。
```
Usage: aether channels points remove [OPTIONS] <CHANNEL_ID> <POINT_TYPE> <POINT_ID>
```

| 标记 | 描述 |
|------|-------------|
| `-f, --force` | 强制删除而不确认 |
```bash
aether channels points remove 1001 T 101 --force
```

### 批量通道点

从 JSON 文件批量创建/更新/删除点。
```
Usage: aether channels points batch [OPTIONS] --file <FILE> <CHANNEL_ID>
```

| 标记 | 描述 |
|------|-------------|
| `--file <FILE>` | JSON 文件的路径：`{"create":[],"update":[],"delete":[]}` |
```bash
aether channels points batch 1001 --file points.json
```

### 通道点映射

显示单个点的实例映射。
```
Usage: aether channels points mapping [OPTIONS] <CHANNEL_ID> <POINT_TYPE> <POINT_ID>
```

```bash
aether channels points mapping 1001 T 101
```

## Aether模型

管理产品模板和设备实例。两个子命令组：`products` 和 `instances`。
```
Usage: aether models [OPTIONS] <COMMAND>
```

### 型号产品列表

显示通过验证的活动包和站点配置选择的产品。
```
Usage: aether models products list [OPTIONS]
```

```bash
aether models products list --json
```

### 可用的模型产品

列出 `products/` 目录中的产品定义。
```
Usage: aether models products available [OPTIONS]
```

```bash
aether models products available
```

### 获取产品模型（`models products get`）

显示有关所选产品的详细信息。
```
Usage: aether models products get [OPTIONS] <NAME>
```

```bash
aether models products get battery
```

### 模型实例列表

显示所有设备实例。
```
Usage: aether models instances list [OPTIONS]
```

| 标记 | 描述 |
|------|-------------|
| `-p, --product <PRODUCT>` | 按产品类型过滤 |
```bash
aether models instances list --product battery
```

### 模型实例创建

从产品模板创建新的设备实例。位置参数：`<PRODUCT>` `<NAME>`。
```
Usage: aether models instances create [OPTIONS] <PRODUCT> <NAME>
```

| 标志 | 描述 |
|------|-------------|
| `-p, --props <PROPS>` | `key=value`格式的属性 |
```bash
aether models instances create battery bat-01 --props capacity=100
```

### 模型实例获取

显示有关实例的详细信息。
```
Usage: aether models instances get [OPTIONS] <NAME>
```

```bash
aether models instances get bat-01
```

### 模型实例更新

更新实例属性。
```
Usage: aether models instances update [OPTIONS] <NAME>
```

| 标记 | 描述 |
|------|-------------|
| `-p, --props <PROPS>` | 要以 `key=value` 格式更新的属性 |
```bash
aether models instances update bat-01 --props capacity=120
```

### modelsinstancesdelete

删除设备实例。

当实例拥有物理操作路由时，命令失败关闭；首先使用受控路由命令删除或迁移该路由。
```
Usage: aether models instances delete [OPTIONS] <NAME>
```

| 标记 | 描述 |
|------|-------------|
| `-f, --force` | 强制删除而不确认 |
```bash
aether models instances delete bat-01 --force
```

### 模型实例数据

从权威SHM平面获取实时测量和动作点数据。
```
Usage: aether models instances data [OPTIONS] <INSTANCE_ID>
```

| 标志 | 描述 |
|------|-------------|
| `-t, --point-type <POINT_TYPE>` | 点类型过滤器（M 表示测量值，A 表示操作，如果未指定，则两者都表示） |
```bash
aether models instances data 9 --point-type M
```

### 模型实例操作

向本地命令平面提交确认的控制操作。成功的响应并不能证明物理设备执行了它；读回相应的测量值以验证结果。如果返回的`audit.status`为`incomplete`，则保留`request_id`和`command_id`；该操作已被接受，不得重试。在运行此命令之前，将 `AETHER_ACCESS_TOKEN` 设置为当前管理员或工程师访问令牌；伪造的参与者/角色标头和本地端口访问不会授予设备控制权限。
```
Usage: aether models instances action [OPTIONS] --point-id <POINT_ID> --value <VALUE> <INSTANCE_ID>
```

| 标志 | 描述 |
|------|-------------|
| `--point-id <POINT_ID>` | 编码为字符串的数字操作点 ID，例如`"1"` |
| `--value <VALUE>` | 要写入的值 |
| `--confirmed` | 显式确认此高风险设备命令 |
```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' \
  aether models instances action 9 --point-id 1 --value 50 --confirmed
```

## Aether规则

管理和执行业务规则。
```
Usage: aether rules [OPTIONS] <COMMAND>
```

### 规则列表

列出所有配置的业务规则。
```
Usage: aether rules list [OPTIONS]
```

| 标记 | 描述 |
|------|-------------|
| `--enabled` | 仅显示已启用的规则 |
```bash
aether rules list --enabled
```

### 获取规则（`rules get`）

显示有关规则的详细信息。
```
Usage: aether rules get [OPTIONS] <RULE_ID>
```

```bash
aether rules get 3
```

### 规则启用

启用业务规则。
```
Usage: aether rules enable [OPTIONS] <RULE_ID>
```

| 标志 | 描述 |
|------|-------------|
| `--confirmed` | 明确确认此高风险规则策略突变 |
```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' aether rules enable 3 --confirmed
```

### 规则禁用

禁用业务规则。
```
Usage: aether rules disable [OPTIONS] <RULE_ID>
```

| 标志 | 描述 |
|------|-------------|
| `--confirmed` | 明确确认此高风险规则策略突变 |
```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' aether rules disable 3 --confirmed
```

### 规则执行

执行规则（如果条件满足则评估并执行）。如果返回的`audit.status`为`incomplete`，则保留`request_id`；执行已经完成，不得重试。
```
Usage: aether rules execute [OPTIONS] <RULE_ID>
```

| 标志 | 描述 |
|------|-------------|
| `--confirmed` | 明确确认该规则可以调度真实设备命令 |
```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' \
  aether rules execute 3 --confirmed
```

### 规则创建

创建新的业务规则。
```
Usage: aether rules create [OPTIONS] --name <NAME>
```

| 标记 | 描述 |
|------|-------------|
| `--name <NAME>` | 规则名称 |
| `--description <DESCRIPTION>` | 规则描述 |
| `--confirmed` | 明确确认此高风险规则策略突变 |
```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' \
  aether rules create --name night-charge --description "Charge during off-peak hours" --confirmed
```

### 规则更新

更新规则元数据和/或流程逻辑。
```
Usage: aether rules update [OPTIONS] <RULE_ID>
```

| 标记 | 描述 |
|------|-------------|
| `--name <NAME>` | 新规则名称 |
| `--description <DESCRIPTION>` | 新描述 |
| `--enabled <ENABLED>` | 启用或禁用规则[可能的值：`true`， `false`] |
| `--priority <PRIORITY>` | 规则优先级（较低 = 较高优先级） |
| `--cooldown-ms <COOLDOWN_MS>` | 执行之间的冷却时间（以毫秒为单位） |
| `--flow-json <FLOW_JSON>` | Vue Flow JSON 文件的路径（使用`-` for stdin) |
| `--confirmed` | 明确确认此高风险规则策略突变 |
```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' \
  aether rules update 3 --flow-json flow.json --confirmed
```

### 规则删除

删除业务规则。
```
Usage: aether rules delete [OPTIONS] <RULE_ID>
```

| 标志 | 描述 |
|------|-------------|
| `-f, --force` | 跳过确认提示 |
| `--confirmed` | 需要安全确认； `--force` 不会取代它 |
```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' \
  aether rules delete 3 --force --confirmed
```

## Aether路由

管理通道到实例点的路由。
```
Usage: aether routing [OPTIONS] <COMMAND>
```

### 路由列表

列出路由配置。
```
Usage: aether routing list [OPTIONS]
```

| 标志 | 描述 |
|------|-------------|
| `-i, --instance <INSTANCE>` | 按实例 ID 过滤 |
| `--channel <CHANNEL>` | 按通道 ID 过滤 |
```bash
aether routing list --instance 9
```

### 路由操作

物理 C/A 目的地的受控单路由命令。每个操作都需要`AETHER_ACCESS_TOKEN`和`--confirmed`；更改路由不会执行设备命令。
```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' aether routing action upsert \
  9 1 --channel-id 1001 --channel-type c --channel-point-id 7 --confirmed

AETHER_ACCESS_TOKEN='<signed access JWT>' \
  aether routing action delete 9 1 --confirmed

AETHER_ACCESS_TOKEN='<signed access JWT>' \
  aether routing action enable 9 1 --confirmed

AETHER_ACCESS_TOKEN='<signed access JWT>' \
  aether routing action disable 9 1 --confirmed
```

`upsert` 接受 `--disabled` 来委托路由而不激活它。旧的 `routing create --point-type a ... --confirmed` 形式仍然是启用的 upsert 的兼容性别名。

### 创建路由（`routing create`）

为实例创建单个路由条目。
```
Usage: aether routing create [OPTIONS] --point-type <POINT_TYPE> --point-id <POINT_ID> --channel-id <CHANNEL_ID> --four-remote <FOUR_REMOTE> --channel-point-id <CHANNEL_POINT_ID> <INSTANCE_ID>
```

| 标记 | 描述 |
|------|-------------|
| `-t, --point-type <POINT_TYPE>` | 点类型：`m`（测量）或 `a`（操作） |
| `-p, --point-id <POINT_ID>` | 实例点 ID |
| `--channel-id <CHANNEL_ID>` | 通道ID |
| `-r, --four-remote <FOUR_REMOTE>` | 四遥类型：`t`（遥测）、`s`（信号）、`c`（控制）、`a`（调节） |
| `-P, --channel-point-id <CHANNEL_POINT_ID>` | 通道点ID |
| `--confirmed` | 明确确认改变物理命令目标的动作路线；需要 `AETHER_ACCESS_TOKEN` |
```bash
aether routing create 9 --point-type m --point-id 101 \
  --channel-id 1001 --four-remote t --channel-point-id 101

AETHER_ACCESS_TOKEN='<signed access JWT>' aether routing create 9 \
  --point-type a --point-id 1 --channel-id 1001 \
  --four-remote c --channel-point-id 7 --confirmed
```

### 路由批处理

从 JSON 文件或标准输入批量更新插入路由。

兼容性批处理仅接受测量条目。在受控批处理应用程序命令可用之前，操作条目将无法关闭；使用 `routing create ... --point-type a --confirmed` 一次创建一个操作路线。
```
Usage: aether routing batch [OPTIONS] --file <FILE> <INSTANCE_ID>
```

| 标记 | 描述 |
|------|-------------|
| `--file <FILE>` | 包含路由条目的 JSON 文件的路径（使用 `-` 作为标准输入） |
```bash
aether routing batch 9 --file routing.json
```

### 删除实例路由（`routing delete-instance`）

删除实例的所有路由。采用实例名称，而不是数字 ID。
```
Usage: aether routing delete-instance [OPTIONS] <INSTANCE_NAME>
```

| 标志 | 描述 |
|------|-------------|
| `-f, --force` | 跳过确认 |
| `--confirmed` | 确认删除物理动作路线；需要 `AETHER_ACCESS_TOKEN` |
```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' \
  aether routing delete-instance bat-01 --force --confirmed
```

### 删除通道路由（`routing delete-channel`）

删除频道的所有路由。
```
Usage: aether routing delete-channel [OPTIONS] <CHANNEL_ID>
```

| 标志 | 描述 |
|------|-------------|
| `-f, --force` | 跳过确认 |
| `--confirmed` | 确认删除物理动作路线；需要 `AETHER_ACCESS_TOKEN` |
```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' \
  aether routing delete-channel 1001 --force --confirmed
```

## aether 服务

启动、停止和管理 Aether 服务。所有服务参数都是可选的；省略它们将针对所有服务。
```
Usage: aether services [OPTIONS] <COMMAND>
```

### 启动服务（`services start`）

启动一项或多项 Aether 服务。
```
Usage: aether services start [OPTIONS] [SERVICES]...
```

```bash
aether services start aether-io aether-automation
```

### 停止服务（`services stop`）

停止一项或多项 Aether 服务。
```
Usage: aether services stop [OPTIONS] [SERVICES]...
```

```bash
aether services stop
```

### 重启服务（`services restart`）

重新启动一项或多项 Aether 服务。
```
Usage: aether services restart [OPTIONS] [SERVICES]...
```

```bash
aether services restart aether-io
```

### 服务状态

显示 Aether 服务的状态。
```
Usage: aether services status [OPTIONS] [SERVICES]...
```

```bash
aether services status --json
```

### 服务日志

查看 Aether 服务的日志。
```
Usage: aether services logs [OPTIONS] <SERVICE>
```

| 标志 | 描述 |
|------|-------------|
| `-f, --follow` | 跟随日志输出 |
| `-n, --tail <TAIL>` | 从末尾开始显示的行数（默认值：100） |
```bash
aether services logs aether-io --follow --tail 200
```

### 构建服务（`services build`）

为服务构建 Docker 镜像。
```
Usage: aether services build [OPTIONS] [SERVICES]...
```

```bash
aether services build aether-io
```

### 拉取服务镜像（`services pull`）

拉取最新的 Docker 镜像。
```
Usage: aether services pull [OPTIONS]
```

```bash
aether services pull
```

### 清理服务构建产物（`services clean`）

清理 Docker 卷和网络。
```
Usage: aether services clean [OPTIONS]
```

| 标志 | 描述 |
|------|-------------|
| `--volumes` | 同时删除卷（仅限长格式；`-v` 是全局详细标志） |
```bash
aether services clean --volumes
```

### 服务刷新

强制使用最新映像重新创建容器。
```
Usage: aether services refresh [OPTIONS] [SERVICES]...
```

| 标志 | 说明 |
|------|-------------|
| `-p, --pull` | 在重新创建之前还提取最新映像 |
| `-s, --smart` | 使用智能模式（仅在映像更改时重新创建；有状态扩展保持显式） |
```bash
aether services refresh --pull --smart
```

## aether logs

日志级别控制和日志文件查看器。
```
Usage: aether logs [OPTIONS] <COMMAND>
```

### 日志级别

设置服务的日志级别。位置参数：`<SERVICE>`（io、自动化、全部）和 `<LEVEL>`（跟踪、调试、信息、警告、错误）或完整的过滤器规范，例如 `"info,io=debug"`。
```
Usage: aether logs level [OPTIONS] <SERVICE> <LEVEL>
```

```bash
aether logs level all debug
```

### 获取日志（`logs get`）

获取服务的当前日志级别（aether-io、aether-automation、全部）。
```
Usage: aether logs get [OPTIONS] <SERVICE>
```

```bash
aether logs get aether-io
```

### 日志列表

列出磁盘上的日志文件（默认：今天）。服务过滤器是可选的。
```
Usage: aether logs list [OPTIONS] [SERVICE]
```

| 标记 | 说明 |
|------|-------------|
| `-d, --date <DATE>` | `YYYYMMDD` 格式的日期（默认值：今天） |
```bash
aether logs list aether-io --date 20260709
```

### 日志视图

查看服务日志文件中的最新行（aether-io、aether-automation、aether-history、aether-uplink、alarm、api）。
```
Usage: aether logs view [OPTIONS] <SERVICE>
```

| 标记 | 描述 |
|------|-------------|
| `-n, --lines <LINES>` | 从末尾开始的行数（默认：50） |
| `--api` | 显示 API 访问日志而不是主日志 |
| `-g, --grep <GREP>` | 过滤包含此模式的行（不区分大小写） |
```bash
aether logs view aether-io -n 100 --grep ERROR
```

### 持续查看日志（`logs tail`）

实时跟踪服务日志文件。
```
Usage: aether logs tail [OPTIONS] <SERVICE>
```

| 标记 | 描述 |
|------|-------------|
| `--api` | 显示 API 访问日志而不是主日志 |
| `-g, --grep <GREP>` | 过滤包含此模式的行（不区分大小写） |
```bash
aether logs tail aether-automation --grep ERROR
```

### 日志 ui

打开具有滚动、搜索和关注功能的交互式日志查看器。
```
Usage: aether logs ui [OPTIONS] <SERVICE>
```

| 标记 | 描述 |
|------|-------------|
| `--api` | 显示 API 访问日志而不是主日志 |
```bash
aether logs ui aether-io
```

## aether shm

零延迟共享内存CLI（如 mysql-cli）。子命令是可选的；运行裸 `aether shm` 直接打开共享内存文件以进行交互式会话（如果 SHM 文件尚不存在，则会失败）。
```
Usage: aether shm [OPTIONS] [COMMAND]
```

### 读取共享内存点位（`shm get`）

获取点值。键格式为 `inst:<id>:M|A:<point_id>` 或
`ch:<id>:T|S|C|A:<point_id>`。
```
Usage: aether shm get [OPTIONS] <KEY>
```

```bash
aether shm get inst:9:M:101
```

### 查看共享内存信息（`shm info`）

显示共享内存统计信息。
```
Usage: aether shm info [OPTIONS]
```

```bash
aether shm info --json
```

### 监视共享内存变化（`shm watch`）

监视键的变化（实时监控）。
```
Usage: aether shm watch [OPTIONS] <KEY>
```

| 标志 | 描述 |
|------|-------------|
| `-i, --interval-ms <INTERVAL_MS>` | 轮询间隔（以毫秒为单位）（默认值：500） |
```bash
aether shm watch ch:1001:T:101 --interval-ms 200
```

### 查看共享内存实时状态（`shm top`）

实时 TUI 仪表板（如 htop）。
```
Usage: aether shm top [OPTIONS]
```

```bash
aether shm top
```

## aether doctor

检查系统健康状况并诊断问题。对于此命令，`-v, --verbose` 显示详细信息（响应时间等）。
```
Usage: aether doctor [OPTIONS]
```

```bash
aether doctor --verbose
```

## aether templates

管理通道配置模板。
```
Usage: aether templates [OPTIONS] <COMMAND>
```

### 模板列表

列出所有频道模板。
```
Usage: aether templates list [OPTIONS]
```

| 标记 | 描述 |
|------|-------------|
| `-p, --protocol <PROTOCOL>` | 按协议类型过滤 |
```bash
aether templates list --protocol modbus_tcp
```

### 获取模板（`templates get`）

显示有关模板的详细信息。
```
Usage: aether templates get [OPTIONS] <ID>
```

```bash
aether templates get 3
```

### 模板快照

将通道的配置快照为可重用模板。
```
Usage: aether templates snapshot [OPTIONS] --name <NAME> <CHANNEL_ID>
```

| 标记 | 描述 |
|------|-------------|
| `-n, --name <NAME>` | 模板名称 |
| `-d, --description <DESCRIPTION>` | 模板描述 |
```bash
aether templates snapshot 1001 --name pcs-modbus-template
```

### 模板应用

将模板应用到目标通道。
```
Usage: aether templates apply [OPTIONS] <TEMPLATE_ID> <CHANNEL_ID>
```

| 标志 | 描述 |
|------|-------------|
| `--clear` | 应用前清除现有点 |
| `--slave-id <SLAVE_ID>` | 覆盖 Modbus 从站 ID |
```bash
aether templates apply 3 1002 --clear --slave-id 2
```

### 模板删除

删除频道模板。
```
Usage: aether templates delete [OPTIONS] <ID>
```

| 标记 | 描述 |
|------|-------------|
| `-f, --force` | 强制删除而不确认 |
```bash
aether templates delete 3 --force
```

## Aether警报

管理警报规则（创建/更新/删除/启用/禁用）；查询警报、事件和统计数据。
```
Usage: aether alarms [OPTIONS] <COMMAND>
```

### 警报列表

列出当前活动的警报。
```
Usage: aether alarms list [OPTIONS]
```

| 标记 | 说明 |
|------|-------------|
| `--channel <CHANNEL>` | 按通道 ID 过滤 |
| `--level <LEVEL>` | 按警告级别过滤（1=低、2=中、3=高） |
| `--keyword <KEYWORD>` | 关键字搜索（规则名称、通道、点） |
| `--page <PAGE>` | 页码，从 1 开始（默认：1） |
| `--size <SIZE>` | 页面大小（默认：50） |
```bash
aether alarms list --level 3
```

### 获取告警（`alarms get`）

获取特定活动警报的详细信息。
```
Usage: aether alarms get [OPTIONS] <ID>
```

```bash
aether alarms get 42
```

### 警报解决

手动清除一个活动警报指示。如果基本条件仍然成立，监控器将在稍后的评估中创建新的警报。
```
Usage: aether alarms resolve [OPTIONS] --confirmed <ID>
```

```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' \
  aether alarms resolve 42 --confirmed
```

### 报警规则

列出报警规则。
```
Usage: aether alarms rules [OPTIONS]
```

| 标记 | 说明 |
|------|-------------|
| `--channel <CHANNEL>` | 按通道 ID 过滤 |
| `--enabled` | 仅显示已启用的规则 |
| `--level <LEVEL>` | 按警告级别过滤（1=低、2=中、 3=高) |
| `--keyword <KEYWORD>` | 关键字搜索 |
| `--page <PAGE>` | 页码，从 1 开始（默认值：1） |
| `--size <SIZE>` | 页面大小（默认值：50） |
```bash
aether alarms rules --enabled
```

### 获取告警规则（`alarms rule-get`）

获取特定报警规则的详细信息。
```
Usage: aether alarms rule-get [OPTIONS] <ID>
```

```bash
aether alarms rule-get 7
```

### 警报事件

列出历史警报事件。
```
Usage: aether alarms events [OPTIONS]
```

| 标记 | 说明 |
|------|-------------|
| `--rule <RULE>` | 按规则 ID 过滤 |
| `--event-type <EVENT_TYPE>` | 按事件类型过滤：`trigger` 或 `recovery` |
| `--level <LEVEL>` | 按警告级别过滤（1=低、2=中、3=高） |
| `--keyword <KEYWORD>` | 关键字搜索 |
| `--page <PAGE>` | 页码，从 1 开始（默认值：1） |
| `--size <SIZE>` | 页面大小（默认值：50） |
```bash
aether alarms events --level 3 --event-type trigger
```

### 警报统计信息

显示警报计数和规则统计信息。
```
Usage: aether alarms stats [OPTIONS]
```

```bash
aether alarms stats --json
```

### 报警监视器

显示报警监视器循环状态。
```
Usage: aether alarms monitor [OPTIONS]
```

```bash
aether alarms monitor
```

### 创建告警规则（`alarms rule-create`）

从 JSON 文件创建警报规则。

警报规则创建、更新、删除、启用、禁用和手动警报解决是受管理的高风险策略命令。将 `AETHER_ACCESS_TOKEN` 设置为当前管理员或工程师访问 JWT 并传递 `--confirmed`；查询命令在本地接口上保持无令牌。
```
Usage: aether alarms rule-create [OPTIONS] --file <FILE> --confirmed
```

| 标记 | 说明 |
|------|-------------|
| `--file <FILE>` | 与警报的 `CreateRuleRequest` 匹配的 JSON 文件的路径 |
| `--confirmed` | 显式确认警报策略突变 |
```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' \
  aether alarms rule-create --file alarm-rule.json --confirmed
```

### 更新告警规则（`alarms rule-update`）

从 JSON 文件更新警报规则（仅当前字段发生变化）。
```
Usage: aether alarms rule-update [OPTIONS] --file <FILE> --confirmed <ID>
```

| 标记 | 说明 |
|------|-------------|
| `--file <FILE>` | 与警报的 `UpdateRuleRequest` 匹配的 JSON 文件的路径 |
| `--confirmed` | 显式确认警报策略突变 |
```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' \
  aether alarms rule-update 7 --file alarm-rule-patch.json --confirmed
```

### 删除告警规则（`alarms rule-delete`）

删除报警规则。
```
Usage: aether alarms rule-delete [OPTIONS] --confirmed <ID>
```

```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' \
  aether alarms rule-delete 7 --confirmed
```

### 启用告警规则（`alarms rule-enable`）

启用警报规则。
```
Usage: aether alarms rule-enable [OPTIONS] --confirmed <ID>
```

```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' \
  aether alarms rule-enable 7 --confirmed
```

### 禁用告警规则（`alarms rule-disable`）

禁用警报规则。
```
Usage: aether alarms rule-disable [OPTIONS] --confirmed <ID>
```

```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' \
  aether alarms rule-disable 7 --confirmed
```

## aether net

管理 MQTT 连接、上行链路配置和 TLS 证书。两个子命令组：`mqtt` 和 `cert`。
```
Usage: aether net [OPTIONS] <COMMAND>
```

### 查看 MQTT 状态（`net mqtt status`）

显示 MQTT 连接状态。
```
Usage: aether net mqtt status [OPTIONS]
```

```bash
aether net mqtt status --json
```

### 查看 MQTT 配置（`net mqtt config`）

显示当前上行链路配置。
```
Usage: aether net mqtt config [OPTIONS]
```

```bash
aether net mqtt config
```

### 设置 MQTT 配置（`net mqtt config-set`）

替换 JSON 文件中的上行链路配置（完整的 `NetConfig` 对象）。
```
Usage: aether net mqtt config-set [OPTIONS] --file <FILE>
```

| 标记 | 描述 |
|------|-------------|
| `--file <FILE>` | 包含完整 `NetConfig` 对象的 JSON 文件的路径 |
```bash
aether net mqtt config-set --file netconfig.json
```

### 重连 MQTT（`net mqtt reconnect`）

重新连接 MQTT 客户端。
```
Usage: aether net mqtt reconnect [OPTIONS]
```

```bash
aether net mqtt reconnect
```

### 断开 MQTT（`net mqtt disconnect`）

断开 MQTT 客户端的连接。
```
Usage: aether net mqtt disconnect [OPTIONS]
```

```bash
aether net mqtt disconnect
```

### 查看证书信息（`net cert info`）

显示已安装的 TLS 证书信息。
```
Usage: aether net cert info [OPTIONS]
```

```bash
aether net cert info
```

### 删除证书（`net cert delete`）

按类型删除 TLS 证书。
```
Usage: aether net cert delete [OPTIONS] <CERT_TYPE>
```

`<CERT_TYPE>` 可能的值：`ca_cert`、`client_cert`、`client_key`。
```bash
aether net cert delete client_cert
```

### 上传证书（`net cert upload`）

上传 TLS 证书文件（最大 1 MB）。接受的扩展名：`.pem` `.crt` `.key` `.cer` `.p12` `.pfx`。
```
Usage: aether net cert upload [OPTIONS] --type <CERT_TYPE> <FILE>
```

| 标志 | 描述 |
|------|-------------|
| `--type <CERT_TYPE>` | 证书角色[可能值：`ca_cert`、`client_cert`、`client_key`] |
```bash
aether net cert upload ca.pem --type ca_cert
```

## aether历史

查询历史传感器数据（最新值、时间范围查询）。
```
Usage: aether history [OPTIONS] <COMMAND>
```

### 历史最新

获取点的最新历史值。位置参数：`<SERIES_KEY>`（例如 `inst:9:M` 或 `io:1001:T`）和 `<POINT_ID>`。
```
Usage: aether history latest [OPTIONS] <SERIES_KEY> <POINT_ID>
```

```bash
aether history latest inst:9:M 101
```

### 历史查询

查询某个点的历史数据。
```
Usage: aether history query [OPTIONS] <SERIES_KEY> <POINT_ID>
```

| 标志 | 描述 |
|------|-------------|
| `--from <FROM>` | 开始时间（ISO 8601，例如 `2026-05-12T00:00:00Z`，或相对的，例如 `-1h`） |
| `--to <TO>` | 结束时间（ISO 8601，默认为现在） |
| `--page <PAGE>` | 页码，从 1 开始（默认值：1） |
| `--size <SIZE>` | 页面大小，每页最大行数（默认值：100） |
```bash
aether history query inst:9:M 101 --from 2026-05-01T00:00:00Z
```

### 历史频道

列出历史已知的频道。
```
Usage: aether history channels [OPTIONS]
```

```bash
aether history channels
```

### 历史指标

显示历史存储指标（行数、数据范围等）。
```
Usage: aether history metrics [OPTIONS]
```

```bash
aether history metrics --json
```

### 历史健康状况

检查历史服务健康状况。
```
Usage: aether history health [OPTIONS]
```

```bash
aether history health
```

### 历史批量

一次请求批量查询多个点的历史数据（最多20个系列）。
```
Usage: aether history batch [OPTIONS] --from <FROM>
```

| 标志 | 说明 |
|------|-------------|
| `--series <KEY,POINT_ID>` | 要查询的系列，格式`series_key,point_id`（可重复，最多 20 个） |
| `--from <FROM>` | 开始时间（ISO 8601，例如`2026-05-01T00:00:00Z`) |
| `--to <TO>` | 结束时间（ISO 8601，默认为现在） |
| `--limit <LIMIT>` | 每个系列返回的最大数据点（默认 1000，最多 5000） |
```bash
aether history batch --series inst:9:M,101 --series inst:9:M,102 \
  --from 2026-05-01T00:00:00Z --limit 500
```

## aether top

用于实时监控的交互式 TUI 仪表板。没有特定于命令的标志。
```
Usage: aether top [OPTIONS]
```

```bash
aether top
```

## aether mcp

运行 MCP 服务器，将 `aether` 的功能作为工具公开。服务器通过 stdio 使用 MCP JSON-RPC；全局 `--json` 标志不会更改其输出。
```
Usage: aether mcp [OPTIONS]
```

| 标志 | 描述 |
|------|-------------|
| `--allow-write` | 将 22 个受管写工具添加到 23 个始终注册的只读工具中。这只是一个登记门；每次调用仍需要 `confirmed: true` |
```bash
aether mcp --allow-write
```

22 次写入是通道 CRUD/生命周期（`channels_create`、`channels_update`、`channels_delete`、`channels_enable`、`channels_disable`、`channels_reconcile`）； `models_instances_action`、`rules_execute`；规则 CRUD 和生命周期（`rules_create`、`rules_update`、`rules_delete`、`rules_enable`、`rules_disable`）；警报规则 CRUD 和生命周期（`alarms_rule_create`、`alarms_rule_update`、`alarms_rule_delete`、`alarms_rule_enable`、`alarms_rule_disable`）；手动警报解决方案 (`alarms_resolve`)；和行动路线治理（`routing_action_upsert`、`routing_action_delete`、`routing_action_set_enabled`）。因此，支持写入的目录总共有 45 个工具。

MCP 桥读取 `AETHER_ACCESS_TOKEN`，将其作为 `Authorization: Bearer` 凭据发送，并为每个受管控的 HTTP 请求生成一个 `X-Request-ID`。保留返回的 `request_id`/`command_id` 值，并且在超时或不完整的审核或发布响应后不会自动重试写入；首先检查状态和审计记录。通道突变成功可能包含降级的运行时间预测；使用其 `request_id`、`resulting_revision` 和 `reconciliation_required` 而不是重试。

请参阅[连接 AI 助手](/guides/ai-assistants)，了解如何连接 MCP 客户端。

## 退出代码和 JSON 模式

观察到的 `aether` 行为0.4.0:

- **退出 0** — 操作成功。
- **退出 1** — 操作失败（例如，目标服务无法访问）。在普通模式下，错误打印为 `Error: <message>`。
- **退出码 2** — 命令行使用错误（未知子命令或标志）；`clap` 将错误和使用提示写入标准错误。

使用 `--json`，结果将作为单个信封发送到 stdout，诊断信息将发送到 stderr：
```json
{ "success": true, "data": { "...": "..." } }
```

如果失败，信封会携带错误消息，并且进程会以代码 1 退出：
```json
{ "success": false, "error": "error sending request for url (...): tcp connect error: Connection refused" }
```

`--json` 还抑制横幅和彩色输出，这使其成为脚本和 AI 代理的推荐模式。如上所述，`mcp` 命令会忽略它。

## 相关页面

- [入门](/guides/getting-started) — 构建、初始化和启动 Aether
- [连接 AI 助手](/guides/ai-assistants) — 将智能体连接到 CLI 和 MCP 服务器
- [系统架构](/concepts/architecture) — 这些命令管理的服务
