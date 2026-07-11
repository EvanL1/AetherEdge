---
name: aether-ems-query
description: Use this skill when the user asks about AetherEMS live data: channels, devices, points, real-time values, historical sensor data, alarms, rules, models, instances, routing, SHM health, service health, or system status. Use aether CLI commands to answer — do NOT inspect source code, local database files, or config YAMLs to answer questions about current platform state.
---

# AetherEMS Query Skill

Use `aether` as the primary interface for all live AetherEMS platform data.

## Core Rules

- Execute `aether` commands to answer questions about current platform state.
- Do not read source code files or SQLite database files to answer live data questions.
- Do not read config YAML files to answer questions about running device data.
- Always add `--json` when you need to parse or chain results.
- When `--host` is needed, pass it as a global flag (before the subcommand).

## Command Discovery

```bash
aether --help
aether <subcommand> --help        # e.g. aether alarms --help
aether channels --help
```

## Global Flags

| Flag | Purpose |
|------|---------|
| `--json` | Machine-readable output (suppresses color/banner) |
| `--host <ip>` | Remote target (e.g. `--host 192.168.30.21`) |
| `--verbose` | Debug-level logging |

## Authentication

`aether` requires **no login**. All commands talk directly to local services.

## Connecting to a Remote Machine

```bash
# Option A — SSH (aether lives on the remote machine)
ssh root@192.168.30.62 'aether --json alarms list'

# Option B — local aether binary + --host flag
#   (requires service ports 6001/6002/6004/6006 to be reachable from localhost)
aether --host 192.168.30.62 --json alarms list
```

Prefer Option A when aether is already installed on the target machine and you are not sure which ports are exposed.

## Command Map

### Channels (io :6001)
```bash
aether channels list
aether channels list --json
aether channels status <id>
aether channels health
aether channels points list <channel_id>
aether channels points get <channel_id> <point_type> <point_id>
```

### Models — Products & Instances (automation :6002)
```bash
aether models products list
aether models products get <id>
aether models instances list
aether models instances list --product <product_id>
aether models instances get <id>
```

### Business Rules (automation :6002)
```bash
aether rules list
aether rules get <id>
aether rules executions <id>           # recent execution results
```

### Alarms (alarm :6007)
```bash
aether alarms list                     # active alerts
aether alarms list --level 3           # high-severity only
aether alarms list --channel <id>
aether alarms list --keyword overvolt
aether alarms get <id>
aether alarms rules                    # alarm rule definitions
aether alarms rules --enabled          # only enabled rules
aether alarms rules --channel <id>
aether alarms rule-get <id>
aether alarms events                   # historical trigger+recovery events
aether alarms events --level 3
aether alarms events --event-type trigger
aether alarms events --rule <rule_id>
aether alarms stats                    # alert count by level
aether alarms monitor                  # alarm monitor loop status
```

### Historical Data (history :6004)
```bash
aether history latest <series_key> <point_id>
# e.g. aether history latest inst:9:M 101

aether history query <series_key> <point_id>
aether history query inst:9:M 101 --from 2026-05-01T00:00:00Z --to 2026-05-12T00:00:00Z
aether history query inst:9:M 101 --page 2 --size 50

# Batch: query multiple points in one request (max 20 series)
aether history batch --series inst:9:M,101 --series inst:9:M,102 --from 2026-05-01T00:00:00Z
aether history batch --series inst:9:M,101 --series inst:12:M,201 --from 2026-05-01T00:00:00Z --to 2026-05-12T00:00:00Z --limit 500

aether history channels                # channels with recorded history
aether history metrics                 # storage backend statistics
aether history health                  # history health check
```

### Authoritative live data (local SHM)
```bash
aether shm get ch:1001:T:101
aether shm get inst:9:M:101
aether shm watch ch:1001:T:101
aether shm info
aether shm top
aether models instances data 9         # remote-capable SHM-backed API
```

### Routing
```bash
aether routing list                    # channel→instance routing table
aether routing get <channel_id>
```

### Service & System Health
```bash
aether doctor                          # full system health check
aether services status
aether logs list
aether logs view <service> -n 100
aether logs tail <service> --grep ERROR
```

## Intent → Command Mapping

| User intent | Command |
|------------|---------|
| "哪些通道在线？" | `aether channels list --json` |
| "通道 1001 状态" | `aether channels status 1001 --json` |
| "有哪些产品？" | `aether models products list --json` |
| "设备实例列表" | `aether models instances list --json` |
| "当前告警有哪些？" | `aether alarms list --json` |
| "高级别告警" | `aether alarms list --level 3 --json` |
| "通道 1001 的告警规则" | `aether alarms rules --channel 1001 --json` |
| "告警事件历史" | `aether alarms events --json` |
| "inst:9:M 点位 101 最新值" | `aether history latest inst:9:M 101 --json` |
| "过去一天数据" | `aether history query inst:9:M 101 --from <yesterday-iso> --json` |
| "同时查多个点" | `aether history batch --series inst:9:M,101 --series inst:12:M,201 --from <iso> --json` |
| "SHM 实时值" | `aether models instances data 9 --json` |
| "系统健康？" | `aether doctor --json` |
| "规则执行结果" | `aether rules executions <id> --json` |

## Multi-step Pattern

When a user provides a name but you need an ID:

```bash
# Step 1: find ID
aether channels list --json
# Step 2: use ID in subsequent command
aether alarms list --channel <id> --json
```

## Output Preference

- Always add `--json` when chaining commands or parsing results programmatically.
- JSON success envelope: `{"success": true, "data": ...}`
- JSON error envelope: `{"success": false, "error": "..."}`
- When presenting results to the user, extract `data` and format meaningfully.
- For alarm warning levels: 1 = low, 2 = medium, 3 = high.
- `series_key` format: `inst:<instance_id>:M` for measurement points, `io:<channel_id>:T` for telemetry history.

## Series Key Reference

| Key pattern | Meaning |
|-------------|---------|
| `inst:<id>:M` | Instance measurement points (automation writes) |
| `io:<ch>:T` | Channel telemetry points (io writes) |
| `io:<ch>:S` | Channel status points |

## Guardrails

- **Read-only**: Do not run commands that mutate state (`models instances action`, `channels write`, `rules enable/disable`) unless the user explicitly asks to make a change. `channels write` injects simulated telemetry; it never sends C/A device commands.
- **No source diving**: Never open `*.rs`, `*.yaml`, or `*.db` files to answer live data questions.
- **No external store needed**: use `aether shm` or SHM-backed service APIs for live data and `aether history` for historical data.
- If a service is not reachable, report the error message from `aether` directly; do not guess at data.

## Terminology

| Term | Meaning |
|------|---------|
| channel | Communication channel (Modbus, IEC104, etc.) |
| instance | Device instance (product + configuration) |
| point / point_id | Sensor or control register within a channel or instance |
| series_key | Logical history-series key, e.g. `inst:9:M` |
| alarm rule | Threshold rule that triggers an alert |
| alert | Currently active alarm condition |
| alert event | Completed trigger or recovery cycle |
| history | History service using local SQLite by default, with optional external storage extensions |
| io | Communication service managing protocol channels |
| automation | Model execution service managing instances and rules |
