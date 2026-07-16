---
title: "入门"
description: "构建工作区、初始化配置、启动服务并验证运行状况"
updated: 2026-07-10
---

# 入门

本指南将带您从全新克隆到正在运行、未调试的 Aether 系统：构建 `aether` CLI、应用经过审核的安全清空设置计划、启动服务并确认运行时正常。

## 先决条件

- **Rust** — 工具链通过以下方式固定到 `1.90.0` `rust-toolchain.toml`； rustup 会在第一次构建时自动安装它。该 pin 还声明用于边缘构建的 `aarch64-unknown-linux-musl` 交叉编译目标。
- **Docker Engine 和 Docker Compose** — 容器组合所需。 `aether services start` 在底层驱动 Docker Compose。 Redis 和 PostgreSQL 不是先决条件。

## 构建和配置

构建 `aether` CLI：
```bash
cargo build --release -p aether
```

将二进制文件安装到您的 PATH 中 — `cp target/release/aether /usr/local/bin/` 或 `cargo install --path tools/aether` — 因此本指南和其他所有指南都可以将其作为裸 `aether` 进行调用。

仓库在 `config.template/` 中提供了一个故障安全空配置。在源签出中，CLI 和 `docker-compose.yml` 默认情况下均使用 `./data/config` 和 `./data`。 Planning 始终是只读的，不会创建任一目录：
```bash
aether --json setup
```

从 JSON 输出中读取 `data.plan_id`，查看列出的操作，然后显式应用完全相同的未更改计划：
```bash
aether setup apply --plan-id <PLAN_ID>
```

仅对新站点或四个分发文件的确切安全子集接受“应用”。在进行任何持久写入之前，Aether 会暂存完整的配置，针对临时 SQLite 数据库运行正常验证和完整原子同步，然后仅创建丢失的文件而不覆盖。它会初始化 `aether.db` 并同步空运行时，但不会启动服务、启用设备或规则或安装域包。如果站点在规划后发生更改，则规划 ID 会过时，并且应用会停止而不进行写入。在生成的 `safe_ready` 站点上重新运行安装程序是无操作的。

会报告现有/自定义站点，但不会由安装程序重写。操作员仍然可以使用 `aether init` 进行显式架构迁移，使用 `aether sync` 进行显式配置应用； `aether sync --dry-run` 验证相同的嵌套文件，而不更改已安装的数据库。

CLI 按以下顺序独立解析每个路径：命令行标志、`AETHER_CONFIG_PATH`/`AETHER_DATA_PATH`、`/etc/aether/install.yaml`，然后是当前签出的 `data/config/` 和 `data/`。安装的包会自动写入上下文文件。如果没有该上下文，Aether 绝不会仅仅因为旧安装目录的存在而采用它。

对于全新的手动 Compose 部署，请创建一个私有环境文件并在验证组合之前填写两个首次启动密钥。打包安装程序会自动执行此操作；仓库设置故意将秘密保留在配置模板之外。
```bash
cp .env.example .env
chmod 600 .env

random_hex_32() {
  if command -v openssl >/dev/null 2>&1; then
    openssl rand -hex 32
  else
    od -An -N32 -tx1 /dev/urandom | tr -d ' \n'
  fi
}
export JWT_SECRET_KEY="$(random_hex_32)"
export AETHER_BOOTSTRAP_ADMIN_PASSWORD="$(random_hex_32)"

env_tmp="$(mktemp ./.env.tmp.XXXXXX)"
chmod 600 "$env_tmp"
awk '
  /^JWT_SECRET_KEY=/ {
    print "JWT_SECRET_KEY=" ENVIRON["JWT_SECRET_KEY"]; next
  }
  /^AETHER_BOOTSTRAP_ADMIN_PASSWORD=/ {
    print "AETHER_BOOTSTRAP_ADMIN_PASSWORD=" ENVIRON["AETHER_BOOTSTRAP_ADMIN_PASSWORD"]; next
  }
  { print }
' .env > "$env_tmp"
mv "$env_tmp" .env

JWT_SECRET_KEY="$JWT_SECRET_KEY" \
  AETHER_BOOTSTRAP_ADMIN_PASSWORD="$AETHER_BOOTSTRAP_ADMIN_PASSWORD" \
  docker compose config --quiet
unset JWT_SECRET_KEY AETHER_BOOTSTRAP_ADMIN_PASSWORD
```

保持`JWT_SECRET_KEY`稳定。使用生成的引导值以 `admin` 身份登录，立即更改密码，然后从 `.env` 中删除 `AETHER_BOOTSTRAP_ADMIN_PASSWORD`。公共注册保持关闭状态，因为示例设置了 `AETHER_ALLOW_PUBLIC_REGISTRATION=false`。

## 启动并验证
```bash
aether services start
aether doctor
```

`aether services start` 调出 Docker Compose 堆栈。撰写文件引用预先构建的图像；在尚未安装 `aetherems:latest` 的计算机上，通过运行 `./scripts/build-installer.sh`（从交叉编译的二进制文件构建映像）来生成它，或使用 `docker load` 加载预构建的映像存档 — 请参阅[部署](/guides/deployment)。

`aether doctor` 检查所需的本地运行时，如果有任何必需的组件，则以非零值退出失败：

1. **Docker 引擎** — 守护进程已安装并运行。
2. **六项核心服务** — IO、自动化、历史记录、API、上行链路和警报回答其特定于服务的运行状况路由。可选的云或存储依赖项可能会报告降级，但不会成为核心故障。
3. **SQLite 数据库** — `aether.db` 存在，已初始化，并显示其上次同步时间。
4. **配置文件** — `global.yaml`、`io/io.yaml`、`automation/automation.yaml` 和 `automation/instances.yaml` 存在。
5. **共享内存** — 存在段文件 `/dev/shm/aether-rtdb.shm` 存在，并且具有可读、有效的数据平面标头和新的 IO 写入器心跳。丢失、陈旧、截断、符号链接或无效的 SHM 都是错误，因为它是权威的活动状态平面。当安装故意使用其他位置时，`AETHER_SHM_PATH` 会覆盖平台默认值。

一切正常时，这些端口正在侦听（有关每个服务的作用，请参阅[系统架构](/concepts/architecture)）。打包组合仅远程公开经过身份验证的 API 网关；其他五个进程API监听`127.0.0.1`：

| 服务 | 端口 |
|---------|------|
| aether-io | 6001 |
| aether-automation | 6002 |
| aether-history | 6004 |
| aether-api | 6005 |
| aether-uplink | 6006 |
| aether-alarm | 6007 |

AetherEdge故意不公开任何捆绑的 Web UI。 AetherEMS 等产品控制台是独立部署的，并通过 `aether-api` 进入。

## 先看看

默认模板故意不包含设备通道或实例，因此这些命令最初应返回空集合：
```bash
# 1. The communication channels aether-io is polling
aether channels list

# 2. The device instances aether-automation is serving
aether models instances list

# 3. Confirm that no control rule was activated implicitly
aether rules list
```

每个命令都接受 `--json` 进行结构化输出，这是 AI 代理和脚本应使用的模式。仅在显式调试步骤添加并启用通道后，数据才开始流动；继续连接设备。

## 后续步骤

- [连接设备](/guides/connect-devices) — 添加真实通道并将其点映射到实例
- [写入规则](/guides/writing-rules) — 使用规则引擎自动控制
- [AI 助手](/guides/ai-assistants) — 通过 AI 驱动 Aether agent
- [部署](/guides/deployment) — Docker Compose 详细信息和边缘安装程序
