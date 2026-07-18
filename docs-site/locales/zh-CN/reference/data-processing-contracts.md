---
title: "数据处理契约"
description: "用于处理帧、请求、派生结果、验证和失败语义的版本 1 契约"
updated: 2026-07-11
---

# 数据处理契约

此参考指定了 **Aether 数据处理** 实施的版本 1 合约。 Rust 域值和编排位于 `aether-domain`、`aether-ports` 和 `aether-application` 中； `aether-data-processing` 提供严格的传输中立 JSON 编解码器，`extensions/http-data-processor` 实现可选的 HTTP 传输。

合约编码一个架构规则：Aether 组装一个完整的输入帧，然后请求处理器。 `DataProcessor` 永远不会接收用于读取 Aether 的 SHM、历史数据库或站点配置的凭据或回调。

## 合约系列

| 合约 | 标识符 | 目的 |
|----------|------------|---------|
| 任务声明 | `aether.data-processing-task.v1` | 可移植域语义和输入绑定 |
| 应用程序请求 | `aether.data-processing.process-task-request.v1` | 选择委托任务、绑定、数据剪切和键入选项 |
| 处理框架 | `aether.processing-frame.v1` | 对齐观察值和已知未来协变量 |
| 处理器请求 | `aether.data-processing.request.v1` | 已解决的任务和绑定、截止日期、完整框架、摘要和类型化选项 |
| 结果信封 | `aether.data-processing.result.v1` | 状态、来源、到期和类型化派生数据 |
| 预测输出 | `aether.data-processing.output.forecast.v1` | 处理器生成的时间索引预测值 |
| 接受的派生数据 | `aether.derived-data.v1` | 带有Aether标记、经过验证的任务输出 |
| 错误信封 | `aether.data-processing.error.v1` | 类型化传输或处理器故障 |

HTTP 适配器使用媒体类型：
```text
application/vnd.aether.data-processing+json;version=1
```

合约标识符是精确的、区分大小写的字符串。版本 1 的使用方必须拒绝不受支持的主要版本，而不是猜测如何解释它。仅当模式明确允许时才可以引入附加字段；实现不得使用未知字段通过公共信封走私特定于供应商的命令。

下面的关键字 **MUST**、**MUST NOT**、**SHOULD** 和 **MAY** 是规范性的。

## 通用值规则

- 时间必须是在 Unix 纪元或之后以 `Z` 结尾的 RFC 3339 UTC 字符串，没有更细的值比毫秒精度。它们无损地映射到 `TimestampMs`；编码器可以省略零小数部分，但不得发出超过三个小数位。处理器不得默默地重新解释本地时间或数字纪元值。
- 持续时间和节奏必须是大于零的整数秒。
- 数字值必须是有限的 JSON 数字。 `NaN`、正无穷大和负无穷大无效。
- 缺失的样本必须是 JSON `null` 且样本质量为 `missing`。
- 对于数字特征和输出，单位必须是明确的。任务声明选择规范单位。当前 v1 运行时验证委托源元数据是否已与该单位和符号约定匹配；它不会在请求时进行转换。
- 功能名称必须与任务声明完全匹配，并且在段内是唯一的。
- 在 Aether 解决重复项后，时间戳必须严格递增。
- 摘要使用带前缀的小写十六进制 SHA-256 `sha256:`。

## `ProcessingFrame`

`ProcessingFrame` 携带所选处理器允许使用的所有数据。它具有历史观察结果、可选的已知未来协变量、可选的静态特征、聚合质量和编辑安全的出处。

### 形状
```json
{
  "schema": "aether.processing-frame.v1",
  "as_of": "2026-07-11T12:00:00Z",
  "cadence_seconds": 900,
  "history": {
    "timestamps": ["2026-07-11T11:45:00Z", "2026-07-11T12:00:00Z"],
    "features": {
      "load": {
        "value_type": "number",
        "unit": "kW",
        "values": [820.0, 835.0],
        "quality": ["good", "good"]
      }
    }
  },
  "future_covariates": {
    "timestamps": ["2026-07-11T12:15:00Z", "2026-07-11T12:30:00Z"],
    "features": {
      "temp_avg": {
        "value_type": "number",
        "unit": "Cel",
        "values": [32.1, 32.0],
        "quality": ["good", "good"]
      },
      "quarter_hour": {
        "value_type": "number",
        "unit": "1",
        "values": [49, 50],
        "quality": ["good", "good"]
      }
    }
  },
  "static_features": {
    "rated_power": {
      "value_type": "number",
      "unit": "kW",
      "value": 2500.0,
      "quality": "good"
    }
  },
  "quality": {
    "input_watermark": "2026-07-11T11:59:58Z",
    "missing_ratio": 0.0,
    "max_gap_seconds": 900,
    "live_tail_included": false,
    "substituted_samples": 0
  },
  "provenance": [
    {
      "segment": "history",
      "feature": "load",
      "source_kind": "history",
      "source_ref": "energy.site.load.active_power",
      "watermark": "2026-07-11T11:59:58Z"
    },
    {
      "segment": "future_covariates",
      "feature": "temp_avg",
      "source_kind": "covariate",
      "source_ref": "weather.nwp.air_temperature",
      "watermark": "2026-07-11T11:50:00Z",
      "issued_at": "2026-07-11T11:40:00Z"
    },
    {
      "segment": "future_covariates",
      "feature": "quarter_hour",
      "source_kind": "calendar",
      "source_ref": "calendar.utc.quarter_hour",
      "watermark": "2026-07-11T12:00:00Z"
    },
    {
      "segment": "static_features",
      "feature": "rated_power",
      "source_kind": "constant",
      "source_ref": "energy.site.rated_power",
      "watermark": "2026-07-11T12:00:00Z"
    }
  ]
}
```

### 顶级字段

| 字段 | 必填 | 验证 |
|-------|----------|------------|
| `schema` | 是 | 完全`aether.processing-frame.v1` |
| `as_of` | 是 | 逻辑截止请求； UTC RFC 3339 |
| `cadence_seconds` | 是 | 与任务修订版匹配的正整数 |
| `history` | 是 | 至少一个时间戳和一个已声明的时间戳功能 |
| `future_covariates` | 任务相关 | 对于声明未来已知输入的任务是必需的；否则省略 |
| `static_features` | 否 | 仅由任务声明的功能；允许空对象 |
| `quality` | 是 | 由 Aether 计算的聚合质量 |
| `provenance` | 是 | 每个填充的 `(segment, feature)` 对只有一个条目；出口策略只能省略 `source_ref` |

通用合约包含 `static_features`，但当前选择加入的 `aether-api` 运行时 YAML 加载器无法绑定静态值。在扩展和测试该加载程序之前，它们仅适用于使用 `DataProcessingBinding` 的自定义组合。运送的负载/PV 运行时路由未声明任何内容。

### 分段架构

`history` 和 `future_covariates` 使用相同的结构架构：
```json
{
  "timestamps": ["2026-07-11T12:15:00Z"],
  "features": {
    "feature_name": {
      "value_type": "number",
      "unit": "kW",
      "values": [123.4],
      "quality": ["good"]
    }
  }
}
```

`value_type` 是 `number`、`string` 或 `boolean` 之一。段中的每个系列必须具有与该段的时间戳完全相同的值和质量条目数。样本质量是以下之一：

| 质量 | 含义 |
|---------|---------|
| `good` | 源适配器和任务策略接受的声明的任务表示中的值 |
| `uncertain` | 源提供了一个值，但将其置信度标记为减少 |
| `substituted` | Aether 使用任务声明的方法填充样本 |
| `missing` | 没有可用值；对应的值为 `null` |

对于数字系列，需要 `unit`。对于字符串和布尔系列，必须省略 `unit`。仅当任务的缺失策略允许时，`null` 才被允许。处理器不得将缺失值转换为零，除非任务在构建帧之前显式声明精确替换。

### 时间不变量

- 版本 1 使用间隔结束标签。对于节奏 `c`，历史标签 `t` 表示原始间隔 `(t-c, t]`。
- 具有 `N` 步数的历史网格必须是 `as_of-(N-1)c, ..., as_of`；它的最终标签恰好是 `as_of`，源读取不得超出该截止值。
- 未来协变量网格必须从 `as_of+c` 开始，并按精确的节奏前进。
- 版本 1 帧使用精确的 `cadence_seconds` 网格。源差距被保留为显式缺失样本或被任务策略拒绝；时间戳不会被静默删除或重新定时。
- 未来的目标值不得出现在 `future_covariates` 中。仅当任务将其标记为提前已知时，才允许使用该功能。
- `input_watermark` 必须小于或等于 `as_of` 并表示所考虑的最新源观察，而不是请求创建时间。

### 聚合质量

| 字段 | 必填 | 含义 |
|-------|----------|---------|
| `input_watermark` | 是 | 组装过程采用的最新实际观测时间 |
| `missing_ratio` | 是 | 缺失单元格数除以全部必需单元格数，取值范围为 `[0, 1]` |
| `max_gap_seconds` | 是 | 所需历史观测值之间的最大差距 |
| `live_tail_included` | 是 | 只读实时状态在存储后是否起作用历史 |
| `substituted_samples` | 是 | 标记为`substituted`的值计数 |

聚合不会取代每个样本的质量。处理器根据选定的任务和可选的处理器工件清单进行验证。

v1 线契约可以承载每个样本的质量，但当前的生产源不能端到端地保留设备原始质量。嵌入式历史表存储没有设备质量的数字观测值，并且当前的 SHM 桥标签接受有限的实时值作为 `good`。因此，当前的调试可以强制执行新鲜度、间隙、缺失、数字约束、出处和发布时间，但需要原始设备质量的部署必须添加具有质量的源适配器。

仅允许对委托聚合为 `Last` 的历史要素使用实时尾部，其中一个瞬时 SHM 值可以有效地替换最终像元。版本 1 拒绝 `Mean`、`Sum`、`Min` 和 `Max` 的实时尾部。当前能源负荷和光伏目标历史使用 `Mean`，因此它们的框架设置为 `live_tail_included: false`。

### 来源追踪

`segment` 为 `history`、`future_covariates` 或 `static_features`，并标识条目所描述的特征的出现次数。 `source_kind` 是 `history`、`live`、`history_and_live`、`covariate`、`calendar` 或 `constant` 之一。 `source_ref` 是语义标识符，而不是 SQL 查询、数据库凭据、SHM 路径、通道地址或模型文件系统路径。当远程数据出口策略删除 `source_ref` 时，它必须保留分段、要素名称、源类型和水印。

`issued_at` 是可选的，并记录发出版本化外部预测（例如 NWP 运行）的时间。如果存在，它必须满足 `issued_at <= watermark <= frame.as_of`。 `future_covariates.timestamps`的有效时间可能晚于`as_of`；问题切可能不会。这可以防止程序集选择在请求的事件时间截止时不可用的协变量预测。它不会使当前历史存储或工件选择器在时间点上安全进行回溯测试。

每个填充的 `(segment, feature)` 对必须具有恰好一个出处条目。缺少条目、具有不同元数据的重复键以及缺少功能的条目均无效。确定性生成的日历值和委托常量也不例外：它们分别使用 `calendar` 和 `constant` 出处。它们的水印记录了用于导出值的数据切割，但它们不会推进聚合 `input_watermark`，这意味着汇编考虑的最新实际观察结果。

### 实现的历史适配器

生产 `aether-api` 组合默认使用 `SqliteHistoryQuery`。它以惰性和只读方式打开现有的 `aether-history.db` 架构，从不创建或迁移它，并针对一个 SQLite 读取事务内的一个逻辑请求执行所有功能读取。任务范围的路由修复每个功能的物理系列、单元、节奏、聚合和重复策略。原始数字行被缩减为区间结束桶；空存储桶保留显式缺失值，水印是实际参与的最新数字原始观察值，而不是存储桶标签。

`aether-history` 仍然是架构、写入、保留和文件生命周期所有者。查询适配器依赖于 SQLite 快照/WAL 行为和部署文件权限；文件丢失或无法访问只会导致键入的处理请求不可用，并且可以在以后的调用中恢复。选择外部历史数据库不会默默地重定向此适配器 — 当前运行时必须使用符合要求的 `HistoryQuery` 实现进行重组。

`mode=ro` 和 `query_only=ON` 限制 SQLite 操作，但不会取代操作系统隔离。生产必须向 API 主体授予对包含数据库/WAL/SHM 系列的专用历史记录目录的只读权限，同时保持其自己的配置/审核数据库单独可写。基础 Compose 目前将 `/app/data` 读写挂载到 `aether-api` 中；它是开发基线，而不是直接历史记录读取的完整生产权限边界。

调用运行时事务快照是读取一致的，但架构不是双时态的。行仅包含事件 `time_ms`：没有 `ingested_at`，也没有源、绑定或配置纪元。因此`as_of`不是时间点数据库重建。使用旧事件时间进行后期回填可以更改稍后的重播，并且重新映射同一逻辑 `(series_key, point_id)` 后面的设备可以拼接源纪元。预期的任务/绑定修订验证当前的调试，但无法过滤存储行中缺少的元数据。防泄漏离线评估必须使用在评估切割时捕获的冻结历史记录快照/导出或具有摄取时间和源历元过滤的另一个适配器。

组合当前根据持久的 `history_config.storage_*` 验证其路径，但这些值是保存的意图，而不是活动编写器的证明。 `PUT /hisApi/storage` 不会重新连接历史记录。在存储更改期间，禁用数据处理，重新连接或重新启动 `aether-history`，验证活动的 SQLite 后端和已委托的哨兵系列，然后使用匹配的路径重新启动 `aether-api`。

`HttpHistoryQuery` 是上游服务的可选环回适配器，已实现精确的节奏网格。它仅接受 `aggregation=last` 和 `duplicate_policy=reject`；它不能替代原始 SQLite 聚合。

## `ProcessTaskRequest`

应用程序调用者选择委托任务和事件时数据剪切。他们不提交框架、端点、处理器 ID、工件选择器、凭证或源查询。
```json
{
  "task_id": "energy.site-load-forecast",
  "expected_task_revision": 1,
  "binding_id": "site-a",
  "expected_binding_revision": 7,
  "as_of": "2026-07-11T12:00:00Z",
  "options": {
    "kind": "forecast",
    "horizon_steps": 2
  }
}
```

传输通过公共 `RequestContext` 提供请求身份和参与者数据；它们在此键入的请求中不会重复。当配置更改时，`expected_task_revision` 和 `expected_binding_revision` 使调用失败关闭。 `binding_id` 命名启用的委托绑定；应用程序以原子方式解析其点、协变量、处理器路由、工件策略和出口策略。

## `DataProcessingRequest`

这是由 Aether 组装的面向处理器的请求。它绝不是公共应用程序输入。

### 数据结构
```json
{
  "schema": "aether.data-processing.request.v1",
  "request_id": "0190aee6-2139-7a87-8448-806f1b843201",
  "submitted_at": "2026-07-11T12:00:01Z",
  "deadline": "2026-07-11T12:00:06Z",
  "task": {
    "id": "energy.site-load-forecast",
    "revision": 1,
    "kind": "forecast"
  },
  "binding": {
    "id": "site-a",
    "revision": 7
  },
  "processor_contract": "aether.data-processing.forecast.v1",
  "artifact": {
    "kind": "model",
    "family": "site-load",
    "version": "v3",
    "artifact_digest": "sha256:98967bdedc60b8ab555e596516eb272063c139ccf3a3112fb29a46ab0610f270"
  },
  "frame": {
    "schema": "aether.processing-frame.v1",
    "as_of": "2026-07-11T12:00:00Z",
    "cadence_seconds": 900,
    "history": {
      "timestamps": ["2026-07-11T11:45:00Z", "2026-07-11T12:00:00Z"],
      "features": {
        "load": {
          "value_type": "number",
          "unit": "kW",
          "values": [820.0, 835.0],
          "quality": ["good", "good"]
        },
        "temp_avg": {
          "value_type": "number",
          "unit": "Cel",
          "values": [31.0, 31.2],
          "quality": ["good", "good"]
        },
        "humidity": {
          "value_type": "number",
          "unit": "%",
          "values": [64.0, 63.0],
          "quality": ["good", "good"]
        },
        "rain": {
          "value_type": "number",
          "unit": "mm",
          "values": [0.0, 0.0],
          "quality": ["good", "good"]
        },
        "quarter_hour": {
          "value_type": "number",
          "unit": "1",
          "values": [47, 48],
          "quality": ["good", "good"]
        }
      }
    },
    "future_covariates": {
      "timestamps": ["2026-07-11T12:15:00Z", "2026-07-11T12:30:00Z"],
      "features": {
        "temp_avg": {
          "value_type": "number",
          "unit": "Cel",
          "values": [32.1, 32.0],
          "quality": ["good", "good"]
        },
        "humidity": {
          "value_type": "number",
          "unit": "%",
          "values": [61.0, 62.0],
          "quality": ["good", "good"]
        },
        "rain": {
          "value_type": "number",
          "unit": "mm",
          "values": [0.0, 0.0],
          "quality": ["good", "good"]
        },
        "quarter_hour": {
          "value_type": "number",
          "unit": "1",
          "values": [49, 50],
          "quality": ["good", "good"]
        }
      }
    },
    "static_features": {},
    "quality": {
      "input_watermark": "2026-07-11T12:00:00Z",
      "missing_ratio": 0.0,
      "max_gap_seconds": 900,
      "live_tail_included": false,
      "substituted_samples": 0
    },
    "provenance": [
      {
        "segment": "history",
        "feature": "load",
        "source_kind": "history",
        "source_ref": "energy.site.load.active_power",
        "watermark": "2026-07-11T12:00:00Z"
      },
      {
        "segment": "history",
        "feature": "temp_avg",
        "source_kind": "history",
        "source_ref": "weather.observed.air_temperature",
        "watermark": "2026-07-11T12:00:00Z"
      },
      {
        "segment": "history",
        "feature": "humidity",
        "source_kind": "history",
        "source_ref": "weather.observed.relative_humidity",
        "watermark": "2026-07-11T12:00:00Z"
      },
      {
        "segment": "history",
        "feature": "rain",
        "source_kind": "history",
        "source_ref": "weather.observed.precipitation",
        "watermark": "2026-07-11T12:00:00Z"
      },
      {
        "segment": "history",
        "feature": "quarter_hour",
        "source_kind": "calendar",
        "source_ref": "calendar.utc.quarter_hour",
        "watermark": "2026-07-11T12:00:00Z"
      },
      {
        "segment": "future_covariates",
        "feature": "temp_avg",
        "source_kind": "covariate",
        "source_ref": "weather.nwp.air_temperature",
        "watermark": "2026-07-11T11:50:00Z",
        "issued_at": "2026-07-11T11:40:00Z"
      },
      {
        "segment": "future_covariates",
        "feature": "humidity",
        "source_kind": "covariate",
        "source_ref": "weather.nwp.relative_humidity",
        "watermark": "2026-07-11T11:50:00Z",
        "issued_at": "2026-07-11T11:40:00Z"
      },
      {
        "segment": "future_covariates",
        "feature": "rain",
        "source_kind": "covariate",
        "source_ref": "weather.nwp.precipitation",
        "watermark": "2026-07-11T11:50:00Z",
        "issued_at": "2026-07-11T11:40:00Z"
      },
      {
        "segment": "future_covariates",
        "feature": "quarter_hour",
        "source_kind": "calendar",
        "source_ref": "calendar.utc.quarter_hour",
        "watermark": "2026-07-11T12:00:00Z"
      }
    ]
  },
  "options": {
    "kind": "forecast",
    "horizon_steps": 2
  },
  "input_digest": "sha256:8b227777d4dd1fc61c6f884f48641d02b50a8a461a77f8fae7f48e32fbd8c372"
}
```

上面的摘要是说明性的，而不是缩写示例的摘要。

### 请求字段

| 字段 | 必需 | 验证 |
|-------|----------|------------|
| `schema` | 是 | 完全正确`aether.data-processing.request.v1` |
| `request_id` | 是 | 此调用的关联标识； v1 不提供重播或重复数据删除语义 |
| `submitted_at` | 是 | 应用程序创建请求的 UTC 时间 |
| `deadline` | 是 | 超过此 UTC 截止时间后，处理器不得开始工作或返回已接纳结果 |
| `task` | 是 | 与已加载任务匹配的 ID、正修订号和任务类型 |
| `binding` | 是 | 已解析的委托绑定 ID 和正修订号 |
| `processor_contract` | 是 | 由任务和处理器公布的契约 |
| `artifact` | 否 | 通用种类、系列和可选请求的版本；无路径或 URL |
| `frame` | 是 | 有效的 `ProcessingFrame` |
| `options` | 是 | 与 `task.kind` 匹配的类型化选项对象 |
| `input_digest` | 是 | 用于关联、审核和离线比较的规范内容标识；不是操作幂等性密钥 |

`options.kind = forecast` 需要正`horizon_steps`。 `quantiles` 是可选的；如果存在，它包含严格介于 0 和 1 之间的唯一有限数字（按升序排列），并且其计数不得超过委托任务的 `max_quantiles`。当前的能源负载和光伏任务修订版将该限制设置为零，因此它们的请求忽略`quantiles`。处理器必须拒绝未知选项，而不是默默地忽略改变行为的字段。

`input_digest` 是基于 RFC 8785 规范 JSON 表示的 SHA-256：
```json
{
  "task": "<the exact versioned task identity object>",
  "binding": "<the exact versioned binding identity object>",
  "processor_contract": "<contract identifier>",
  "artifact": "<the artifact object or null>",
  "frame": "<the complete frame object>",
  "options": "<the complete typed options object>"
}
```

此图中的字符串代表相应的 JSON 值，而不是摘要输入中的文字字符串。任务对象包括ID、revision、kind；绑定对象包括 ID 和修订版本。这些修订对其受控定义进行内容不同的更改，而无需将站点配置复制到处理器请求中。处理器端点和身份、关联时间和 `request_id` 被排除，因此对完全相同的规范化受控内容的独立调用保留相同的摘要。当源可变时，仅重复 `as_of` 并不能保证内容。版本 1 不提供内置结果缓存或重播存储。

故意缺少参与者和参与者的权限。 Aether 授权应用程序调用并审核参与者；它不会向处理器公开身份，除非单独的、显式的服务身份验证协议需要它。

工件身份不是工件年表。选择器/结果来源可以携带种类、系列、版本和摘要，但版本 1 没有 `trained_through` 或 `available_at` 字段，并且不与 `frame.as_of` 进行比较。摘要固定的工件一旦提供即可重现，但仍可以为旧框架选择稍后训练或发布的模型。严格的历史模型评估必须在评估削减时冻结工件注册表，直到训练和可用性削减成为规范契约字段。

## `ProcessingResult`

### 成功的预测
```json
{
  "schema": "aether.data-processing.result.v1",
  "request_id": "0190aee6-2139-7a87-8448-806f1b843201",
  "task": {
    "id": "energy.site-load-forecast",
    "revision": 1,
    "kind": "forecast"
  },
  "binding": {
    "id": "site-a",
    "revision": 7
  },
  "input_digest": "sha256:8b227777d4dd1fc61c6f884f48641d02b50a8a461a77f8fae7f48e32fbd8c372",
  "status": "produced",
  "issued_at": "2026-07-11T12:00:02Z",
  "expires_at": "2026-07-11T13:00:00Z",
  "input_watermark": "2026-07-11T12:00:00Z",
  "processor": {
    "id": "load-forecasting-edge",
    "version": "0.1.0",
    "contract": "aether.data-processing.forecast.v1"
  },
  "artifact": {
    "kind": "model",
    "family": "site-load",
    "version": "v3",
    "artifact_digest": "sha256:f04c532f2f814a3690f0f40e6f26fa82b0d69b9c510e7c0bb9f9f4de35b5a882"
  },
  "output": {
    "schema": "aether.data-processing.output.forecast.v1",
    "kind": "forecast",
    "target": "load",
    "unit": "kW",
    "sign_convention": "positive_consumption",
    "cadence_seconds": 900,
    "timestamp_semantics": "interval_end",
    "points": [
      {
        "timestamp": "2026-07-11T12:15:00Z",
        "value": 846.2
      },
      {
        "timestamp": "2026-07-11T12:30:00Z",
        "value": 852.7
      }
    ]
  },
  "warnings": []
}
```

### 常见结果字段

| 字段 | 必填 | 验证 |
|-------|----------|------------|
| `schema` | 是 | 完全`aether.data-processing.result.v1` |
| `request_id` | 是 | 完全请求关联 |
| `task` | 是 | 请求中的确切任务 ID、修订版和类型 |
| `binding` | 是 | 来自请求的准确绑定 ID 和修订版请求 |
| `input_digest` | 是 | 请求的精确摘要 |
| `status` | 是 | `produced`、`fallback`，或`unavailable` |
| `issued_at` | 是 | 结果完成的 UTC 时间 |
| `expires_at` | 生成/回退 | `issued_at`之后，受任务限制策略 |
| `input_watermark` | 是 | 精确接受的帧水印 |
| `processor` | 是 | 稳定的处理器标识、版本和合约 |
| `artifact` | 否 | 使用时的实际通用工件种类、系列、版本和摘要 |
| `output` | 生成/后备 | 类型化处理器输出；禁止`unavailable` |
| `fallback` | 后备 | 策略、原因代码和数据基础 |
| `unavailable` | 不可用 | 原因代码和重试指导 |
| `warnings` | 是 | 稳定警告代码数组；如果没有则为空 |

### 预测输出

版本 1 最初定义了类型化预测输出架构。估计、检测和分类任务应添加自己的版本化输出架构，而不是在 `output` 下放置任意 JSON。

版本 1 将 `timestamp_semantics` 修复为 `interval_end`：每个点时间戳标识其预测的间隔的结束时间。 `interval_start`、`instant` 或任何其他解释需要未来的合约版本，并且必须被 v1 解码器拒绝。

预测必须满足以下所有条件：

- `target`、`unit` 和 `sign_convention` 与任务声明完全匹配；
- 点时间戳与请求的未来范围完全匹配，并且节奏；
- 点数等于 `options.horizon_steps`；
- 时间戳严格递增并使用 v1 `interval_end` 语义；
- 值和分位数值是有限的；
- 返回的分位数概率与请求的集合完全匹配；
- 分位数值在每个点的概率不递减时间戳；
- 当返回概率 `0.5` 时，任务声明它是否必须等于主 `value` 或者可能是一个单独的估计器。

Aether 必须拒绝违反这些不变量的语法上成功的处理器响应。

## 回退语义

回退是由指定的、批准的策略生成的可用处理器输出。经过Aether验证并盖章后才成为衍生数据；回退并不是隐藏处理器故障的方法。
```json
{
  "schema": "aether.data-processing.result.v1",
  "request_id": "0190aee6-2139-7a87-8448-806f1b843201",
  "task": {
    "id": "energy.site-load-forecast",
    "revision": 1,
    "kind": "forecast"
  },
  "binding": {
    "id": "site-a",
    "revision": 7
  },
  "input_digest": "sha256:8b227777d4dd1fc61c6f884f48641d02b50a8a461a77f8fae7f48e32fbd8c372",
  "status": "fallback",
  "issued_at": "2026-07-11T12:00:02Z",
  "expires_at": "2026-07-11T12:30:00Z",
  "input_watermark": "2026-07-11T12:00:00Z",
  "processor": {
    "id": "load-forecasting-edge",
    "version": "0.1.0",
    "contract": "aether.data-processing.forecast.v1"
  },
  "fallback": {
    "strategy": "persistence",
    "strategy_version": "1",
    "reason_code": "MODEL_UNAVAILABLE",
    "source_feature": "load",
    "based_on_data_through": "2026-07-11T11:45:00Z"
  },
  "output": {
    "schema": "aether.data-processing.output.forecast.v1",
    "kind": "forecast",
    "target": "load",
    "unit": "kW",
    "sign_convention": "positive_consumption",
    "cadence_seconds": 900,
    "timestamp_semantics": "interval_end",
    "points": [
      {"timestamp": "2026-07-11T12:15:00Z", "value": 835.0},
      {"timestamp": "2026-07-11T12:30:00Z", "value": 835.0}
    ]
  },
  "warnings": ["MODEL_FALLBACK_USED"]
}
```

回退规则：

- 任务必须显式允许指定策略；
- 响应必须使用 `status: fallback` 并包含原因；
- `expires_at` 应短于正常模型输出；
- 持久性或历史平均策略必须使用实际请求数据；
- 零系列仅在以下情况下有效该任务明确定义零作为其基线，并且响应仍然将其标记为后备；
- 消费者决定是否可以接受特定的后备方案。处理器无法默默地促进回退到 `produced`。

对于功率数据，零是一个合理的物理值。因此，在没有回退标签的情况下在技术故障后返回零尤其危险。

## 不可用的语义

当处理器处理有效请求但无法产生任何满足任务策略的结果时使用 `status: unavailable` — 例如，不存在经批准的模型或允许的回退缺乏足够的观察结果。
```json
{
  "schema": "aether.data-processing.result.v1",
  "request_id": "0190aee6-2139-7a87-8448-806f1b843201",
  "task": {
    "id": "energy.site-load-forecast",
    "revision": 1,
    "kind": "forecast"
  },
  "binding": {
    "id": "site-a",
    "revision": 7
  },
  "input_digest": "sha256:8b227777d4dd1fc61c6f884f48641d02b50a8a461a77f8fae7f48e32fbd8c372",
  "status": "unavailable",
  "issued_at": "2026-07-11T12:00:02Z",
  "input_watermark": "2026-07-11T12:00:00Z",
  "processor": {
    "id": "load-forecasting-edge",
    "version": "0.1.0",
    "contract": "aether.data-processing.forecast.v1"
  },
  "unavailable": {
    "reason_code": "INSUFFICIENT_HISTORY",
    "retryable": true,
    "retry_after_seconds": 900
  },
  "warnings": []
}
```

在此状态下禁止 `output`、`expires_at` 和明显可用的默认值。

## 已接受的 `DerivedData`

`ProcessingResult` 是不受信任的处理器输出。在所有相关性、模式、范围、出处和过期检查通过后，Aether 将接受的输出标记为 `DerivedData`：
```json
{
  "schema": "aether.derived-data.v1",
  "result_id": "0190aee6-22ac-72da-b214-629a31ccb99c",
  "request_id": "0190aee6-2139-7a87-8448-806f1b843201",
  "task": {
    "id": "energy.site-load-forecast",
    "revision": 1,
    "kind": "forecast"
  },
  "binding": {
    "id": "site-a",
    "revision": 7
  },
  "accepted_at": "2026-07-11T12:00:03Z",
  "expires_at": "2026-07-11T13:00:00Z",
  "input_digest": "sha256:8b227777d4dd1fc61c6f884f48641d02b50a8a461a77f8fae7f48e32fbd8c372",
  "processing_status": "produced",
  "processor": {
    "id": "load-forecasting-edge",
    "version": "0.1.0"
  },
  "artifact": {
    "kind": "model",
    "family": "site-load",
    "version": "v3",
    "artifact_digest": "sha256:f04c532f2f814a3690f0f40e6f26fa82b0d69b9c510e7c0bb9f9f4de35b5a882"
  },
  "quality": {
    "input_watermark": "2026-07-11T12:00:00Z",
    "missing_ratio": 0.0,
    "fallback_used": false
  },
  "data": {
    "schema": "aether.data-processing.output.forecast.v1",
    "kind": "forecast",
    "target": "load",
    "unit": "kW",
    "sign_convention": "positive_consumption",
    "cadence_seconds": 900,
    "timestamp_semantics": "interval_end",
    "points": [
      {"timestamp": "2026-07-11T12:15:00Z", "value": 846.2},
      {"timestamp": "2026-07-11T12:30:00Z", "value": 852.7}
    ]
  }
}
```

如果验证失败，应用程序会记录 `rejected` 结果，并且不会创建 `DerivedData`。因此，`rejected` 是应用程序结果，而不是处理器可能断言其自身响应的状态。

## 错误包络和 HTTP 映射

契约、传输、容量和意外处理器故障使用带有类型错误的非 2xx 响应。它们与状态为 `unavailable` 的已完成 `ProcessingResult` 不同。
```json
{
  "schema": "aether.data-processing.error.v1",
  "request_id": "0190aee6-2139-7a87-8448-806f1b843201",
  "code": "FRAME_INVALID",
  "category": "invalid_data",
  "message": "history.load contains a missing sample",
  "retryable": false,
  "details": {
    "path": "/frame/history/features/load/values/41",
    "rule": "task missing_policy is reject"
  }
}
```

| HTTP | 类别 | 示例代码 | 重试 |
|------|----------|---------------|-------|
| 400 | `invalid_request` | `SCHEMA_UNSUPPORTED`、`OPTION_UNKNOWN` | 不 |
| 401/403 | `authorization` | `PROCESSOR_AUTH_REQUIRED`、`PROCESSOR_AUTH_DENIED` | 仅在凭证/策略更改后 |
| 404 | `not_found` | `MODEL_NOT_FOUND`、`TASK_NOT_SUPPORTED` | 否，除非部署发生变化 |
| 413 | `resource_limit` | `FRAME_TOO_LARGE` | 仅在减少请求后 |
| 422 | `invalid_data` | `FRAME_INVALID`、`QUALITY_REJECTED`、`UNIT_UNSUPPORTED` | 仅在数据更改后 |
| 429 | `capacity` | `PROCESSOR_BUSY` | 是的，尊敬的`retry_after_seconds` |
| 500 | `internal` | `PROCESSOR_INTERNAL` | 政策依赖 |
| 503 | `unavailable` | `MODEL_RUNTIME_UNAVAILABLE` | 是，当标记为可重试时 |
| 504 | `timeout` | `DEADLINE_EXCEEDED` | 是的，有新的截止日期 |

错误消息和详细信息不得公开凭据、模型文件系统路径、SQL 语句、原始堆栈跟踪或未声明的源数据。Aether适配器将这些错误映射为类型化端口错误，并保留处理器的稳定代码以进行诊断。

## 验证订单

合格的应用程序/适配器应按以下顺序进行验证：

1. 大小、媒体类型、JSON 语法和合约主要版本；
2. 请求身份、截止日期和规范摘要；
3. 配置的任务 ID、修订版、种类和处理器路线；
4. 特征集、值类型、单位和符号约定；
5. 时间戳顺序、节奏、窗口边界和数组长度；
6. 根据任务政策进行抽样和总体质量；
7. 针对处理器描述符的可选工件选择器；和
8. 结果相关性、出处、类型化输出模式、范围和到期日。

验证失败永远不会触发设备写入，也永远不会改变 SHM。失败的处理器调用也不会使历史记录、采集、警报或确定性安全行为变得不可用。

## 非幂等执行和重试

`data_processing.process` 声明为 `idempotent: false`。尽管查询不会改变Aether状态或控制设备，但调用它可以执行本地或远程处理器工作并创建新的所需审核记录。版本 1 没有重播存储、请求重复数据删除契约、精确结果保证或重用请求 ID 的特殊 `409` 行为。

输入摘要是准确的内容标识，而不是操作标识。仅当键入的错误将失败标记为可重试时，调用方才可以重试，并且应该尊重重试元数据，使用新的截止日期，并假设先前的处理器工作可能已经运行。确定性实现可以在离线黄金测试中使用固定框架和工件；该属性不会将公共操作转变为精确重播API。

## 能力元数据

连接此功能的每个传输都必须使用并公开相同的应用程序元数据。版本 1 目前通过 `aether-api` 上经过身份验证的 HTTP 路由公开应用程序； CLI 和 MCP 绑定仍需后续工作。应用程序目录中实现的基线描述符是：

| 能力 | 种类 | 风险 | 允许 | 确认 | 审计 | 幂等 |
|------------|------|------|------------|--------------|-------|------------|
| `data_processing.tasks.list` | 询问 | 低的 | `data_processing.read` | 绝不 | 不需要 | 是的 |
| `data_processing.processors.health` | 询问 | 低的 | `data_processing.read` | 绝不 | 不需要 | 是的 |
| `data_processing.process` | 查询 | 中 | `data_processing.run` | 策略 | 必需 | 否 |

`data_processing.process`仍然是查询，因为它生成派生数据并且不会改变设备或Aether状态。保守地说，策略驱动的确认属于中等风险，因为配置的远程处理器可能会导致遥测数据离开边缘主机。部署可以批准仅限本地的路由，而无需每次调用​​确认；路由的数据边界仍然是可发现的。任务和运行状况发现不包括观察值，也不需要持久的审计记录。处理确实会读取任务范围的操作数据，因此当所需的审核接收器无法记录调用时，它会失败关闭。

面向应用程序的路由是：

- `GET /api/v1/data-processing/tasks`;
- `GET /api/v1/data-processing/processors/health`;和
- `POST /api/v1/data-processing/process`。

仅当显式启用数据处理时才会安装这些路由。JWT 角色映射向查看者、工程师和管理员授予发现权限；执行处理仅限于工程师和管理员。面向处理器的伴生服务路由仍然是独立的 `POST /v1/process` 边界。

任务绑定、路由和批准的工件仅通过现有的受控配置路径进行更改。处理请求不得激活工件作为副作用。设备控制也是独立的，并且仍然是通过 `ControlApplication` 的高风险命令。

任务发现返回实际委托的路由策略。代表性条目（此处缩写为完整的 `features` 数组）具有以下嵌套形状：
```json
{
  "task": {"id": "energy.site-load-forecast", "revision": 1},
  "binding": {"id": "energy.example-site", "revision": 1},
  "kind": "forecast",
  "processor_contract": "aether.data-processing.forecast.v1",
  "features": [
    {
      "name": "load",
      "role": "history",
      "value_type": "number",
      "unit": "kW",
      "integer": false
    }
  ],
  "forecast": {
    "target": {
      "name": "load",
      "unit": "kW",
      "sign_convention": "positive_consumption"
    },
    "cadence_ms": 900000,
    "history_aggregation": "mean",
    "history_duplicate_policy": "latest",
    "history_feature_policies": [
      {"feature": "load", "aggregation": "mean", "duplicate_policy": "latest"},
      {"feature": "temp_avg", "aggregation": "mean", "duplicate_policy": "latest"},
      {"feature": "humidity", "aggregation": "mean", "duplicate_policy": "latest"},
      {"feature": "rain", "aggregation": "sum", "duplicate_policy": "latest"},
      {"feature": "quarter_hour", "aggregation": "last", "duplicate_policy": "reject"}
    ],
    "history_steps": 672,
    "max_horizon_steps": 288,
    "max_quantiles": 0,
    "max_output_age_ms": 3600000,
    "max_missing_ratio": 0.0,
    "max_input_age_ms": 900000,
    "max_gap_ms": 1800000,
    "require_future_issue_time": true,
    "allowed_fallbacks": ["persistence"],
    "fallback_policies": [
      {
        "strategy": "persistence",
        "version": "1",
        "source_feature": "load",
        "max_output_age_ms": 1800000
      }
    ]
  },
  "artifact": {
    "kind": "model",
    "family": "site-load",
    "version": "v3",
    "digest": "sha256:98967bdedc60b8ab555e596516eb272063c139ccf3a3112fb29a46ab0610f270"
  },
  "processor_id": "load-forecasting-edge",
  "processor_version": "0.1.0",
  "data_boundary": "local",
  "deadline_ms": 5000,
  "audit_finalization_timeout_ms": 1000,
  "max_concurrency": 1,
  "max_frame_samples": 5000,
  "max_request_bytes": 4194304
}
```

`deadline_ms` 是框架组装加上处理器工作的硬预算，而不是完整的 HTTP 响应 SLA​​。 `audit_finalization_timeout_ms` 发布单独的强制终端审核限额，因此观察到的 API 调用最多可以完成这两个字段的总和。审核失败仍然无法关闭请求。

然后，AI 客户端可以区分观察或解释预测与激活模型或调度控制计划。

## 兼容性规则

- 处理器可以同时支持多个主要合约，但每个请求只选择一个。
- 新任务类型接收类型化选项架构和类型化输出架构。在预测字段成为通用 blob 之前，请勿展开它们。
- 重命名功能、更改单位或符号约定、更改时间戳语义或更改缺失数据规则需要进行新的任务修订。
- 删除兼容性适配器需要一致性测试和规定的迁移标准。
- 域包可能需要最低处理器契约，但只有组合根才能选择实际适配器或端点。

## 相关页

- [连接数据处理器](/guides/data-processors) — 声明任务并路由处理器
- [AetherEMS 功率预测](https://github.com/EvanL1/AetherEMS/blob/main/packs/energy/knowledge/power-forecasting.md) — 第一个下游预测合约
- [JSON Schemas](https://github.com/EvanL1/AetherEdge/blob/main/contracts/data-processing/README.md) — 严格的机器可读 v1 线路Guards
- [负载预测处理器](https://github.com/EvanL1/AetherEdge/blob/main/integrations/load-forecasting/README.md) — 请求驱动的边缘平台实现
- [数据流](/concepts/data-flow) — SHM 和历史权威
- [HTTP 数据处理器](/extensions/http-data-processor) — v1 处理器的有界可选实现Transport
- [HTTP API](/reference/http-api) — Aether 面向应用程序的 API 的服务信封约定
