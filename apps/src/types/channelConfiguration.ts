// 通道基础信息
export interface ChannelBasic {
  id?: number
  name: string
  description: string | null
  protocol: 'modbus_tcp' | 'can' | 'virt' | 'modbus_rtu' | 'di_do'
  enabled: boolean
}
export interface ChannelListItem extends ChannelBasic {
  connected: boolean
  last_update: string
  error_count: number
  last_error: string | null
}
export interface modbusTcpParams {
  host: string
  port: number
  retry_count?: number
  timeout_ms?: number
  max_batch_size: number
  poll_interval_ms: number
}
export interface canParams {
  bitrate: number
  data_bitrate: number
  fd_mode: boolean
  interface: string
  listen_only: boolean
  loopback: boolean
  timeout_ms: number
}
export interface virtualParams {
  update_interval_ms: number
}
export interface modbusRtuParams {
  baud_rate: number
  data_bits: number
  device: string
  parity: string
  poll_interval_ms: number
  retry_count: number
  stop_bits: number
  timeout_ms: number
  max_batch_size: number
}
// 通道详情信息
export interface ChannelDetail extends ChannelBasic {
  parameters: {
    [key: string]: any
    parameters: modbusTcpParams | canParams | virtualParams | modbusRtuParams
  }
  runtime_status?: {
    connected: boolean
    running: boolean
    last_update: string
    error_count: number
    last_error: string | null
    statistics?: {
      [key: string]: any
    }
  }
  point_counts?: {
    telemetry: number
    signal: number
    control: number
    adjustment: number
  }
  logging: {
    enabled: boolean
    level: 'info' | 'debug'
  }
}
export interface updateChannelDetail extends ChannelBasic {
  parameters: {
    [key: string]: any
  }
  logging?: {
    enabled: boolean
    level: 'info' | 'debug'
  }
}
export type PointType = 'T' | 'S' | 'C' | 'A'
// 点位信息
export interface PointInfo {
  point_id: number
  signal_name: string
  scale: number
  offset: number
  unit: string
  data_type: string
  reverse: boolean
  description: string
  // 实时值（通过WebSocket更新）
  value?: number
  rowStatus?: 'normal' | 'modified' | 'added' | 'deleted'
  modifiedFields?: string[] // 记录哪些字段被修改了
  isEditing?: boolean // 标记该行是否处于编辑状态
  isNewUnconfirmed?: boolean // 标记是否为未确认的新增行
  originalData?: Record<string, any> // 备份原始数据用于取消编辑，可以存储 PointInfo 字段或 mapping 字段
  protocol_mapping?: {
    bit_position?: number
    byte_order?: string
    data_type?: string
    function_code?: number
    register_address?: number
    slave_id?: number
  }
  config?: {
    realTimeValue?: any
  }
  has_mapping?: boolean
}
export interface PointInfoResponse {
  telemetry: PointInfo[]
  signal: PointInfo[]
  control: PointInfo[]
  adjustment: PointInfo[]
}
// 点位配置参数
export interface modbusPointMapping {
  byte_order: string
  data_type: string
  function_code: number
  bit_position: number
  point_id?: number
  register_address: number
  slave_id: number
}
export interface virtualPointMapping {
  expression: string
}
export interface CanPointMapping {
  bit_length: number
  byte_order: string
  can_id: number | string
  data_type: string
  offset: number
  scale: number
  signed: boolean
  start_bit: number
}
export interface mappingResponse {
  point_id: number
  signal_name?: string
  protocol_data: modbusPointMapping | virtualPointMapping | CanPointMapping
}
export interface UpdateMappingPoint {
  four_remote: PointType
  point_id: number
  protocol_data: modbusPointMapping | virtualPointMapping | CanPointMapping
}
export interface BatchUpdateMappingPointRequest {
  mappings: UpdateMappingPoint[]
  mode: 'replace' | 'merge'
  reload_channel: boolean
  validate_only: boolean
}
// /mappings 接口返回结构：{ telemetry: mappingResponse[], signal: mappingResponse[], control: mappingResponse[], adjustment: mappingResponse[] }
export interface MappingCategoryResponse {
  telemetry: mappingResponse[]
  signal: mappingResponse[]
  control: mappingResponse[]
  adjustment: mappingResponse[]
}
// 筛选条件
export interface ChannelFilters {
  protocol: string
  enabled: boolean | null
  connected: boolean | null
}

// 协议选项
export const PROTOCOL_OPTIONS = [
  { label: 'modbus_tcp', value: 'modbus_tcp' },
  { label: 'modbus_rtu', value: 'modbus_rtu' },
  { label: 'di_do', value: 'di_do' },
  // { label: 'can', value: 'can' },
  // { label: 'virt', value: 'virt' },
] as const
// 发布点位值请求
export interface PublishPointsRequest {
  type: 'C' | 'A' | 'T' | 'S'
  id?: string
  value?: number
  points?: Array<{ id: number; value: number }>
}
// 数据类型选项
export const DATA_TYPE_OPTIONS = [
  { label: 'float16', value: 'float16' },
  { label: 'float32', value: 'float32' },
  { label: 'float64', value: 'float64' },
  { label: 'int16', value: 'int16' },
  { label: 'int32', value: 'int32' },
  { label: 'int64', value: 'int64' },
  { label: 'uint16', value: 'uint16' },
  { label: 'uint32', value: 'uint32' },
  { label: 'uint64', value: 'uint64' },
  { label: 'bool', value: 'bool' },
] as const

// 字节序选项
export const BYTE_ORDER_OPTIONS = [
  { label: 'ABCD', value: 'ABCD' },
  { label: 'DCBA', value: 'DCBA' },
  { label: 'BADC', value: 'BADC' },
  { label: 'CDAB', value: 'CDAB' },
] as const

// 64位字节序选项
export const BYTE_ORDER_64_OPTIONS = [
  { label: 'ABCDEFGH', value: 'ABCDEFGH' },
  { label: 'HGFEDCBA', value: 'HGFEDCBA' },
  { label: 'BADCFEHG', value: 'BADCFEHG' },
  { label: 'GHEFCDAB', value: 'GHEFCDAB' },
] as const

// 功能码选项
export const FUNCTION_CODE_OPTIONS = [
  { label: 'Read Coils (01)', value: 1 },
  { label: 'Read Discrete Inputs (02)', value: 2 },
  { label: 'Read Holding Registers (03)', value: 3 },
  { label: 'Read Input Registers (04)', value: 4 },
  { label: 'Write Single Coil (05)', value: 5 },
  { label: 'Write Single Register (06)', value: 6 },
  { label: 'Write Multiple Coils (15)', value: 15 },
  { label: 'Write Multiple Registers (16)', value: 16 },
] as const

// 批量点位增删改请求类型
export interface PointDataPayload {
  data_type?: string
  description?: string
  offset?: number
  reverse?: boolean
  scale?: number
  signal_name?: string
  unit?: string
}
export interface CreatePointRequestItem {
  point_id: number
  point_type: PointType
  data: PointDataPayload
  force?: boolean
}
export interface UpdatePointRequestItem {
  point_id: number
  point_type: PointType
  data?: PointDataPayload
}
export interface DeletePointRequestItem {
  point_id: number
  point_type: PointType
}
export interface BatchPointsChangeRequest {
  create?: CreatePointRequestItem[]
  delete?: DeletePointRequestItem[]
  update?: UpdatePointRequestItem[]
}
