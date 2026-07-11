// 控制管理相关类型定义

// 操作符类型
export type Operator = '>' | '>=' | '<' | '<=' | '=' | 'gt' | 'gte' | 'lt' | 'lte' | 'eq'

// Trigger config — mirrors backend TriggerConfig enum (serde snake_case tag)
export interface TriggerConfigInterval {
  type: 'interval'
  interval_ms: number
}

export interface PointRef {
  instance: number
  point_type: 'measurement' | 'action'
  point: number
}

export interface TriggerConfigOnChange {
  type: 'on_change'
  point_refs: PointRef[]
  time_deadband_ms: number | null
  value_deadband: null
}

export type TriggerConfig = TriggerConfigInterval | TriggerConfigOnChange

// 规则表单模型类型
export interface RuleFormModel {
  rule_name: string
  service_type: string
  channel_id: number | undefined
  point_id: number | null
  data_type: 'T' | 'S' | null
  warning_level: number | null
  operator: Operator | null
  value: number | null
  description?: string
  enabled: boolean
  trigger_config?: TriggerConfig
}

// 规则信息类型
export interface RuleInfo {
  id: number
  channel_id?: number
  rule_name: string
  service_type: string
  point_id: number | null
  data_type: 'T' | 'S' | null
  warning_level: number | null
  operator: Operator | null
  value: number | null
  notification?: string[]
  enabled: boolean
  description?: string
  created_at: number // Unix 时间戳（秒）
  updated_at?: number // Unix 时间戳（秒）
  trigger_config?: string | null // stored as JSON string in DB
}

// 对话框暴露类型
export interface DialogExpose {
  dialogVisible: boolean
}

// GET /alarmApi/rules 列表响应
export interface RuleResponse {
  list: RuleInfo[]
  total: number
}

// GET /alarmApi/rules/{id} 单条规则响应
export interface RuleDetailResponse {
  list: RuleInfo[]
  total: number
}
