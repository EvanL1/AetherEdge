---
title: "Aether HTTP 数据处理器"
description: "`DataProcessor` 端口的可选、有界 HTTP 实现。它向`/v1/process`发送一个完整的请求帧；它没有公开任何回调..."
updated: 2026-07-12
---

# Aether HTTP 数据处理器

`DataProcessor` 端口的可选、有界 HTTP 实现。它向`/v1/process`发送一个完整的请求帧；它不会公开对 Aether、SHM、历史记录或配置的回调。

仅本地主机或环回地址上的本地处理器接受普通 HTTP。远程路由需要 HTTPS。请求和响应大小以及连接和请求超时都是强制配置。

## 组成
```rust
use std::time::Duration;

use aether_http_data_processor::{
    BearerSecret, HttpDataProcessor, HttpDataProcessorConfig,
};
use aether_ports::{DataBoundary, DataProcessorDescriptor};

# fn build(
#     descriptor: DataProcessorDescriptor,
#     deployment_token: String,
# ) -> Result<HttpDataProcessor, aether_ports::PortError> {
let config = HttpDataProcessorConfig::new(
    "https://processor.example.net",
    descriptor,
    Duration::from_secs(2),
    Duration::from_secs(10),
    4 * 1024 * 1024,
)?
.with_bearer_secret(BearerSecret::new(deployment_token)?);
let processor = HttpDataProcessor::new(config)?;
# assert_eq!(processor.descriptor().data_boundary(), DataBoundary::Remote);
# Ok(processor)
# }
```

只有组合根选择来源和可选秘密。适配器派生固定版本路由：

- `POST /v1/process`
- `GET /v1/health`

请求和成功处理响应使用 `application/vnd.aether.data-processing+json;version=1`。健康检查端点接受包含 `status`、`processor`、`version` 和 `contract` 的小型 JSON 响应，并根据配置的描述符验证这些身份字段。

## 硬边界

- 请求 JSON 由 `aether-data-processing` 编码，并在任何网络之前根据描述符的 `max_frame_samples` 和 `max_request_bytes` 进行检查调用。
- 流式传输时响应是有限的。 `Content-Length` 是早期守卫，不是限制的权威。
- 重定向和环境代理发现已禁用。
- 远程路由需要 HTTPS。本地 HTTP 需要显式 `Local` 描述符和环回或 `localhost` 源。
- URL 凭据、查询字符串、片段、非源路径、零限制和零超时会导致 `Permanent` 的配置失败。
- 承载令牌没有环境加载 API，在HTTP 标头，并从所有适配器 `Debug` 输出中进行编辑。
- 远程响应正文、URL 和传输内部结构永远不会复制到端口错误中。

每个 4xx 或 5xx 响应必须使用相同的版本化媒体类型和封闭的 `aether.data-processing.error.v1` 信封。在解码之前，主体的大小是有限的。未知字段、显式空值、格式错误的 JSON、不匹配的 HTTP 状态/类别、无效或不匹配的请求 ID 以及不一致的重试元数据无法作为 `InvalidData` 关闭。

已验证的失败映射到稳定的 `PortErrorKind` 值：到 `Timeout` 的截止时间错误、到 `Conflict` 的经过验证的 HTTP 冲突响应、 `Unavailable` 可重试容量/不可用，`InvalidData` 无效帧，`Rejected` 请求/资源拒绝，`Permanent` 不可重试授权、查找、内部或不可用故障。只有经过验证的稳定代码、类别、可重试性、重试延迟和请求ID才进入端口诊断；处理器的自由格式消息和详细信息永远不会被复制。带有 `status: unavailable` 的响应仍然是有效的 `ProcessingResult`，而不是传输错误。

`Conflict` 映射不会创建请求重播语义。公共 `data_processing.process` 操作是非幂等的，并且没有内置重复数据删除或请求 ID 重用保证。

## 验证
```bash
cargo test -p aether-http-data-processor
cargo clippy -p aether-http-data-processor --all-targets -- -D warnings
cargo fmt --package aether-http-data-processor -- --check
```
