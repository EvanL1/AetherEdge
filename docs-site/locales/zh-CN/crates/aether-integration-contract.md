---
title: "aether-integration-contract"
description: "AetherContracts 0.1.0-alpha.4 候选版集成数据协议的严格 Rust 产品绑定"
---

# aether-integration-contract

这个 Rust 包严格绑定 AetherContracts `0.1.0-alpha.4` 候选版中的实验性
`aether.integration` v1alpha1 协议。

它与传输方式无关，并提供：

- 闭合的蛇形命名拓扑和观测数据对象；
- 严格 JSON 解码和确定性的 RFC 8785 编码；
- 依赖拓扑的身份、引用、质量和值类型校验；
- `int64` 和 `uint64` 的无损字符串编码；
- 规范十进制和无填充 Base64url 校验；
- Aether Foundation 的二进制 64 位浮点数安全语义；
- 明确的 Home Assistant 来源地址与语义点位类型投影。

AetherEdge 绑定对完整消息实施 16 MiB 安全上限，比协议规定的可移植字段上限更严格。
随附测试按照 `0.1.0-alpha.4` 样例清单中的 SHA-256 摘要固定每个官方集成数据协议样例。

这个包不定义 CloudLink 封装，也不提供物理控制路径。

```bash
cargo test -p aether-integration-contract
```
