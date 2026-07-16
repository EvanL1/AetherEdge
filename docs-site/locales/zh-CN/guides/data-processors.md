---
title: "连接数据处理器"
description: "在域包中声明数据处理任务并连接请求驱动的本地或远程处理器"
updated: 2026-07-11
---

# 连接数据处理器

此页面介绍了 **Aether 数据处理** 的实施集成模式。此仓库中提供了核心类型、应用程序编排、v1 编解码器、本地测试适配器、有界 HTTP 适配器、架构和 energy-pack 示例。选择加入的 `aether-api` 组合会读取严格的运行时配置；完整的合成模板是 [`packs/energy/data-processing/runtime.example.yaml`](https://github.com/EvanL1/AetherEdge/blob/main/packs/energy/data-processing/runtime.example.yaml)。

数据处理器接收完整的受控输入帧并返回派生数据。它不会回到Aether来发现自己的输入。这使得同一处理器可用作进程内适配器、本地 sidecar 或经批准的远程服务，而无需更改数据所有权。
```text
caller
  │
  ▼
DataProcessingApplication
  ├─ task declaration from a domain pack
  ├─ HistoryQuery
  ├─ LiveState (read-only)
  ├─ CovariateSource
  └─ deterministic transforms
            │
            ▼
      ProcessingFrame
            │ request
            ▼
       DataProcessor
            │
            ▼
 ProcessingResult (untrusted)
            │ validate and stamp
            ▼
        DerivedData
```

这种划分是经过深思熟虑的：

- Aether 拥有点身份、源选择、时间对齐、单元/签名契约验证、质量政策、授权和审核。
- A `DataProcessor` 拥有处理器特定的功能排序、缩放器、张量构造、模型执行和输出后处理。
- 域包声明任务的语义。它不选择具体端点或包含凭据。
- 派生数据不是实时设备状态。处理器无法编写 SHM、历史存储或设备命令。

## 在域包中声明任务

将任务声明与定义其输入和输出语义的行业知识一起放置。仓库约定为 `packs/<domain>/data-processing/tasks/*.yaml`；能源包在那里提供完整的、默认禁用的负载和光伏示例。组合根或调试加载程序将经过验证的资产转换为启用的路由。

以下负载预测任务是说明性的：
```yaml
schema: aether.data-processing-task.v1
id: energy.site-load-forecast
revision: 1
kind: forecast
processor_contract: aether.data-processing.forecast.v1

target:
  name: load
  semantic_point: site.load.active_power
  unit: kW
  sign_convention: positive_consumption

frame:
  cadence_seconds: 900
  history_steps: 672
  horizon_steps: 288
  timezone: UTC
  live_tail: forbidden

inputs:
  history:
    - name: load
      unit: kW
      source:
        kind: measurement
        instance_ref: site_load
        point_ref: active_power
    - name: temp_avg
      unit: Cel
      source:
        kind: covariate
        dataset_ref: weather.observed
        field: air_temperature
    - name: humidity
      unit: "%"
      source:
        kind: covariate
        dataset_ref: weather.observed
        field: relative_humidity
    - name: rain
      unit: mm
      source:
        kind: covariate
        dataset_ref: weather.observed
        field: precipitation
    - name: quarter_hour
      unit: "1"
      source:
        kind: calendar
        transform: quarter_hour_of_day_zero_based

  future_covariates:
    - name: temp_avg
      unit: Cel
      source:
        kind: covariate
        dataset_ref: weather.nwp
        field: air_temperature
    - name: humidity
      unit: "%"
      source:
        kind: covariate
        dataset_ref: weather.nwp
        field: relative_humidity
    - name: rain
      unit: mm
      source:
        kind: covariate
        dataset_ref: weather.nwp
        field: precipitation
    - name: quarter_hour
      unit: "1"
      source:
        kind: calendar
        transform: quarter_hour_of_day_zero_based

alignment:
  aggregation: mean
  timestamp_semantics: interval_end
  duplicate_policy: latest
  feature_policies:
    - { name: load, aggregation: mean, duplicate_policy: latest }
    - { name: temp_avg, aggregation: mean, duplicate_policy: latest }
    - { name: humidity, aggregation: mean, duplicate_policy: latest }
    - { name: rain, aggregation: sum, duplicate_policy: latest }
    - { name: quarter_hour, aggregation: last, duplicate_policy: reject }
  missing_policy: reject

quality:
  max_input_age_seconds: 900
  max_gap_seconds: 1800
  max_missing_ratio: 0.0
  require_input_watermark: true
  require_covariate_issue_time_not_after_as_of: true

output:
  unit: kW
  sign_convention: positive_consumption
  max_quantiles: 0
  expires_after_seconds: 3600
```

### 属于任务的内容

声明应仅包含可移植域语义：

- 稳定的任务 ID、修订版和类型化任务类型；
- 语义实例和点引用，而不是通道 ID、SHM 槽、SQL 名称或供应商寄存器地址；
- 规范单位和符号约定；
- 历史和已知未来特征；
- 节奏、窗口、对齐和缺失数据规则；
- 预期的派生数据形状和新鲜度限制；以及
- 任务所需的处理器契约。

站点调试将 `instance_ref` 和 `point_ref` 解析为实际实例和路由。如果无法解析所需的参考或其物理单位、比例、偏移、点类型或目标符号约定与委托任务不完全匹配，则包验证必须失败关闭。当前的 v1 运行时验证了这些事实；它不执行工程单位或符号转换。

声明不得包含：

- 处理器 URL、进程名称、API 令牌或 TLS 密钥；
- 具体历史数据库查询；
- SHM 路径或布局假设；
- ONNX/RKNN 输入节点名称；或
- 要从结果执行的设备操作。

这些详细信息属于组合、适配器或单独的控制用例。

## 实现`DataProcessor`

故意缩小实现的端口。在类似 Rust 的伪代码中：
```rust
#[async_trait]
pub trait DataProcessor: Send + Sync {
    fn descriptor(&self) -> &DataProcessorDescriptor;

    async fn health(&self) -> PortResult<ProcessorHealth>;

    async fn process(
        &self,
        request: DataProcessingRequest,
    ) -> PortResult<ProcessingResult>;
}
```

请求和结果是[数据处理契约](/reference/data-processing-contracts) 中记录的类型化契约。处理器描述符声明支持的合约版本、任务类型、本地/远程数据边界以及有限帧/请求限制。它不得公开通用供应商命令集或未经验证的`run(json)`逃生舱口。

处理器负责：

- 拒绝不受支持的契约版本、任务类型、功能和形状；
- 应用所选模型拥有的功能顺序和规范化；
- 执行其确定性算法或模型运行时；
- 返回实际模型版本和工件摘要；
- 区分正常结果、显式回退和无可用结果；并
- 遵守最后期限和有限的资源限制。

处理器不得：

- 查询 Aether SHM、SQLite、历史服务或域包目录；
- 仅接受 `plant_id`，然后发现相应的数据本身；
- 从站点推断单位或功率方向名称；
- 将派生值作为普通实时测量值发布；
- 调度设备命令；或
- 将处理失败转变为明显正常的零值结果。

对于模型支持的处理，将模型内部保留在处理器端。处理器可以拥有特征顺序、缩放器统计、序列长度、模型工件、ONNX/RKNN 选择和反规范化。 Aether 发送命名的、包含单位的观测值，而不是模型张量。

## 配置本地和远程处理器

只有组合根才会选择具体的处理器。严格的运行时 YAML 包含完整的任务、绑定、历史路由、协变量源和处理器描述符。这个简短的片段仅显示了处理器部分；不要将其用作完整配置。 `HttpDataProcessorConfig` 接收经过验证的端点并派生固定的 `/v1/process` 和 `/v1/health` 路由：
```yaml
processor:
  endpoint: http://127.0.0.1:8989/
  id: load-forecasting-edge
  version: 0.1.0
  contract: aether.data-processing.forecast.v1
  requires_artifact: true
  boundary: local
  max_frame_cells: 5000
  max_request_bytes: 4194304
  connect_timeout_ms: 500
  request_timeout_ms: 4500
  max_response_bytes: 4194304
  bearer_token_env: AETHER_LOAD_FORECASTING_BEARER_TOKEN
```

通用域/处理器合约支持静态功能，自定义进程内组合可以通过 `DataProcessingBinding` 绑定它们。当前的 `aether-api` 运行时 YAML 加载器没有静态值绑定字段，因此运行时配置的 v1 路由不得声明静态功能。在调试之前添加装载机支持和测试；不要假设仅线路模式就可以使用它。

本地和远程适配器使用相同的请求/结果协定。远程路由添加显式数据出口边界，并且必须由任务和路由策略预先批准。秘密保留在环境拥有的秘密注入中。当前配置没有自定义 CA 文件设置； HTTPS 使用 HTTP 客户端配置的信任根。需要私有信任材料的部署必须通过受支持的传输组合来提供它，而不是发明 YAML 密钥。

默认的 Aether 发行版不得需要任何处理器。如果没有配置路由，则任务不可用，其余的采集、历史记录、警报、规则和设备控制将独立继续。

## Aether 如何组装请求

`DataProcessingApplication`对每个请求拥有以下步骤，并由委托的组装组合提供特定于任务的转换：

1. 授权公布的处理能力并加载任务修订版。
2. 解析语义通过委托实例映射进行源引用。
3. 通过 `HistoryQuery` 查询所需的历史范围。
4. 当任务允许实时尾部并且该功能使用 `aggregation: last` 时，通过只读 `LiveState` 端口读取最新值，然后仅替换其最终间隔单元格，而不更改 SHM 权限。版本 1 拒绝 `mean`、`sum`、`min` 或 `max` 的实时尾部，因为瞬时值无法表示聚合存储桶。加载/PV 任务禁止这样做。
5. 通过类型化 `CovariateSource` 获取未来已知的输入。
6. 在本地生成确定性日历功能。
7. 验证准确的委托单元/符号元数据、对齐时间戳、聚合原始观察结果、解决重复项并应用声明的缺失数据策略。版本 1 不执行运行时单位/符号转换。
8. 计算帧质量和规范输入摘要。
9. 选择配置的处理器并提交一个完整的 `ProcessingFrame`。
10. 在公开 `DerivedData` 之前验证返回的任务 ID、请求 ID、输入摘要、时间戳、单位、状态、模型出处和过期时间。

对于间隔结束节奏 `c`，历史标签为 `as_of-(history_steps-1)c, ..., as_of`。标签 `t` 聚合 `(t-c, t]` 中的原始观察结果，并且任何源读取都不得超出 `as_of`。未来协变量和预测输出从 `as_of+c` 开始。默认运行时通过只读 SQLite 适配器针对 `aether-history.db` 实现此功能；一个请求的所有功能读取共享一个事务。可选的 HTTP 历史适配器仅适用于已实现精确节奏网格并支持受限 `last/reject` 策略的上游服务。

元数据检查不能证明间隔含义。在启用物理路由之前，请使用站点黄金测试夹具来验证每个原始源的聚合和对齐语义。这对于 `rain` 是强制性的：`aggregation: sum` 仅当每个源值是其时间间隔内的增量降水量而不是滚动累积或速率时才有效。

当前的 SQLite 历史记录也不提供时间点回测削减。行没有摄取时间戳或源/配置纪元，因此未更改的逻辑系列后面的后期回填和物理重新映射可以更改或拼接旧的事件时间窗口。绑定修订仅验证当前路由。离线评估需要在评估过程中捕获的冻结历史记录导出，或双时态、带有纪元的历史适配器。也冻结工件注册表：v1 工件标识具有版本和摘要，但没有 `trained_through` 或 `available_at`，因此旧的 `as_of` 本身无法排除稍后发布的模型。

将历史记录存储更改视为维护。 `history_config.storage_*` 行已保存意图，`PUT /hisApi/storage` 不会重新连接活动写入器。禁用处理，应用重新连接或历史服务重新启动，验证活动 SQLite 后端以及委托的哨兵系列，然后才针对同一路径重新启动 `aether-api`。

通过文件系统隔离支持此操作：为 API 提供对历史数据库/WAL/SHM 目录的独立许可只读访问权限，与其可写配置/审核数据库分开。基本 Compose `/app/data:rw` 安装和 SQLite 只读标志本身并不是生产权限边界。

处理器不会获取 Aether 数据库凭据，也不会获得反向回调 URL。如果需要另一次观察，则任务声明不完整，必须进行修改，而不是允许未声明的读取。

## 调用应用程序API

版本 1 由选择加入、受 JWT 保护的 `aether-api` 路由公开：

- `GET /api/v1/data-processing/tasks`;
- `GET /api/v1/data-processing/processors/health`;和
- `POST /api/v1/data-processing/process`。

查看者、工程师和管理员角色可以使用发现；处理需要工程师或管理员。进程主体是严格的应用程序请求，而不是完整的框架或处理器端点。 `x-request-id` 是可选的，`x-aether-confirmed: true` 在路由策略需要时提供显式确认。 v1 中未实现这些功能的 CLI 和 MCP 绑定；未来的传输必须调用相同的应用程序 API 而不是 sidecar。

## 在开发期间直接请求处理器

可选的 HTTP 适配器将 `DataProcessor::process` 映射到版本化的面向处理器的 `POST /v1/process` 端点。仓库的负载预测集成实现了该端点；它是处理器边界，而不是默认 Aether 服务上面向应用程序的端点。
```bash
curl --fail-with-body \
  --request POST http://127.0.0.1:8989/v1/process \
  --header 'Content-Type: application/vnd.aether.data-processing+json;version=1' \
  --data @processing-request.json
```

直接调用对于处理器一致性测试非常有用。生产调用者应通过 `DataProcessingApplication` 输入，以便无法绕过源解析、数据质量、策略和结果验证。

## 重试和内容标识

`data_processing.process` 是只读查询，但特意声明了 `idempotent: false`。版本 1 没有请求重播存储、重复数据删除契约、精确结果保证或 `409 REQUEST_ID_REUSED` 行为。每个接受的调用都会收到自己的审核记录，并可能再次执行处理器。

`input_digest` 标识确切的任务/绑定修订版、框架、工件选择器、契约和选项。支持关联和离线比较；它不是公共操作的幂等密钥。调用者只能在其自己的有限策略下重试类型化的可重试 `429`、`503` 或 `504` 响应，并具有新的截止日期并意识到工作可能已经运行。

## 一致性检查表

在将任务路由到处理器之前，验证它：

- 接受完整的帧并且不生成反向数据读取；
- 验证参考页面中的每个合约不变量；
- 拒绝 NaN、无穷大、时间戳无序、单位不匹配和未声明的特征；
- 可以在离线黄金测试中重现固定帧，其中算法是确定性的，而不将该测试属性视为 API 重放承诺；
- 不调用当前历史记录加上当前模型选择时间点回测；需要冻结历史记录和工件剪切，或等效的双时态/可用性元数据；
- 将超时、断开连接、过载和格式错误的响应处理为类型化故障；
- 标记回退输出，并且从不将不可用数据伪装为预测；
- 返回模型和处理器出处，无需本地文件系统路径或秘密；
- 传递正常、缺失、陈旧和边界数据；并
- 证明处理器丢失不会阻止采集或确定性安全行为。

当前的 SQLite 历史架构不保留设备源样本质量，并且当前的 SHM 桥标签接受有限实时值作为 `good`。新鲜度、差距、缺失、数字限制、发布时间和出处均受到强制执行，但需要端到端源质量保真度的部署必须在生产前委托具有质量的源适配器。

## 相关页面

- [数据处理契约](/reference/data-processing-contracts) — v1 传输契约和验证规则
- [HTTP数据处理器](/extensions/http-data-processor) — 有界本地/远程适配器和组合 API
- [AetherEMS 功耗预测](https://github.com/EvanL1/AetherEMS/blob/main/packs/energy/knowledge/power-forecasting.md) — 第一个下游任务和处理器
- [负载预测处理器](https://github.com/EvanL1/AetherEdge/blob/main/integrations/load-forecasting/README.md) — 针对现有边缘平台经过测试的 `/v1/process` 适配器
- [JSON Schemas](https://github.com/EvanL1/AetherEdge/blob/main/contracts/data-processing/README.md) — 严格的 v1 传输验证
- [数据流](/concepts/data-flow) — 权威的实时和历史路径
- [系统架构](/concepts/architecture) — 核心层和服务边界
- [应用程序和代理的安全操作](/guides/safe-operations) — 派生数据为何如此不绕过控制策略
