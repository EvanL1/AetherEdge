import { defineStore } from 'pinia'
import type { UserInfo, LoginParams } from '@/types/user'
import { userApi } from '@/api/user'
import wsManager from '@/utils/websocket'
import MD5 from 'crypto-js/md5'
// 用户状态管理
export const useUserStore = defineStore(
  'user',
  () => {
    // 状态
    const token = ref<string>('')
    const refreshToken = ref<string>('')
    const userInfo = ref<UserInfo | null>(null)
    // routesInjected 不参与持久化，仅用于本地内存
    const routesInjected = ref<boolean>(false) // 是否已注入动态路由

    // 计算属性
    const isLoggedIn = computed(() => !!token.value && !!userInfo.value)
    const displayName = computed(() => userInfo.value?.username || '')

    const roles = computed(() =>
      userInfo.value?.role.name_en ? [userInfo.value.role.name_en] : ['Admin'],
    )

    // 用户登录
    const login = async (params: LoginParams) => {
      try {
        // 对密码进行MD5加密
        const encryptedParams = {
          ...params,
          password: MD5(params.password).toString(),
        }

        const response = await userApi.login(encryptedParams)

        if (response.success) {
          // 更新状态（会自动持久化）
          token.value = response.data.access_token
          refreshToken.value = response.data.refresh_token
          ElMessage.success(response.message || 'Login successful')
          return { success: true, message: response.message || 'Login successful' }
        } else {
          return { success: false, message: response.message || 'Login failed' }
        }
      } catch (error: any) {
        return { success: false, message: error.message || 'Login failed' }
      }
    }

    // 用户登出
    const logout = async () => {
      try {
        // 调用登出API
        if (token.value) {
          const res = await userApi.logout(refreshToken.value)
          if (res.success) {
            ElMessage.success('Logout successful')
            clearUserData()
          } else {
            ElMessage.error(res.message || 'Logout failed')
          }
        }
      } catch (error) {
        console.error('Logout API call failed:', error)
      }
    }

    // 获取用户信息
    const getUserInfo = async () => {
      try {
        const response = await userApi.getUserInfo()

        if (response.success) {
          userInfo.value = response.data

          return { success: true, message: response.message || 'Get user info successful' }
        } else {
          return { success: false, message: response.message || 'Get user info failed' }
        }
      } catch (error: any) {
        return { success: false, message: error.message || 'Get user info failed' }
      }
    }

    // 刷新用户Token
    const refreshUserToken = async () => {
      try {
        if (!refreshToken.value) {
          return { success: false, message: 'No refresh token available' }
        }

        const response = await userApi.refreshToken(refreshToken.value)

        if (response.success && response.data) {
          token.value = response.data.access_token || ''
          refreshToken.value = response.data.refresh_token || ''
          return { success: true, message: response.message || 'Token refreshed successfully' }
        } else {
          return { success: false, message: response.message || 'Token refresh failed' }
        }
      } catch (error: any) {
        return { success: false, message: error.message || 'Token refresh failed' }
      }
    }

    // 清除用户数据（手动清除持久化数据）
    const clearUserData = () => {
      // // 断开WebSocket连接
      // wsManager.disconnect()

      token.value = ''
      userInfo.value = null
      routesInjected.value = false // 清除时也重置
      refreshToken.value = ''
    }

    return {
      // 状态
      token,
      refreshToken,
      userInfo,
      routesInjected,

      // 计算属性
      isLoggedIn,
      displayName,
      roles,

      // 方法
      login,
      logout,
      getUserInfo,
      clearUserData,
      refreshUserToken,
    }
  },
  {
    // 只持久化 refreshToken，token 和 userInfo 不持久化（存储在内存中）
    persist: {
      key: 'user',
      storage: localStorage,
      pick: ['refreshToken'],
    },
  },
)
