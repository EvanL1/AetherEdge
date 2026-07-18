---
title: "使用方分发规范 v1 alpha 1"
description: "让 AetherCloud、AetherEdge 与独立实现锁定一个精确 AetherContracts 发布版本，并默认离线验证。"
updated: 2026-07-17
id: distribution-v1alpha1
status: experimental
version: 0.1.0-alpha.4
normative: true
---

# 使用方分发规范 v1 alpha 1

> 本页帮助中文用户理解协议。带标签版本中的英文规范、JSON Schema、测试夹具和 TCK 才是规范性依据。

这套配置档让 AetherCloud、AetherEdge（旧发布制品中名为 AetherIot）与独立实现锁定一个精确的 AetherContracts 发布版本，同时让默认验证路径保持离线。分发一致性只能证明发布身份和字节完整性，不能证明使用方编解码器、状态机、认证配置档或消息代理集成符合契约。

最新发布版本是 `v0.1.0-alpha.3`。当前 `0.1.0-alpha.4` 内容是尚未发布的开发目标，仍处于实验阶段，不是生产分发声明。

## 权威与信任

使用方仓库中经过评审的 `aether-contracts.lock.json` 是本地信任决定。它锁定发布版本、带注释标签对象、解析后的提交、精确发布地址、发布包大小与 SHA-256，以及外部清单 SHA-256。标签对象和提交提供可审查的来源标识，发布包和清单摘要则提供强制执行的字节身份。

GitHub 标签、发布版本和同站托管的校验文件都只是可变分发证据，不是第二个信任根。本 alpha 版本尚不要求用 Sigstore 或 SLSA 证明把发布包和提交进行密码学绑定。缓存或内容分发网络只能提供已经通过锁定摘要接纳的字节。使用方不得跟随 `main`、`latest`、版本范围或未锁定的操作版本。

## 锁定行为

`schemas/distribution/v1alpha1/consumer-lock.schema.json` 将使用方锁定义为封闭对象。实验版本的安全策略固定为：

- `conformance_claim` 必须是 `distribution-only`；
- `production_release` 必须是 `false`；
- `legacy_default` 必须是 `true`；
- `physical_control` 必须是 `false`。

一个 `import` 会绑定发布制品路径、使用方目标路径和一份 SHA-256。`pending_import` 记录尚未采用的发布制品及非空原因。已导入集合和待处理集合不能重叠。

锁中的 `adoption` 部分声明采用范围、必需模块和精确的发布源文件全集。已导入与待处理源文件的并集必须恰好等于该全集。`partial-consumer` 至少有一个待处理源文件；`complete-consumer` 没有待处理项，并导入完整集合。即使分发采用完整，也不能据此宣称行为一致。

使用方必须在锁的 `manifest.local_path` 提交发布版本中 `contract-manifest.json` 的精确字节。离线验证会检查其摘要、发布身份、安全声明、制品声明和每一个已导入的使用方字节。离线验证不会下载、修复、回退或写入任何内容。

## 在线验证

可选在线验证器只下载锁中指定的地址，并在检查内容前强制核对精确响应大小和 SHA-256。它会在锁定的压缩字节数、解压字节数、条目数量、路径长度、单文件字节数和普通文件总字节数限制内，在进程中解析 gzip 与 tar。

验证器会拒绝绝对路径、越界路径、链接、设备、不支持的条目类型、重复的规范化路径、无效校验和、格式错误的结束标记，以及不符合单一锁定发布根目录的归档布局。只有验证通过后，内容才会提取到私有目录和文件。随后验证器会检查清单，以及所有已导入和待处理的发布源字节。失败即终止，不会回退到相邻源码目录或使用方本地候选文件。

使用方必须把 `.github/actions/verify-consumer` 锁定到该发布版本解析后提交的完整 40 位字符。复合操作把自己的实际提交交给验证器；版本不同会返回 `ACTION_COMMIT_MISMATCH`。锁文件路径必须相对使用方仓库，并始终位于仓库内部。

这些检查只能补充使用方自己的编解码器和状态机一致性测试，不能替代它们，也不能把实验版本变成生产版本。
