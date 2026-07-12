# AetherEMS 开发快速开始

本指南帮助新开发者在 30 分钟内搭建 AetherEMS 开发环境并运行第一个测试。

## 目录

- [系统要求](#系统要求)
- [快速开始（5 分钟）](#快速开始5-分钟)
- [完整开发环境搭建](#完整开发环境搭建)
- [项目结构](#项目结构)
- [开发工作流](#开发工作流)
- [常见问题](#常见问题)

---

## 系统要求

### 必需软件

| 软件 | 版本要求 | 安装命令 |
|------|----------|----------|
| **Rust** | 1.90+ | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` |

### 可选软件

| 软件 | 用途 | 安装命令 |
|------|------|----------|
| **Docker** | 完整 Compose 发行版与可选 profile | [下载 Docker Desktop](https://docs.docker.com/get-docker/) |
| **Node.js / pnpm** | 可选 Web 客户端 | `npm install -g pnpm` |
| **redis-cli** | 可选 Redis `StateMirror` 扩展调试 | `brew install redis` |
| **sqlitebrowser** | SQLite 可视化 | `brew install --cask db-browser-for-sqlite` |
| **just** | 任务运行器 | `brew install just` |

### 验证安装

```bash
# 检查所有依赖
rustc --version    # rustc 1.90.0+
# 以下仅在使用完整发行版或可选 Web 客户端时检查
docker --version
node --version
pnpm --version
```

---

## 快速开始（5 分钟）

```bash
# 1. 克隆项目
git clone https://github.com/EvanL1/Aether.git
cd Aether

# 2. 生成并应用 fail-safe 空站点计划（无需外部数据库）
cargo build --release -p aether
./target/release/aether --json setup
# 审阅 JSON 后，把 data.plan_id 原样填入：
./target/release/aether setup apply --plan-id <PLAN_ID>

# 3. 运行测试验证环境
cargo test --workspace

# 4. 验证无外部服务的最小 Edge SDK composition
cargo run -p aether-example-minimal-gateway
```

**成功标志：** 测试通过，最小示例输出 `Aether minimal gateway ready`。

---

## 完整开发环境搭建

### 步骤 1：环境配置

```bash
# 复制环境变量模板
cp .env.example .env
chmod 600 .env

# 编辑 .env：手工填入两个独立随机值（可用 openssl rand -hex 32）
# JWT_SECRET_KEY=<随机值>
# AETHER_BOOTSTRAP_ADMIN_PASSWORD=<另一个随机值>
# AETHER_CONFIG_PATH=./data/config
# AETHER_DATA_PATH=./data
# 仅启用可选 Redis StateMirror 扩展时才需要 AETHER_REDIS_URL
```

### 步骤 2：构建项目

```bash
# 构建所有包（首次需要较长时间）
cargo build --workspace

# 构建发布版本（性能更好）
cargo build --release --workspace
```

### 步骤 3：初始化全新开发站点

```bash
# 默认 setup 只读；源码默认路径已对齐 Compose 的 ./data/config 挂载
./target/release/aether --json setup

# 审阅计划后应用同一 ID
./target/release/aether setup apply --plan-id <PLAN_ID>
```

### 步骤 4：启动完整组合（可选）

```bash
# Compose 引用预构建镜像；先按 deployment.md 构建或加载 aetherems:latest。
# 默认仅启动六个 Rust 进程，不启动前端或外部数据库。
docker compose up -d

# 可选扩展必须显式选择 profile
docker compose --profile redis up -d
docker compose --profile postgres-storage up -d

# 验收本地六进程、SQLite 与 SHM writer heartbeat
./target/release/aether doctor
```

### 步骤 5：运行测试

```bash
# 运行所有测试
cargo test --workspace

# 运行特定包的测试
cargo test -p aether-io
cargo test -p aether-automation
cargo test -p aether-rtdb

# 运行一个实际存在的集成测试目标
cargo test -p aether-example-minimal-gateway --test composition_contract
```

### 步骤 6：启动前端

```bash
# 进入前端目录
cd apps

# 安装依赖
pnpm install

# 启动开发服务器
pnpm dev

# 访问 http://localhost:8080
```

---

## 项目结构

```
AetherEMS/
├── apps/                    # 前端应用（Vue 3 + Element Plus + ECharts）
│   ├── src/
│   │   ├── views/          # 页面组件
│   │   ├── components/     # 通用组件
│   │   └── api/            # API 客户端
│   └── package.json
│
├── services/                # 后端服务
│   ├── io/             # 通信服务 - 工业协议驱动、通道管理 (Rust)
│   │   ├── src/
│   │   │   ├── api/        # REST API 处理器
│   │   │   ├── core/       # 核心逻辑
│   │   │   └── protocols/  # 协议实现（10 种）
│   │   └── Cargo.toml
│   │
│   ├── automation/             # 模型服务 - 产品定义、设备实例、规则引擎 (Rust)
│   │   ├── src/
│   │   │   ├── api/        # REST API 处理器
│   │   │   └── rule_routes.rs  # 规则 API
│   │   └── Cargo.toml
│   │
├── history/             # 历史数据服务 - 默认 SQLite，PostgreSQL 可选 (Rust)
│   ├── api/     # API 网关 (WebSocket, JWT, Rust)
│   ├── uplink/         # 网络服务 (MQTT, Rust)
│   └── alarm/       # 告警管理 (Rust)
│
├── libs/                    # 13 个共享 Rust 库
│   ├── aether-core/       # 核心类型与编解码器（no_std）
│   ├── aether-model/      # 数据模型、产品定义
│   ├── aether-routing/    # 数据流路由
│   ├── aether-rtdb/       # 遗留可选镜像抽象（非实时权威面）
│   ├── aether-rtdb-shm/   # 共享内存 RTDB（零拷贝）
│   ├── aether-shm/        # 共享内存读写器
│   ├── aether-infra/      # 遗留基础设施辅助层（SQLite 与可选外部存储）
│   ├── aether-calc/       # 表达式求值引擎
│   ├── aether-rules/      # 规则引擎
│   ├── aether-sim/        # 波形生成器
│   ├── aether-schema-macro/ # SQL DDL 过程宏
│   ├── common/             # 服务引导与共享工具
│   └── errors/             # 统一错误类型
│
├── tools/
│   ├── aether/            # CLI 配置与服务管理工具
│   └── simulator/          # Modbus TCP/RTU 从站模拟器
│
├── firmware/                # 嵌入式固件原型（ARM/STM32）
│
├── data/                    # Compose 对齐的站点根目录
│   ├── config/              # setup 激活的 fail-safe 配置
│   │   ├── global.yaml
│   │   ├── io/io.yaml
│   │   └── automation/     # automation.yaml / instances.yaml
│   └── aether.db          # SQLite 配置数据库
│
├── docker-compose.yml       # Docker 服务定义
├── Cargo.toml              # Rust workspace 配置
└── .env.example            # 环境变量模板
```

---

## 开发工作流

### 日常开发命令

```bash
# 代码检查（格式 + clippy + 测试）
./scripts/quick-check.sh

# 仅格式化
cargo fmt --all

# 仅 clippy 检查
cargo clippy --workspace --all-targets

# 监视模式开发（需要 cargo-watch）
cargo watch -x "check --workspace"
```

### 服务开发

```bash
# 本地服务共同使用的 access-JWT 校验密钥（仅限当前开发终端）
export JWT_SECRET_KEY="${JWT_SECRET_KEY:-$(openssl rand -hex 32)}"

# 开发模式运行 aether-io（自动重载）
cargo watch -x "run -p aether-io"

# 调试日志
RUST_LOG=debug cargo run -p aether-io

# 详细协议日志
RUST_LOG=io::protocols=trace cargo run -p aether-io
```

### 配置更改流程

```bash
# 1. 编辑配置文件
vim data/config/io/io.yaml

# 2. 验证配置（不实际同步）
./target/release/aether sync --dry-run

# 3. 同步到数据库
./target/release/aether sync

# 4. 热加载服务
curl -X POST http://localhost:6001/api/channels/reload
```

### 数据库操作

```bash
# 查看数据库状态
./target/release/aether status --detailed

# 导出配置备份
./target/release/aether export --output backup/

# 仅重建可丢弃的本地开发站点（绝不要用于已投运数据）
rm -rf data
./target/release/aether --json setup
# 审阅后使用 JSON 中的新 plan ID 执行 setup apply
```

### Git 工作流

```bash
# 创建功能分支
git checkout -b feature/my-feature main

# 提交前检查
./scripts/quick-check.sh

# 提交
git add .
git commit -m "feat: add new feature"

# 推送并创建 PR
git push -u origin feature/my-feature
```

---

## 常见问题

### Q: 可选 Redis 镜像连接失败

```
Error: Failed to connect to Redis at redis://localhost:6379
```

核心服务不依赖 Redis；未启用镜像时可忽略这类扩展诊断。确实需要镜像时：

```bash
# 显式启动可选 profile
docker compose --profile redis up -d

# 检查可选容器
docker compose --profile redis ps
```

### Q: 数据库初始化失败

```
Error: Failed to initialize database
```

**解决方案：**
```bash
# 检查权限
ls -la data/

# 对全新开发目录重新生成只读计划；不要覆盖现有站点
./target/release/aether --json setup
```

### Q: 端口被占用

```
Error: Address already in use (os error 48)
```

**解决方案：**
```bash
# 查找占用端口的进程
lsof -i :6001
lsof -i :6002

# 终止进程
kill -9 <PID>

# 或更改端口（在 .env 中配置）
```

### Q: Cargo 构建缓慢

**解决方案：**
```bash
# 使用 mold 链接器（Linux）
cargo install mold
RUSTFLAGS="-C link-arg=-fuse-ld=mold" cargo build

# 使用增量编译
export CARGO_INCREMENTAL=1

# 使用 sccache
cargo install sccache
export RUSTC_WRAPPER=sccache
```

### Q: 测试失败但本地服务正常

**说明：** 默认测试使用内存或 SHM fixture，不需要 Redis。

**解决方案：**
```bash
# 确保测试隔离
cargo test -- --test-threads=1

# 清除误设的可选镜像地址，检查是否有环境依赖
unset AETHER_REDIS_URL
cargo test
```

### Q: 前端无法连接后端

**解决方案：**
```bash
# 检查后端服务
curl http://localhost:6001/health
curl http://localhost:6002/health

# 检查 CORS 配置
# 确保后端允许 http://localhost:8080

# 检查前端 API 配置
cat apps/.env.local
# VITE_API_BASE_URL=http://localhost:6001
```

---

## 下一步

- 阅读 [API 参考文档](./API_REFERENCE.md) 了解所有 API
- 阅读 [Aether CLI 指南](./AETHER_CLI_GUIDE.md) 掌握管理工具
- 阅读 [配置格式指南](./CONFIG_FORMAT_GUIDE.md) 理解配置系统
- 查看 `CLAUDE.md` 了解代码规范和约束

---

## 联系支持

- **Issues**: https://github.com/EvanL1/Aether/issues
- **文档**: https://github.com/EvanL1/Aether/tree/main/docs
