// 告警记录类型定义

// 规则快照类型
export interface RuleSnapshot {
  rule_name: string
  warning_level: number
  operator: string
  value: number | string
  description: string
}

// 当前告警数据类型
export interface CurrentAlarmData {
  id: number
  rule_id: number
  rule_snapshot: string // API 返回 JSON 字符串
  service_type: string
  channel_id: number
  data_type: string
  point_id: number
  rule_name: string
  warning_level: number
  operator: string
  threshold_value: number | string
  current_value: number | string
  status: 'active' | 'inactive'
  triggered_at: number
}

// 历史告警数据类型
export interface HistoryAlarmData {
  id: number
  rule_id: number
  rule_snapshot: string // API 返回 JSON 字符串
  service_type: string
  channel_id: number
  data_type: string
  point_id: number
  rule_name: string
  warning_level: number
  operator: string
  threshold_value: number | string
  trigger_value?: number | string
  recovery_value?: number | string | null
  event_type: 'trigger' | 'recovery'
  triggered_at: number
  recovered_at?: number
  duration?: number
}
export interface CurrentAlarmResponse {
  list: CurrentAlarmData[]
  total: number
}
export interface HistoryAlarmResponse {
  list: HistoryAlarmData[]
  total: number
}
// 告警级别枚举
export enum AlarmLevel {
  LEVEL_1 = 1,
  LEVEL_2 = 2,
  LEVEL_3 = 3,
}

// 操作符枚举
export enum AlarmOperator {
  GREATER_THAN = '>',
  LESS_THAN = '<',
  EQUAL = '==',
  NOT_EQUAL = '!=',
  GREATER_EQUAL = '>=',
  LESS_EQUAL = '<=',
}

// 事件类型枚举
export enum AlarmEventType {
  TRIGGER = 'trigger',
  RECOVERY = 'recovery',
}

// 告警状态枚举
export enum AlarmStatus {
  ACTIVE = 'active',
  INACTIVE = 'inactive',
}

// 服务类型枚举
export enum ServiceType {
  RULESRV = 'rulesrv',
}

// 数据类型枚举
export enum DataType {
  TEMPERATURE = 'T',
  STATUS = 'S',
  PRESSURE = 'P',
  VOLTAGE = 'V',
}

// 告警查询参数
export interface AlarmQueryParams {
  type: 'current' | 'history'
  warning_level?: AlarmLevel
  service_type?: ServiceType
  channel_id?: number
  data_type?: DataType
  point_id?: number
  status?: AlarmStatus
  event_type?: AlarmEventType
  start_time?: number
  end_time?: number
  page?: number
  page_size?: number
}
