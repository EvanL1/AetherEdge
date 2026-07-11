// 用户管理相关类型定义

// 用户表单模型类型
export interface UserFormModel {
  username: string
  role_id: number
  is_active?: boolean
  password?: string
  confirmPassword?: string
}

// 对话框暴露类型
export interface DialogExpose {
  dialogVisible: boolean
}
