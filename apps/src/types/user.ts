// 用户信息接口（根据后端实际返回结构调整）
export interface UserRole {
  id: number
  name_en: string
  name_zh: string
  description: string
}

export interface UserInfo {
  id: number
  username: string
  is_active: boolean
  last_login?: string
  created_at?: string
  updated_at?: string
  role: UserRole
}

// 用户管理表格用户信息接口（根据后端结构调整）
export interface UserManagementInfo {
  id: number
  username: string
  is_active: boolean
  last_login: string
  created_at: string
  role: UserRole
}
//用户新增
export interface UserAddParams {
  username: string
  password?: string
  role_id: number
}

// 登录请求参数
export interface LoginParams {
  username: string
  password: string
  // role 字段可选，后端如不需要可去掉
  role?: string
  remember?: boolean
}

// 登录响应
export interface LoginResponse {
  access_token: string
  refresh_token: string
  expires_in: number
  token_type: string
}

// 注册请求参数
export interface RegisterParams {
  username: string
  password: string
  role: number
}

// 更新用户信息参数
export interface UpdateUserParams {
  username?: string
  is_active?: boolean
  role_id?: number
}

// API 响应格式
export interface ApiResponse<T = any> {
  code: number
  message: string
  data: T
  success: boolean
}
