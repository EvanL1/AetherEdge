// 统计相关类型定义

// 操作日志记录类型
export interface OperationLogRecord {
  id: string
  user: string
  role: string
  action: string
  device: string
  result: string
  time: string
  ip: string
}
