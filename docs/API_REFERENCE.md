# Aether HTTP API

> 这是旧链接的兼容入口。逐接口定义以服务生成的 OpenAPI 为准；Swagger UI 仅由
> `aether-api` 网关提供，避免六份 UI 和六个对外文档入口漂移。

## 内置文档

默认情况下，五个内部服务只在各自 loopback 端口提供 `/openapi.json`；网关
OpenAPI 与 Swagger 仅在显式启用后提供。启用网关 Swagger 后，使用一个文档选择器查看和调用全部契约：

```bash
./scripts/build-installer.sh v0.0.1 arm64 -s rust --enable-swagger
# http://<edge-host>:6005/docs
```

| 服务 | Gateway OpenAPI JSON |
|---|---|
| `aether-api` | `/openapi/gateway.json` |
| `aether-io` | `/openapi/io.json` |
| `aether-automation` | `/openapi/automation.json` |
| `aether-history` | `/openapi/history.json` |
| `aether-uplink` | `/openapi/uplink.json` |
| `aether-alarm` | `/openapi/alarm.json` |

只有 `aether-api` 是远程入口；其余服务端口必须留在 loopback。网关会把服务契约
重写到固定的 `/api/v1/<service>` 路径，因此 Swagger 的请求不会绕过认证边界。
仅在受信的投运网络启用 Swagger。

认证、暴露边界、响应信封和服务级路由概览见
[HTTP API 参考](reference/http-api.md)。代码改动必须通过：

```bash
./scripts/check-openapi-contracts.sh
```
