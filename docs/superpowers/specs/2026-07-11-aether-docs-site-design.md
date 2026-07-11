# Aether 文档站设计 — 托管到 Cloudflare

- **日期**: 2026-07-11
- **状态**: 已定稿,待实施
- **前置**: 无

## 目标

把 Aether(内核)的文档托管成一个公开的文档站,部署在 Cloudflare 上。参考 Neon
(`neon.com/docs`)的文档模式,核心不是视觉上像素级模仿,而是复刻它对 LLM/agent
友好的具体机制:分类清晰的 `llms.txt` 索引、每页可拿到原始 markdown(URL 加
`.md` 后缀,或者用 `Accept: text/markdown` 请求头)、以及一个让 agent 能"照着做
就把系统跑起来"的入口页面。

## 非目标

- 不做 AetherEMS(能源发行版)文档站——先只做 Aether 内核这一份,AetherEMS 那份
  以后照抄这套方案单独做。
- 不追求视觉上和 Neon 逐像素一致。
- 不做多版本切换(项目仍在早期,只发布 `main` 分支的最新内容)。
- 不改变仓库现有的 CI/CD(`release.yml` 不动),只新增一个独立的部署工作流。
- 不给 Aether 自身(六个核心服务)新增任何运行时服务或依赖——文档站是完全独立
  的静态内容托管项目,不进 Cargo workspace,不影响 io/automation/api 等服务的
  部署和运行。下文提到的 Cloudflare Worker 仅仅是"文档站怎么被托管"这一层,和
  Aether 产品本身的架构无关。

## 架构总览

新增一个独立目录 `docs-site/`,是它自己的 npm 工程(和仓库现有的 `apps/`
Vue 前端平级、互不影响,不进 Cargo workspace)。技术选型:

- **Astro Starlight** 作为静态站点生成器——专门为技术文档设计,开箱有侧边栏
  导航、搜索、暗色模式。
- **`starlight-llms-txt`** 社区插件——按 Starlight 侧边栏分组结构自动产出
  `/llms.txt`(索引)、`/llms-full.txt`(全站内容拼接)、每页对应的 `/page.md`
  原始 markdown 文件。这三样正好对应 Neon 文档站验证过的三个具体特性。
- 部署到 **Cloudflare Workers 的静态资源模式**(不是 Cloudflare Pages——
  Cloudflare 目前的推荐路径是 Workers + Static Assets,Pages 本身也在向
  Workers 迁移)。因为部署形态是 Worker 而不是纯静态托管,才能加一段自定义
  `fetch` handler 实现"发 `Accept: text/markdown` 请求头拿 markdown"这个
  Neon 才有的细节功能。

## 内容范围与同步机制

内容不手工复制维护两份,而是构建时从仓库主 `docs/` 树里"抽取"一份到 Starlight
要求的 `docs-site/src/content/docs/` 目录,由一个小的 prebuild 脚本
(`docs-site/scripts/sync-content.mjs`)完成,读取 `docs-site/content.manifest.txt`
(纯文本,一行一个 glob)来决定收录哪些文件。

**收录**(内核通用内容):
- `docs/concepts/*`、`docs/guides/*`、`docs/reference/*`、`docs/architecture/*`、
  `docs/adr/*`(0001-0008)
- `docs/CONFIG_FORMAT_GUIDE.md`、`docs/GETTING_STARTED_DEVELOPMENT.md`、
  `docs/websocket-rule-monitor-api.md`、`docs/benchmarking.md`
- `docs/security/dependency-exceptions.md`
- 根目录 `AGENTS.md`、`ARCHITECTURE.md`
- `crates/*/README.md`、`extensions/*/README.md`

**不收录**:
- `docs/domain/*`(能源特定:control-strategies、ess-primer、product-models、
  safe-operations)
- `docs/plans/*`、`docs/superpowers/**`(内部规划/过程文档,包括这份 spec 自己)
- `docs/operations-log.md`(内部运维记录)
- `docs/AETHER_CLI_GUIDE.md`、`docs/API_REFERENCE.md`——两者都在正文里自称是
  "迁移期旧版指南",内容已被 `docs/reference/cli.md`、`docs/reference/http-api.md`
  取代,收录了反而造成两份互相矛盾的真源。

清单以后要调整,只改 `content.manifest.txt` 这一个文件,不用碰构建逻辑或
Astro 配置。

**frontmatter 缺失的处理**:抽查发现只有 `docs/concepts/*` 和 `docs/reference/*`
一致带 YAML frontmatter(`title`/`description`/`updated`),其余收录范围
(adr/、architecture/、根目录几份文档、`crates/*/README.md`、
`extensions/*/README.md`、`AGENTS.md`、`ARCHITECTURE.md`)基本都没有。Starlight
的内容集合(content collection)要求每页至少有 `title` 字段,没有就会构建失败,
所以这不是可选的润色项。同步脚本 `sync-content.mjs` 在拷贝时对没有 frontmatter
的文件自动补一段:`title` 取文件内第一个 `# 标题` 行,`description` 取标题后
第一段非空文本(截断到合理长度),`updated` 留空或用文件的 git 最后修改日期。
这样可以不去手工改仓库里几十个源文件,由同步脚本统一兜底。

## 新增页面:Agent Quickstart

`docs-site/src/content/docs/agent-quickstart.md`,内容是纯命令序列而非叙述性
文字,给 AI agent(而非人类)读的"从零跑起来"路径:

1. 安装 `aether` CLI(下载/校验/解压命令)
2. `aether --json setup` → 读计划 → `aether setup apply --plan-id <ID>`
3. `aether services start`
4. `aether doctor`(给出预期的健康输出示例,作为 agent 的成功判据)
5. 连接 `aether mcp`(给出 Claude Desktop/Claude Code 的配置片段,链接到
   `docs/guides/ai-assistants.md` 看细节)

每一步标注"预期输出/成功判据"。这个页面在 Starlight 侧边栏排最前面,因此
`starlight-llms-txt` 生成 `/llms.txt` 时它自然是第一条链接——对应 Neon 首页
把 quickstart 和 AI 工具接入放在最显眼位置的做法。

## LLM 友好特性(对照 Neon 验证过的具体机制)

| Neon 的做法 | 这次的实现 |
|---|---|
| `llms.txt` 开头一句"给 URL 加 `.md` 或发 `Accept: text/markdown`" | `starlight-llms-txt` 插件生成的 `llms.txt` 头部文案里写同样的提示 |
| `llms.txt` 按 ~15 个功能模块分类,每条 `- [标题](url.md): 描述` | 插件按 Starlight 侧边栏分组(concepts/guides/reference/adr/...)自动产出,描述取每页 frontmatter 的 `description` 字段 |
| 每页 URL 加 `.md` 拿纯 markdown | 插件构建时在 `dist/` 里为每页产出同名 `.md` 文件,静态直出 |
| 发 `Accept: text/markdown` 请求头也能拿 markdown | Cloudflare Worker 的 `fetch` handler 里加判断:请求头包含 `text/markdown` 时改发对应的 `.md` 资源,否则正常 `env.ASSETS.fetch()` |
| AI 工具接入入口显眼 | Agent Quickstart 页面 + llms.txt 第一条链接 |

注意仓库根目录已有一份 `llms.txt`(给在仓库里工作的编码 agent 看整体架构用),
和文档站生成的 `/llms.txt`(给消费公开文档站的 agent 看)是两个不同用途的文件,
不冲突、不合并。

## 部署

- `astro build` 产出纯静态 `dist/`。
- Wrangler 配置 `docs-site/wrangler.toml`:`[assets] directory = "./dist"`,
  加一个十几行的 `fetch` handler(上面 markdown content-negotiation 那段逻辑),
  不需要其他服务端逻辑。
- 项目名 `aether-docs`,先用 Cloudflare 默认分配的
  `aether-docs.<account>.workers.dev`;以后要绑自定义域名由你自己在
  Cloudflare 面板加,不在这次范围内。

## CI/CD

新增 `.github/workflows/docs-site-deploy.yml`(不改动现有 `release.yml`):

- 触发条件:push 到 `main`,且改动路径命中
  `docs/**`、`docs-site/**`、`AGENTS.md`、`ARCHITECTURE.md`、
  `crates/*/README.md`、`extensions/*/README.md` 之一。
- 步骤:checkout → 装 Node → `docs-site/` 下 `npm ci` → 跑内容同步脚本 →
  `astro build` → `wrangler deploy`。
- 需要的 GitHub secrets:`CLOUDFLARE_API_TOKEN`、`CLOUDFLARE_ACCOUNT_ID`
  (实施阶段给出精确的申请/配置步骤)。

## 测试与校验

- CI 里跑 `astro check`(类型/内容校验)和 Starlight 自带的死链检测,任何一项
  失败就让 CI 红,不会带着坏文档站悄悄部署上线。
- 本地开发用 `npm run dev` 预览。
- 部署后手动核实:`/llms.txt` 能访问且包含 Agent Quickstart 链接、任选一页
  `/somepage.md` 能拿到纯 markdown、发 `Accept: text/markdown` 请求头访问
  HTML 页面路径也能拿到 markdown。

## 后续可能的跟进(不在本次范围)

- AetherEMS(能源发行版)那份文档站,照抄这套方案单独做一遍。
- 自定义域名绑定。
- 多版本文档切换(等项目更成熟、需要维护多个已发布版本时再做)。
