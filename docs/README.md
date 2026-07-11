# Aether 文档

## 快速开始

- [开发环境搭建](./GETTING_STARTED_DEVELOPMENT.md) - 从零开始运行项目
- [Aether CLI 参考](./reference/cli.md) - 当前命令、参数和部署模式

## API 文档

- [HTTP API 参考](./API_REFERENCE.md) - 完整的 REST API 说明
- [WebSocket API](./websocket-rule-monitor-api.md) - 实时数据推送接口

## 配置说明

- [配置格式指南](./CONFIG_FORMAT_GUIDE.md) - YAML、CSV、JSON 配置规范

## 运维参考

- [运维日志](./operations-log.md) - 问题记录与解决方案

---

## 常用命令

```bash
# 在仓库根目录生成只读计划；默认路径与 Compose 的 ./data 挂载一致
aether --json setup

# 审阅计划后应用输出中的同一 plan ID
aether setup apply --plan-id <PLAN_ID>

# 先按部署指南构建或加载 aetherems:latest，再启动六进程组合
aether services start

# 检查系统状态
aether doctor

# 查看帮助
aether --help
```

## 环境变量

| 变量 | 说明 | 默认值 |
|------|------|--------|
| `AETHER_IO_URL` | Io 服务地址 | `http://localhost:6001` |
| `AETHER_AUTOMATION_URL` | Automation 服务地址 | `http://localhost:6002` |
| `AETHER_CONFIG_PATH` | 配置文件目录 | 源码 checkout 为 `./data/config`；安装后由 install context 指定 |
| `AETHER_DATA_PATH` | 数据文件目录 | 源码 checkout 为 `./data`；安装后由 install context 指定 |
