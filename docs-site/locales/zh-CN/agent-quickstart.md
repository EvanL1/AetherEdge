---
title: "智能体快速入门"
description: "安装 AetherEdge 技能，启动默认安全的边缘运行时，并让智能体先读取能力再提出变更。"
---

本页面面向能够操作命令行的代码智能体。每一步都给出命令和成功标准，方便智能体在确认当前步骤完成后再继续。

## 1. 在应用仓库中安装 AetherEdge 技能

请在智能体将要操作的应用仓库中运行：

~~~bash
npx skills add EvanL1/AetherEdge -s aether-iot
~~~

如果代码助手没有自动重新加载项目技能，请重新启动对应会话。

**成功标准**：代码助手把 `aether-iot` 列为可用技能。

## 2. 安装 aether 命令行工具

开发阶段最直接的方式是从源码构建：

~~~bash
cargo build --release -p aether
sudo cp target/release/aether /usr/local/bin/aether
~~~

如果构建失败，请先检查[入门指南](/guides/getting-started/)中的环境要求。

有正式发布版本时，也可以从 GitHub 发布页下载与系统匹配的压缩包，并在解压前验证校验值：

| 系统 | 文件 |
| --- | --- |
| Linux arm64 | aether-linux-aarch64.tar.gz |
| Linux x86_64 | aether-linux-x86_64.tar.gz |
| macOS arm64 | aether-darwin-aarch64.tar.gz |
| Windows x86_64 | aether-windows-x86_64.zip |

~~~bash
REPO="EvanL1/AetherEdge"
ASSET="aether-linux-x86_64.tar.gz"   # 改成与当前系统匹配的文件名

TAG=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
  | grep -m1 '"tag_name"' | cut -d '"' -f4)

curl -fsSLO "https://github.com/$REPO/releases/download/$TAG/$ASSET"
curl -fsSLO "https://github.com/$REPO/releases/download/$TAG/$ASSET.sha256"
shasum -a 256 -c "$ASSET.sha256"

tar xzf "$ASSET"
chmod +x aether
sudo mv aether /usr/local/bin/aether
~~~

**成功标准**：运行 `aether --version` 后能够看到版本号，进程退出码为 0。

## 3. 生成并应用首次启动方案

先生成方案：

~~~bash
aether --json setup
~~~

从返回数据中读取 `data.plan_id`，然后原样应用这份方案：

~~~bash
aether setup apply --plan-id <PLAN_ID>
~~~

**成功标准**：命令返回的 JSON 中 `success` 为 `true`，进程退出码为 0。这个步骤只创建默认安全的空配置和本地 SQLite 状态，不会启动服务或启用设备。

## 4. 启动服务

AetherEdge 默认使用 Docker Compose 部署。先生成首次启动所需的两个密钥，再启动服务：

~~~bash
cp .env.example .env
chmod 600 .env

export JWT_SECRET_KEY="$(openssl rand -hex 32)"
export AETHER_BOOTSTRAP_ADMIN_PASSWORD="$(openssl rand -hex 32)"
sed -i.bak \
  -e "s/^JWT_SECRET_KEY=.*/JWT_SECRET_KEY=${JWT_SECRET_KEY}/" \
  -e "s/^AETHER_BOOTSTRAP_ADMIN_PASSWORD=.*/AETHER_BOOTSTRAP_ADMIN_PASSWORD=${AETHER_BOOTSTRAP_ADMIN_PASSWORD}/" \
  .env && rm .env.bak
unset JWT_SECRET_KEY AETHER_BOOTSTRAP_ADMIN_PASSWORD

aether services start
~~~

**成功标准**：`aether --json services status` 显示所有请求启动的服务都在运行。

如果本机还没有兼容的 `aetherems:latest` 运行时镜像，请先按照[部署指南](/guides/deployment/)构建或载入镜像。保留这个历史镜像名称，不表示 AetherEMS 属于当前仓库。

## 5. 检查运行状态

~~~bash
aether --json doctor
~~~

**成功标准**：返回数据中的 `success` 为 `true`，进程退出码为 0。

`doctor` 会检查 Docker 引擎、六个核心服务的健康接口、SQLite 数据库、四个必要配置文件和共享内存段。任何 `false` 或非零退出码都表示至少一项失败，应读取 `error` 字段定位原因。

## 6. 连接智能体客户端

默认只开放读取能力：

~~~bash
claude mcp add aether -- aether mcp
~~~

只有在会话确实需要设备控制或规则变更时，才启用受治理的写入能力：

~~~bash
claude mcp add aether -- aether mcp --allow-write
~~~

在连接真实硬件前，请先阅读[应用与智能体安全操作](/guides/safe-operations/)。

**成功标准**：客户端返回的工具列表中包含 `channels_list`。默认服务只提供读取工具。

`allow-write` 只会注册当前允许的受治理写入命令，并不等于用户已经确认操作。每次写入仍需明确传入 `confirmed: true`。不要自动重试结果不完整的写入；通道变更应保存 `request_id`、`resulting_revision` 和 `reconciliation_required`。更多连接方式请查看[连接智能助手](/guides/ai-assistants/)。

最后，可以向智能体提出下面的要求：

~~~text
开始使用 AetherEdge。请先以只读方式检查运行时，说明当前真实可用的应用能力；在我确认之前，不要提出或执行任何变更。
~~~
