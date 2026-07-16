---
title: "分发规范 v1 alpha 1 说明"
description: "此配置文件允许 AetherCloud、AetherIot 和独立实现固定一个精确的 AetherContracts 版本，同时保留其默认验证..."
updated: 2026-07-15
id: distribution-v1alpha1
status: experimental
version: 0.1.0-alpha.3
normative: true
---

# 分发规范 v1 alpha 1 说明

> 本中文页面用于帮助理解。规范性定义、Schema、测试夹具和 TCK 以 AetherContracts 对应版本的英文发布内容为唯一权威。

> 权威来源：[AetherContracts](https://github.com/EvanL1/AetherContracts/blob/main/spec/distribution-v1alpha1.md)。此页面已镜像到统一的 AetherIoT 文档中。

此配置文件允许 AetherCloud、AetherIot 和独立实现固定一个确切的 AetherContracts 版本，同时保持其默认验证路径离线。分发一致性证明发布身份和字节完整性；它不能证明消费者编解码器、状态机、身份验证配置文件或代理集成是否一致。

## 权限和信任

消费者仓库中经过审查的 `aether-contracts.lock.json` 是本地信任决策。它固定发布版本、带注释的标签对象、剥离的提交、确切的发布 URL、包大小和 SHA-256 以及外部清单 SHA-256。标签对象和提交提供可审查的来源标识符。捆绑包和清单摘要提供了强制的字节身份。

GitHub 标签、版本和共同托管的校验和文件是可变的分发证据，而不是第二个信任根。此 alpha 版本尚不需要 Sigstore 或 SLSA 证明以加密方式将捆绑包绑定到提交。缓存或 CDN 可能只提供锁摘要已接受的字节。消费者不得遵循`main`、`latest`、版本范围或未固定的操作修订。

## 锁定行为

锁定由`schemas/distribution/v1alpha1/consumer-lock.schema.json`关闭。此实验系的安全策略是固定的：

- `conformance_claim` 为 `distribution-only`；
- `production_release` 为 `false`；
- `legacy_default` 为 `true`；
- `physical_control` 为`false`。

`import` 绑定一个发布工件路径、一个使用方目标路径和一个 SHA-256。 `pending_import` 记录消费者尚未采用的发布工件和非空原因。导入的和待处理的源集是不相交的。锁的 `adoption` 部分声明范围、所需模块以及确切所需的发布源闭包。进口源和待处理源的合并必须等于关闭。 `partial-consumer` 至少有一个待处理源； `complete-consumer` 没有任何内容并导入整个闭包。完全的分发采用仍然没有说明行为一致性。

消费者在锁的 `manifest.local_path` 处提交确切的释放 `contract-manifest.json` 字节。离线验证检查其摘要、发布身份、安全声明、工件声明以及每个导入的消费者字节。它不执行下载、修复、回退或写入操作。

## 在线验证

可选的在线验证程序仅下载由锁命名的 URL。它在检查之前强制执行准确的响应大小和 SHA-256。它在锁的最大压缩字节、扩展字节、条目计数、路径字节、每个文件字节和总常规文件字节下解析进程内的 gzip 和 tar。它拒绝绝对或转义路径、链接、设备、不支持的条目类型、重复的规范化路径、无效的校验和、格式错误的终止符以及除一个锁定版本根之外的任何存档布局。提取使用私有目录和文件，并且仅在验证后进行。然后，它会验证清单以及每个导入的和待处理的发布源字节。失败是终结；没有回退到同级结帐或消费者本地候选者。

消费者必须将 `.github/actions/verify-consumer` 固定到锁定版本的完整 40 个字符的剥离提交。复合操作将其实际操作提交传递给验证器，验证器拒绝使用 `ACTION_COMMIT_MISMATCH` 的不同修订。锁定路径是与消费者相关的，并且必须保留在消费者仓库内。此检查补充但绝不会取代消费者的本机编解码器和状态机一致性测试。
