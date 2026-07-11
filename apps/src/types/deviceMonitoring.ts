// 设备监控相关类型定义

// 左侧表格数据类型
export interface LeftTableItem {
  name: string
  value: number | null | string
  unit: string
  updateTime?: number | string | null
  pointId: number
}

// 右侧表格数据类型
export interface RightTableItem {
  name: string
  status: number | null | string
  pointId: number
  updateTime?: number | string | null
}
