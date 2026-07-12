# Aether HTTP API

> 这是旧链接的兼容入口。逐接口定义以服务内置的 Swagger UI / OpenAPI 为准，
> 不再在 Markdown 中复制参数、响应和示例，避免两套文档漂移。

## 内置文档

Swagger 受 `swagger-ui` feature 控制。构建六服务安装包时统一启用：

```bash
./scripts/build-installer.sh v0.5.0 arm64 -s rust --enable-swagger
```

| 服务 | Swagger UI | OpenAPI JSON |
|---|---|---|
| `aether-io` | `http://127.0.0.1:6001/docs` | `http://127.0.0.1:6001/openapi.json` |
| `aether-automation` | `http://127.0.0.1:6002/docs` | `http://127.0.0.1:6002/openapi.json` |
| `aether-history` | `http://127.0.0.1:6004/docs` | `http://127.0.0.1:6004/openapi.json` |
| `aether-api` | `http://<edge-host>:6005/docs` | `http://<edge-host>:6005/openapi.json` |
| `aether-uplink` | `http://127.0.0.1:6006/docs` | `http://127.0.0.1:6006/openapi.json` |
| `aether-alarm` | `http://127.0.0.1:6007/docs` | `http://127.0.0.1:6007/openapi.json` |

只有 `aether-api` 是远程入口；其余服务端口必须留在 loopback。启用后 `/docs`
和 `/openapi.json` 本身是公开路由，受保护操作仍按 OpenAPI 声明要求 Bearer JWT
或服务凭证。只在受信的投运网络启用 Swagger。

认证、暴露边界、响应信封和服务级路由概览见
[HTTP API 参考](reference/http-api.md)。代码改动必须通过：

```bash
./scripts/check-openapi-contracts.sh
```
