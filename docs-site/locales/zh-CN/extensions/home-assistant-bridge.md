---
title: Home Assistant 桥接扩展
description: 把本地 Home Assistant 转换为只读委托设备投影，并单独提供默认关闭的受治理开关控制
updated: 2026-07-17
---

# Home Assistant 桥接扩展

这个可选扩展把已经投运的本地 Home Assistant 作为委托设备来源。Home Assistant 仍是这些
设备在该接入路径上的状态权威；AetherCloud 不直接连接 Home Assistant，也不保存其凭据。

当前源码已经具备 WebSocket 认证、区域/设备/实体/状态快照、有序状态变化、有限属性映射、
有界缓存、显式完整重同步语义，以及可跨重启恢复的拓扑世代号账本。默认路径只生成只读投影，
不会调用 Home Assistant 服务，也不会把 Home Assistant 状态写入 AetherEdge 权威共享
内存。账本只保存拓扑摘要和递增世代号的对应关系，不保存设备状态或凭据。

使用 `home-assistant` 构建特性编译，并通过进程环境变量显式启用后，`aether-io` 可以
装配该扩展。另一个默认关闭的 `home-assistant-cloudlink` 构建特性可以把已提交的只读
拓扑和观测分别写入持久文件队列，再通过经过认证的 TLS MQTT 会话发送；MQTT 发布确认
不会删除记录，只有严格的 CloudLink 应用确认才会推进队列。

第三个 `home-assistant-integration-control` 构建特性只在 Home Assistant、只读
CloudLink 发布和控制三个运行时开关都显式开启时装配固定开关执行器。经过校验的运行时清单
必须同时声明两项集成协议。边缘端会在订阅控制请求前装配签名校验、当前拓扑解析、本地
投运与委托策略、确认校验、持久审计和作业账本。调用方不能提供 Home Assistant 服务名、
附加参数、地址或凭据；接入方接纳也不代表物理动作成功。

预编译发行版、安装器和 Compose 尚未启用这些路径；目前也没有 YAML 配置项、命令行入口、
公开查询接口、生产级 OAuth 流程、通用设备命令表面或生产密钥轮换与撤销。协议边界见
[集成控制协议](/aethercontracts/spec/integration-control-v1alpha1/)。

连接地址、环境密钥引用、首次快照、可选 CloudLink 发布、断线恢复和故障排查见
[接入 Home Assistant](/guides/home-assistant)。
