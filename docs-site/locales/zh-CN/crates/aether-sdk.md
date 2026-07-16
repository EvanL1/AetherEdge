---
title: "aether-edge-sdk"
description: "用于嵌入 Aether AI 原生物联网边缘内核的版本测试版外观。"
updated: 2026-07-16
---

# aether-edge-sdk

版本化 beta 外观，用于嵌入 Aether AI 原生物联网边缘内核。

API 已针对打包和 SemVer 兼容性进行了版本控制，但第一个独立注册表版本尚未完成。在此之前，请从固定仓库修订版本中使用它，而不是假设 crates.io 可用性。

Rust 库目标导入为 `aether_sdk`。 `AetherBuilder` 没有具体的基础架构默认值。主机显式提供权威的实时状态、设备命令调度程序和强制审核接收器。这使得 Redis、PostgreSQL、SQLx、Web 框架和协议驱动程序远离 SDK 的默认依赖关系图。

`aether_sdk::pack` 外观公开版本化、故障关闭的域包清单加载器。加载包会验证兼容性和受限资源目录；它从不安装或调试该包。

可选的 `local-runtime` 功能在 `aether_sdk::local` 下公开零外部服务适配器。下游应用程序仅依赖于这个外观；工作区的域、端口、应用程序和适配器包是源模块，不定义独立的注册表产品。
```toml
[dependencies]
aether-sdk = { package = "aether-edge-sdk", git = "https://github.com/EvanL1/AetherEdge.git", tag = "v0.5.0", features = ["local-runtime"] }
```

有关可运行的零外部服务组合，请参阅仓库的 [`examples/minimal-gateway`](https://github.com/EvanL1/AetherEdge/tree/main/examples/minimal-gateway)。
```bash
cargo test -p aether-edge-sdk
cargo test -p aether-edge-sdk --features local-runtime
cargo run -p aether-example-minimal-gateway
```

您可以选择根据 MIT 或 Apache-2.0 获得许可。
