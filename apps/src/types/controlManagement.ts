// 控制管理相关类型定义

// 操作符类型
export type Operator = '>' | '>=' | '<' | '<=' | '=' | 'gt' | 'gte' | 'lt' | 'lte' | 'eq'

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
}

// 规则信息类型
export interface RuleInfo {
  id: number
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
  created_at: string
}

// 对话框暴露类型
export interface DialogExpose {
  dialogVisible: boolean
}
