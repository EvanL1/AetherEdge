# 裸机 Linux 安装与启动（无 Docker）设计文档

- **日期**: 2026-07-10
- **状态**: 已定稿，待实施
- **关联**: 文档体系 spec（`2026-07-10-ai-native-docs-design.md`）——本功能落地后须更新 `docs/guides/deployment.md`

## 目标

在没有 Docker 的 Linux 目标机（arm64 / amd64）上安装并运行完整 AetherEMS：6 个 Rust 服务 + Redis + Web UI，由 systemd 管理，一个自包含 `.run` 安装包完成部署。目标机零系统依赖（无需 Docker、无需发行版包管理器装任何东西）。

## 背景：当前部署链全部绑定 Docker

- `aether services start/stop/refresh` 底层直接调 `docker compose`（`tools/aether/src/services.rs:551-566`）
- `.run` installer 是 makeself 打包的 **Docker 镜像** + 1200 行 Docker 中心的 `scripts/install.sh`
- `aether doctor` 含 Docker Engine / 容器检查（`doctor.rs:99,126`）
- Web UI 由 nginx 容器托管，且 `apps/nginx.conf` **不是纯静态托管**——它是多服务反向代理：`/api/`→apigateway:6005、`/alarmApi/`→:6007、`/modApi/`→:6002、`/comApi/`→:6001，外加 `auth_request` JWT 外层校验与 WebSocket 代理；前端 `baseURL: ''` 全部同源调用
- 仓库中没有任何 systemd unit

## 决策记录

| 决策 | 选择 | 理由 |
|---|---|---|
| 进程监管 | systemd units + `aether.target` | 工业边缘标准做法：开机自启、崩溃重拉、journald 日志；依赖顺序用 unit 依赖表达 |
| 目标平台 | arm64 + amd64（与现有 installer 对齐） | 复用 build-installer.sh 现有 musl 静态交叉编译 |
| Redis | 构建机上用 Docker 交叉编译**静态 redis-server**，打进 `.run` | 目标机零依赖；构建机本就要求 Docker（现有 installer 如此），目标机保持无 Docker |
| Web UI | 同法捆绑**静态 nginx** + 现有 `apps/nginx.conf` 原样复用 | 行为与 Docker 模式字节级一致；零 Rust/前端改动。已否决"apigateway 加 ServeDir"——经核实前端依赖 nginx 的多服务反代（见背景），改造 apigateway 为 ingress 是数百行新子系统 |
| installer 形态 | `build-installer.sh --bare-metal` 产出独立变体 `.run`，内含**新的精简 `scripts/install-baremetal.sh`** | 1200 行 Docker 版 `install.sh` 不动，避免双模式纠缠 |
| CLI 适配 | `aether services` / `aether doctor` 增加 **systemd 模式**，运行时自动检测 | 检测规则：`/etc/systemd/system/aether.target` 存在且 `systemctl` 可用 → systemd 模式；否则维持现有 Docker 行为 |
| 运行身份 | root | 边缘设备惯例；GPIO/CAN/网络配置本就需要特权。不引入专用用户 |
| 非 systemd 发行版 | v1 不支持 | YAGNI；Alpine/OpenWrt 等场景后续再说 |

## 安装布局

```
/opt/aether/bin/          comsrv modsrv hissrv apigateway netsrv alarmsrv aether
                          redis-server nginx        （静态二进制）
/opt/aether/apps/         前端 dist（nginx root）
/etc/aether/script-host/  main.py（comsrv 自定义 transform，python3 为可选依赖；路径由
                          script_runner.rs:375 硬编码的部署期查找顺序决定，不是
                          /opt/aether/ 下——不要挪地方）
/etc/aether/              config（install-baremetal.sh 从捆绑的 config.template/ 拷贝，
                          镜像 scripts/install.sh:1023-1040 的做法——aether init
                          只做 SQLite schema 迁移，不铺配置模板）+ nginx.conf +
                          aether.db（aether sync 产出）
/var/lib/aether/          运行数据（hissrv 本地文件等）
/etc/systemd/system/      aether-redis / aether-comsrv / aether-modsrv / aether-hissrv /
                          aether-apigateway / aether-netsrv / aether-alarmsrv /
                          aether-apps(nginx) 各 .service + aether.target
```

启动顺序：`aether-redis` → `aether-comsrv` → `aether-modsrv`（`After=`/`Requires=` 表达，叠加既有应用层 `wait_for_dependency` 双保险）；其余服务 `After=aether-redis`。全部 `WantedBy=aether.target`，`systemctl enable --now aether.target` 一键起停全站。

SHM 路径用 Linux 原生 `/dev/shm`（现有解析逻辑已支持，无改动）。

## 三块工作

### 1. `scripts/build-static-deps.sh` — 静态依赖构建（构建机）

- 用 Docker（alpine 构建容器 + musl）交叉编译 `redis-server`（Redis 8.x，`make REDIS_STATIC`）与 `nginx`（stable，`--with-cc-opt=-static` 风格最小模块集：仅需现 conf 用到的 http/proxy/gzip/auth_request/websocket 升级支持）
- 产物按 `build/cache/static-deps/<name>-<version>-<arch>/` 缓存，重复构建直接命中
- 独立脚本，`build-installer.sh --bare-metal` 调用它

### 2. installer 裸机变体

- `build-installer.sh` 增加 `--bare-metal` 分支：复用现有 Rust musl 交叉编译与 apps `npm run build`，跳过 Docker 镜像构建；打包内容 = 上述安装布局全部文件 + systemd unit 文件（仓库内静态模板 `scripts/systemd/*.service`）+ `install-baremetal.sh`；makeself 产出 `AetherEdge-baremetal-<arch>-<version>.run`
- `scripts/install-baremetal.sh`（新，精简）：检测 systemd → 铺二进制 → 首次安装时把捆绑的 `config.template/` 拷贝到 `/etc/aether/config/`（`aether init` 不做这一步）→ `aether init` + `aether sync` → 安装 units → `systemctl daemon-reload && systemctl enable --now aether.target` → 打印各服务状态。可重复执行（升级 = 覆盖二进制 + restart target，不覆盖已存在的 `/etc/aether/config/`）
- 同时写出 `/opt/aether/uninstall.sh`（stop/disable units、删 units、保留 `/etc/aether` 配置与数据）

### 3. `aether` CLI systemd 模式

- `services.rs`：抽出部署模式检测（`DeployMode::Docker | Systemd`）；systemd 模式下 start/stop/refresh/status 改调 `systemctl`（target 级 + 单服务级），Docker 路径行为不变。单元测试覆盖模式检测与命令映射（mock 不了 systemctl 就测参数构造函数）
- `doctor.rs`：systemd 模式下跳过 Docker Engine/容器检查，改查各 unit `is-active` 与 redis/端口连通性（端口检查逻辑现成可复用）

## 文档跟进（本功能 plan 的收尾任务）

- `docs/guides/deployment.md` 增加 "Bare-metal Linux (systemd)" 一节（installer 用法、布局、`aether services` 双模式说明）
- `docs/reference/cli.md` 若 services/doctor 帮助文本变化则同步
- 均以落地后的真实代码为准，遵守文档 corpus 的反幻觉规则

## 明确不做

- 不支持非 systemd 发行版；不做 `aether` 自 spawn/pidfile 进程模式
- 不捆绑 TimescaleDB/PostgreSQL（hissrv 默认 null backend，外部 PG 由用户自备并在配置里指）
- 不改 apigateway（不做 ingress 化）；不改 nginx.conf；不改前端
- 不做专用运行用户/权限收窄；不做 SELinux/AppArmor 适配
- 不动现有 Docker 版 `install.sh` 与 compose 流程

## 验收标准

1. `./scripts/build-installer.sh --bare-metal <ver> arm64`（及 amd64）产出自包含 `.run`；构建两次第二次静态依赖走缓存
2. 在无 Docker 的目标机执行 `.run`：全部 unit `active`，`aether doctor` 全绿（systemd 模式），浏览器访问 `:8080` UI 可登录、看板有数据
3. `aether services stop/start/refresh/status` 在 systemd 模式下语义与 Docker 模式对齐；Docker 环境行为回归不变（现有测试不破）
4. 重复执行 `.run` 完成升级且配置/数据保留；`uninstall.sh` 干净卸载并保留 `/etc/aether`
5. `./scripts/quick-check.sh` 通过
