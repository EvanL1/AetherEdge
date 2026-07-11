# WebSocket 规则监控 API

通过 WebSocket 订阅规则引擎的实时监控数据。

## 连接

```
ws://localhost:6005/ws?client_id={client_id}
```

## 订阅规则

```json
{
  "type": "subscribe",
  "data": {
    "source": "rule",
    "rule_id": 1,
    "interval": 1000
  }
}
```

## 推送数据

```json
{
  "type": "rule_monitor",
  "timestamp": 1733644800,
  "data": {
    "rule_id": 1,
    "variables": { "X1": 50.3, "X2": 25.0 },
    "last_execution": {
      "success": true,
      "execution_path": ["start", "switch_1", "change_1", "end"]
    }
  }
}
```

## 取消订阅

```json
{ "type": "unsubscribe", "data": { "source": "rule" } }
```
