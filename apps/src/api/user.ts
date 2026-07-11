import { Request } from '@/utils/request'
import MD5 from 'crypto-js/md5'
import type {
  LoginParams,
  LoginResponse,
  RegisterParams,
  UpdateUserParams,
  UserInfo,
  ApiResponse,
  UserAddParams,
} from '@/types/user'

// 用户相关API接口
export const userApi = {
  /**
   * 用户登录
   * @param params 登录参数
   * @returns 登录响应
   */
  login(params: LoginParams): Promise<ApiResponse<LoginResponse>> {
    return Request.post('/api/v1/auth/login', params)
  },
  /**
   * 用户登出
   * @returns 登出响应
   */
  logout(refreshToken: string): Promise<ApiResponse<null>> {
    return Request.post('/api/v1/auth/logout', { refresh_token: refreshToken })
  },

  /**
   * 获取当前用户信息
   * @returns 用户信息
   */
  getUserInfo(): Promise<ApiResponse<UserInfo>> {
    return Request.get('/api/v1/auth/me')
  },
  /**
   * 刷新Token
   * @param refreshToken 刷新令牌
   * @returns 新的Token和RefreshToken
   */
  refreshToken(refreshToken: string): Promise<
    ApiResponse<{
      access_token: string
      refresh_token: string
      token_type: string
      expires_in: number
    }>
  > {
    // 标记这是刷新token请求，避免在响应拦截器中再次触发刷新逻辑
    return Request.post(
      '/api/v1/auth/refresh',
      { refresh_token: refreshToken },
      {
        _isRefreshTokenRequest: true,
      },
    )
  },
  /**
   * 获取用户详情
   * @param userId 用户ID
   * @returns 用户详情
   */
  getUserDetail(userId: number): Promise<ApiResponse<UserInfo>> {
    return Request.get(`/api/v1/auth/users/${userId}`)
  },
  /**
   * 添加用户
   * @param params 用户信息
   * @returns 添加响应
   */
  addUser(params: UserAddParams): Promise<ApiResponse<UserInfo>> {
    // 对密码进行MD5加密
    const encryptedParams = {
      ...params,
      password: params.password ? MD5(params.password).toString() : undefined,
    }
    return Request.post('/api/v1/auth/register', encryptedParams)
  },
  /**
   * 更新用户
   * @param userId 用户ID
   * @param params 用户信息
   * @returns 更新响应
   */
  updateUser(userId: number, params: UpdateUserParams): Promise<ApiResponse<UserInfo>> {
    return Request.put(`/api/v1/auth/users/${userId}`, params)
  },
  /**
   * 删除用户
   * @param userId 用户ID
   * @returns 删除响应
   */
  deleteUser(userId: number): Promise<ApiResponse<null>> {
    return Request.delete(`/api/v1/auth/users/${userId}`)
  },
}
