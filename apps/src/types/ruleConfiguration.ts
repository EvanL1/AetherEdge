export interface FlowPosition {
  x: number
  y: number
}

export interface FlowNodePayload {
  id: string
  type?: string
  position: FlowPosition
  data?: Record<string, any>
}

export interface FlowEdgePayload {
  id: string
  source: string
  target: string
  sourceHandle?: string
  targetHandle?: string
}

export interface FlowJsonPayload {
  nodes: FlowNodePayload[]
  edges: FlowEdgePayload[]
}

export interface RuleChainPayload {
  cooldown_ms?: number
  description?: string
  enabled?: boolean
  flow_json: FlowJsonPayload
  format?: string
  id?: string
  name?: string
  priority?: number
}
export interface Rule {
  id: string
  name: string
  description?: string
  priority?: number
  enabled?: boolean
}

// 创建仅要求名称与可选描述，其它由后端填充
export type CreateRulePayload = {
  name: string
  description?: string
}
// 更新时可部分更新
export type UpdateRulePayload = Partial<{
  name: string
  description: string
  priority: number
  enabled: boolean
}>
// 规则链相关类型定义

export interface RuleChain {
  id: string
  name: string
  description: string
  priority: number
  enabled: boolean
  cooldown_ms: number
}

export interface RuleCard {
  id: string
  cardId?: string
  type:
    | 'filter'
    | 'attributes'
    | 'transform'
    | 'action'
    | 'external'
    | 'flow'
    | 'function-switch'
    | 'action-changeValue'
    | 'start'
    | 'end'
  description: string
  // 可选的展示与配置字段，供编辑对话框与节点显示使用
  label?: string
  config?: Record<string, any>
  name?: string
}

export interface Edge {
  id: string
  type: string
  source: string
  target: string
  sourceHandle: string
  targetHandle: string
  data: {
    color?: string
    strokeWidth?: number
    dash?: string
    animated?: boolean
    curved?: boolean
    [key: string]: any
  }
  label: string
  sourceX: number
  sourceY: number
  targetX: number
  targetY: number
}

export interface Node {
  id: string
  type: string
  position: { x: number; y: number }
  data: RuleCard
  [key: string]: any
}

export interface TooltipContent {
  title: string
  description: string
  examples?: string[]
}
