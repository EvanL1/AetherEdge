---
title: "aether-pack"
description: "为 Aether 边缘内核加载版本化、行业中立的领域包清单"
updated: 2026-07-16
---

# aether-pack

针对 Aether 边缘内核的版本化、行业中立的域包清单加载。该crate验证包自己的发布标识、分发标识、兼容的Aether范围、所需的功能和协议 ID、明确未委托的示例以及包根限制的资产目录。

加载是只读的。它不安装包、启用通道或规则或调试现场硬件。不受支持或未知的清单字段、缺少要求以及绝对路径、遍历 `..` 或在包根目录之外解析的路径会失败并出现键入错误。
```rust
use aether_pack::{PackRuntime, load_pack_manifest};

# fn inspect() -> Result<(), aether_pack::PackError> {
let runtime = PackRuntime::new("0.5.0")
    .with_capabilities(["device.read_point"])
    .with_protocols(["modbus_tcp"]);
let manifest = load_pack_manifest("packs/example", &runtime)?;
println!("{} {}", manifest.id(), manifest.version());
# Ok(())
# }
```

## 当前领域包配置

自动化和 `aether mcp` 使用一个共享入口点：`<AETHER_CONFIG_PATH>/global.yaml`。安全默认激活无域包：
```yaml
packs: []
```

操作员通过声明预期身份及其根来激活已安装的 Pack。根目录可以是绝对目录或相对于配置目录，但不能包含 `..`：
```yaml
packs:
  - id: energy
    root: /opt/aether/packs/energy
```

配置的身份必须匹配`pack.yaml`。每个选定的清单在其模型或知识变得可见之前都会验证 Aether 兼容性、功能、协议、调试和资产限制。

Pack 拥有的 `mappings`、`rules`、`evaluations` 和 `data_processing` 任务是正式的索引资产类别。每个目录包含使用`aether.pack.asset-index.v1`的`index.yaml`；清单功能 ID、索引 ID 和实际常规文件必须完全匹配。未知字段/文件、重复 ID 或路径、符号链接、路径转义、媒体/架构不匹配以及超大文件无法关闭。 Pack v1 将每个类别固定为其相应的 v1 有效负载架构；更改有效负载契约需要更改 Pack 契约版本。只有显式活动的 Pack 才会提供命名空间 `<pack>/<category>/<asset>` 身份。

机器可读合约是 [`Pack manifest v1`](https://github.com/EvanL1/AetherEdge/blob/main/contracts/pack/pack-manifest.v1.schema.json) 和 [`Pack asset index v1`](https://github.com/EvanL1/AetherEdge/blob/main/contracts/pack/pack-asset-index.v1.schema.json) 架构。

根据您的选择在 MIT 或 Apache-2.0 下获得许可。
