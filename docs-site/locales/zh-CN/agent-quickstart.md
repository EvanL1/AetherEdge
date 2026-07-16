---
title: "智能体快速入门"
description: "安装AetherEdge技能，启动安全空运行时，并从零开始连接AI代理。"
---

此页面是为驱动 shell 的 AI 代理编写的，而不是为阅读散文的人类编写的。每个步骤都会说明命令和确切的信号，表示“此步骤成功，继续。”

## 1。从代理将运行的应用程序仓库安装 AetherEdge Skill

：
```bash
npx skills add EvanL1/AetherEdge -s aether-iot
```

如果编码助手没有自动重新加载项目技能，请重新启动它。

**成功标准：**助手将 `aether-iot` 列为可用技能。

## 2.安装 `aether` CLI

从源代码构建签出是最直接的开发路径：
```bash
cargo build --release -p aether
sudo cp target/release/aether /usr/local/bin/aether
```

如果构建失败，请参阅[入门](/guides/getting-started/)了解先决条件。

一旦存在标记版本，预构建的二进制文件会更快 - 从 GitHub Releases 下载，验证其校验和，然后提取它。选择与您的平台匹配的资产：

| 平台 | 资产 |
|---|---|
| Linux arm64 | `aether-linux-aarch64.tar.gz` |
| Linux x86_64 | `aether-linux-x86_64.tar.gz` |
| macOS arm64 | `aether-darwin-aarch64.tar.gz` |
| Windows x86_64 | `aether-windows-x86_64.zip` |
```bash
REPO="EvanL1/AetherEdge"
ASSET="aether-linux-x86_64.tar.gz"   # substitute your platform's asset name

TAG=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
  | grep -m1 '"tag_name"' | cut -d '"' -f4)

curl -fsSLO "https://github.com/$REPO/releases/download/$TAG/$ASSET"
curl -fsSLO "https://github.com/$REPO/releases/download/$TAG/$ASSET.sha256"
shasum -a 256 -c "$ASSET.sha256"

tar xzf "$ASSET"
chmod +x aether
sudo mv aether /usr/local/bin/aether
```

**成功标准：** `aether --version` 打印版本字符串并退出 0.

## 3.规划并应用首次运行配置
```bash
aether --json setup
```

从 JSON 输出中读取 `data.plan_id`。然后，在不修改中间站点的任何内容的情况下，应用该确切的计划：
```bash
aether setup apply --plan-id <PLAN_ID>
```

**成功标准：** apply 命令的 JSON 信封具有 `"success": true` 和退出代码 0。这永远不会启动服务或启用设备 - 它只会创建安全-空配置和本地 SQLite 状态。

## 4.启动服务

Aether 的默认部署是 Docker Compose。生成两个所需的首次启动密码，然后启动堆栈：
```bash
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
```

**成功标准：** `aether --json services status` 报告所有请求的服务正在运行。如果此计算机上尚不存在兼容性 `aetherems:latest` 运行时镜像，请参阅[部署](/guides/deployment/)——需要先构建或加载它，然后 `services start` 才能成功。保留的镜像名称不会使 EMS 产品成为此仓库的一部分。

## 5.验证健康状况
```bash
aether --json doctor
```

**成功标准：** 信封为 `{"success": true, ...}`，进程退出 0。`doctor` 检查 Docker 引擎、所有六个核心服务的运行状况路由、SQLite 数据库、四个必需的配置文件和共享内存段 — `false`/非零结果表示其中一个失败；读取 JSON `error` 字段以查找其中的一个。

## 6.连接 MCP 客户端
```bash
claude mcp add aether -- aether mcp
```

对于需要执行写操作（设备控制、规则变更）的会话，在针对真实硬件操作前，请先阅读[应用与智能体安全操作](/guides/safe-operations/)：
```bash
claude mcp add aether -- aether mcp --allow-write
```

**成功标准：** 客户端的 `tools/list` 响应包含 `channels_list`。默认服务器仅公开读取工具。`--allow-write` 注册当前受管的写入允许列表，但该标志不代表命令已确认：每次调用仍然需要 `confirmed: true`；签名的 `AETHER_ACCESS_TOKEN` 会作为承载凭证发送，桥接层还会添加请求 ID。切勿自动重试结果不完整的写操作。对于通道变更，应保留 `request_id`、`resulting_revision` 和 `reconciliation_required`；期望状态提交成功后，运行时投影仍可能处于降级状态。请参阅[连接 AI 助手](/guides/ai-assistants/)了解 Claude Desktop 配置和远程运行时连接方法。

现在询问助手：
```text
Get started with AetherEdge. Inspect the runtime in read-only mode and explain
which application capabilities are available before proposing any changes.
```
