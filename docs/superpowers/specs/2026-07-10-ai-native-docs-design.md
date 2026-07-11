# AI-Native 文档体系设计文档

- **日期**: 2026-07-10
- **状态**: 已定稿，待实施
- **前置**: `../../reference/mcp-tools.md`（MCP server 已落地；工具清单以生成的正式参考为准）

## 目标

为 AetherEMS 建立一套 Neon 风格的、独立的英文文档体系，服务两类 AI 受众：

1. **AI coding agent**（Claude Code / Codex 等）——在仓库里工作时可检索的深度知识层：架构决策、领域模型、"为什么这样设计"
2. **终端用户的 AI 助手**（Claude Desktop 等经 `aether mcp` 连入）——通过 MCP resources 获得领域上下文，理解 51 个工具背后的储能领域语义，而不只是工具描述里那一行话

核心定位：**储能行业 know-how 是这套文档的主体**，软件架构是支撑。一个连上 MCP 的 AI 助手需要知道"PCS 是什么、SOC 管理策略长什么样、哪些写入会动真实设备"，才能安全且有用地操作系统。

同时 README 重定位为 **AI-native EMS**。

## 决策记录

| 决策 | 选择 | 理由 |
|---|---|---|
| 交付形式 | 纯 repo 内 Markdown + 根目录 `llms.txt` | 受众是 AI，不需要文档站点/静态生成器；符合 YAGNI |
| 与 CLAUDE.md 关系 | **零交叉**。CLAUDE.md 继续做仓库内开发约束，不动、不引用、不迁移 | 两者受众和职责完全不同（开发约束 vs 产品/领域文档） |
| 与现有中文 docs 关系 | 零交叉。现有 `docs/*.md` 中文文件保留不动 | 新体系独立，像 Neon 一样从零建 |
| 语言 | 英文 | Neon 即英文；AI token 效率更高；开源项目国际受众 |
| MCP 触达路径 | 扩展 `aether mcp` 新增 `resources/list` + `resources/read` | 真正符合 MCP 协议语义。**推翻上个 spec 的"不加 resources"决定**——当时排除是因为无内容可暴露，现在有了 |
| 资源内容来源 | 编译期 `include_str!` 内嵌进二进制 | 边缘设备上没有 repo；文档随二进制走，版本天然对齐（AI 读到的说明永远与它能调的工具行为一致） |
| 内嵌方式 | 静态常量数组，不引入 rust-embed | ~15 个文件，一个 `&[(&str, &str)]` 足够 |
| 手写 vs 生成 | 全部手写，唯一例外 `reference/mcp-tools.md` 由脚本从二进制 `tools/list` 生成 | 领域概念只能手写；51 工具参考是漂移风险最高的页面且真源就是二进制 |
| llms.txt 维护 | 手工维护 | ~20 行，不值得生成脚本 |

## 目录结构与页面清单

```
llms.txt                          ← 仓库根，全文集索引（每页一行：路径 + description）
docs/
  domain/                         ★ 储能行业 know-how（核心支柱）
    ess-primer.md                 储能电站基础：PCS/BMS/电芯簇、SOC/SOH、并网接口，
                                  以及这些概念如何映射到 Aether 的 product/instance/point 模型
    product-models.md             13 个内置产品模型详解（Station/ESS/PCS/Battery/PVInverter/
                                  PV_DCDC/Diesel/Generator/EVChargingLoad/HVACLoad/Load/
                                  Load_Three_Phase/Env）：层级关系、每个产品的 M/A 点位语义、
                                  SunSpec 对应关系
    control-strategies.md         控制策略如何表达为规则：SOC 管理（battery_soc_management
                                  模板逐节点讲解）、削峰填谷、需量控制的建模思路
    safe-operations.md            安全操作语义：哪些写入直达真实设备（channels write / control）、
                                  写门禁哲学（--allow-write 注册期门禁 vs read_only_hint）、
                                  NaN 哨兵与 comsrv:online 的正确解读、AI 操作员守则
  concepts/                       系统架构（"为什么这样设计"）
    architecture.md               7 服务拓扑、端口、通信机制（SHM/UDS/Redis/HTTP/MQTT）、启动顺序
    data-model.md                 Instance 纯物模型（四份正交数据、不染色原则）、T/S/C/A 点类型、
                                  product → instance → point 层级
    shared-memory.md              SHM 布局、写者所有权（类型强制）、seqlock、generation、
                                  rebuild-via-swap、PointWatch 事件平面
    rule-engine.md                双列存储不变量、Scheduler（tick vs OnChange）、Executor、热加载
    data-flow.md                  上行/下行全链路 + 延迟预算、ShmRedisSync、Redis 键空间摘要
  guides/                         操作指南（"怎么做"）
    getting-started.md            build → init → sync → services start → doctor
    connect-devices.md            channel 配置、14 协议与 feature gate、点位到 instance 的映射
    writing-rules.md              规则编写（HTTP API 与 Vue Flow UI 两条路径）
    ai-assistants.md              连接 Claude Desktop / Claude Code 到 aether mcp：
                                  配置示例、只读 vs --allow-write、安全模型
    deployment.md                 docker compose、build-installer、边缘设备部署
  reference/                      参考（"是什么"）
    cli.md                        aether CLI 命令参考
    mcp-tools.md                  ★ 生成页：51 工具按域分组（名称/描述/read-only 标记/参数 schema）
    configuration.md              配置 schema（config/*.yaml → aether sync → SQLite）、环境变量
    http-api.md                   响应信封约定 + JWT 认证 + 端点概览（不逐端点抄写，指向 Swagger）
```

共 18 页 + llms.txt + README 改造。

## 页面规范

每页统一 frontmatter：

```yaml
---
title: Instance Data Model
description: Why instances are pure thing-models with no status field
updated: 2026-07-10
---
```

`description` 一处三用：llms.txt 索引行、MCP `resources/list` 的资源描述、页面定位。

写作基准：每页回答一个问题；领域页必须落在真实代码/配置素材上（`products/*.json`、`battery_soc_management.json`、`mcp.rs` 写门禁实现），不写行业空话。

## llms.txt 格式

遵循 llmstxt.org 约定，置于仓库根：

```
# AetherEMS

> AI-native industrial energy management system built in Rust. ...

## Domain Knowledge
- [ESS Primer](docs/domain/ess-primer.md): <description>
...

## Concepts
...

## Guides
...

## Reference
...
```

## MCP resources 扩展

- `tools/aether/src/mcp.rs` 实现 rmcp `ServerHandler` 的 `list_resources` + `read_resource`
- URI 方案：`aether://docs/<section>/<page>`，如 `aether://docs/domain/safe-operations`
- 暴露**精选子集**（11 个资源）：`domain/*`（4）+ `concepts/*`（5）+ `reference/mcp-tools.md` + `guides/ai-assistants.md`。部署/入门类 guides 对已连入的 AI 助手无意义，不暴露
- resources 只读，**不受 `--allow-write` 门禁影响**，两种模式下都注册
- 内容内嵌：`include_str!` 静态数组 `&[(&str /* uri */, &str /* title */, &str /* description */, &str /* body */)]`
- MIME type: `text/markdown`
- 测试：`resources/list` 返回 11 项且每项 URI 可 `resources/read`；stdout 纯净性测试（复用现有 `test_json_output_is_clean` 模式）覆盖 resources 方法

## mcp-tools.md 生成脚本

`scripts/gen-mcp-docs.sh`：

1. 向 `aether mcp --allow-write` 管道送 `initialize` + `tools/list`
2. 渲染为按域分组（net/alarms/channels/rules/routing/history/models/templates）的 markdown：工具名、描述、read-only/write 标记、参数 schema 要点
3. 产物 `docs/reference/mcp-tools.md` 提交进 repo
4. 工具变更后手动重跑；不进 CI，不做新鲜度检查

脚本先 `cargo build -p aether` 确保二进制新鲜。生成页同样带 frontmatter，`updated` 取脚本运行日期（`date +%F`）。

## README 重定位

- `README.md` headline 改为强调 **AI-native EMS**：首段和 Features 首条突出 MCP server（51 tools、注册期写门禁）、MCP resources（领域知识内嵌二进制）、llms.txt / AI 可摄入文档
- 增加 "AI-Native" 章节：Claude Desktop 配置片段、`aether mcp` / `aether mcp --allow-write` 说明
- `README-CN.md` 同步修改（这对既有双语文件保持一致，不算新增中文镜像）

## 明确不做

- 不建文档站点/静态生成器；不做 `llms-full.txt`；不做新文档的中文镜像
- 不迁移/翻译/引用现有中文 `docs/*.md`；不改 CLAUDE.md
- 不做 OpenAPI/clap/config-schema 生成管线（记为 v2 方向）
- 不加 MCP prompts 能力；不做 `resources/subscribe`；不做 resource templates
- llms.txt 不上生成脚本

## 验收标准

1. 18 页全部就位，frontmatter 完整，无 TBD/占位符
2. 仓库根 `llms.txt` 索引 18 页，description 与页面 frontmatter 一致
3. `aether mcp` 的 `resources/list` 返回 11 项，每项可读，stdout 纯净性测试通过
4. `scripts/gen-mcp-docs.sh` 可重复执行且幂等（同一工具集产出相同内容，`updated` 除外）
5. README/README-CN 以 AI-native EMS 定位，含 MCP 接入示例
6. `./scripts/quick-check.sh` 通过（mcp.rs 改动部分）
