/**
 * Axios 请求封装
 * 提供统一的HTTP请求配置、拦截器和错误处理
 */

import axios, { type AxiosRequestConfig } from 'axios'
import { ElMessage } from 'element-plus'
import { ref } from 'vue'
import { useUserStore } from '@/stores/user'
import { useGlobalStore } from '@/stores/global'

// 存储所有pending的请求（使用 ref 包装 Map 以确保响应式）
const pendingRequests = ref(new Map<string, any>())

// 更新全局 loading 状态的函数
const updateGlobalLoading = () => {
  try {
    const globalStore = useGlobalStore()
    globalStore.loading = pendingRequests.value.size > 0
  } catch (error) {
    // Pinia 未初始化时忽略错误
    console.warn('Pinia store not available:', error)
  }
}

// 规范化用于生成请求唯一标识的负载（排除 _t，稳定排序）
const normalizeForKey = (value: any): any => {
  if (value === null || value === undefined) return ''
  if (value instanceof Date) return value.toISOString()
  if (Array.isArray(value)) return value.map((v) => normalizeForKey(v))
  if (typeof value === 'object') {
    // 处理 FormData
    if (typeof FormData !== 'undefined' && value instanceof FormData) {
      const obj: Record<string, any> = {}
      ;(value as FormData).forEach((v, k) => {
        // 文件对象不参与去重，只记录占位，避免巨大的序列化
        obj[k] = typeof v === 'string' ? v : '[binary]'
      })
      return normalizeForKey(obj)
    }
    // 普通对象：去除 _t 并按 key 排序
    const sorted: Record<string, any> = {}
    Object.keys(value)
      .filter((k) => k !== '_t')
      .sort()
      .forEach((k) => {
        sorted[k] = normalizeForKey(value[k])
      })
    return sorted
  }
  return value
}

// 根据 axios config 生成稳定的请求标识，包含 method、url、参数/数据（排除 _t）
const buildRequestKey = (config: any): string => {
  const method = (config?.method || 'get').toLowerCase()
  const url = config?.url || ''
  // GET/DELETE 使用 params，其它使用 data
  const source = method === 'get' || method === 'delete' ? config?.params : config?.data
  const normalized = normalizeForKey(source)
  const payload =
    normalized === ''
      ? ''
      : typeof normalized === 'string'
        ? normalized
        : JSON.stringify(normalized)
  return `${method}-${url}-${payload}`
}

// 定义响应数据的通用接口
export interface ApiResponse<T = any> {
  code: number // 响应状态码
  message: string // 响应消息
  data: T // 响应数据
  success: boolean // 请求是否成功
  [key: string]: any
}

// 定义请求配置的扩展接口
export interface RequestConfig extends AxiosRequestConfig {
  showErrorMessage?: boolean // 是否显示错误消息，默认true
  showSuccessMessage?: boolean // 是否显示成功消息，默认false
  skipGlobalLoading?: boolean // 是否跳过全局loading，默认false
  _isRefreshTokenRequest?: boolean // 是否为刷新token请求（内部使用，避免重复刷新）
}

/**
 * 创建统一的axios实例
 * 设置基础配置项
 */
const service = axios.create({
  // API的base_url，可以通过环境变量配置
  baseURL: '',
  // 请求超时时间 (毫秒)
  timeout: 30000,
  // 默认请求头
  headers: {
    'Content-Type': 'application/json;charset=UTF-8',
  },
  // 跨域请求时是否需要使用凭证
  withCredentials: false,
})
/**
 * 请求拦截器
 * 在发送请求之前做一些统一处理
 */
const requestInterceptor = (config: any) => {
  // 添加时间戳防止缓存 (GET请求) - 在生成 key 之前添加，但 normalizeForKey 会过滤掉 _t
  if (
    config.method?.toLowerCase() === 'get' &&
    config.url !== '/modApi/api/instances/search' &&
    config.url !== '/comApi/api/channels/search'
  ) {
    config.params = {
      ...config.params,
      _t: Date.now(),
    }
  }

  // 生成请求的唯一标识（_t 会被 normalizeForKey 过滤掉，所以 key 是稳定的）
  const requestKey = buildRequestKey(config)

  // 检查是否跳过全局loading
  const requestConfig = config as any
  const skipGlobalLoading = requestConfig.skipGlobalLoading === true

  // 如果存在相同的pending请求，取消它（但需要检查是否跳过全局loading）
  if (!skipGlobalLoading && pendingRequests.value.has(requestKey)) {
    const cancelToken = pendingRequests.value.get(requestKey)
    cancelToken.cancel('请求被取消')
    pendingRequests.value.delete(requestKey)
    updateGlobalLoading()
  }

  // 创建新的cancelToken
  const cancelToken = axios.CancelToken.source()
  config.cancelToken = cancelToken.token
  // 将 requestKey 保存到 config 中，以便在错误处理时能够清除
  config._requestKey = requestKey

  // 如果配置了 skipGlobalLoading，则不添加到 pendingRequests
  if (!skipGlobalLoading) {
    pendingRequests.value.set(requestKey, cancelToken)
    updateGlobalLoading()
  }

  // 从内存中获取token并添加到请求头（token存储在内存中，refreshToken存储在localStorage）
  const userStore = useUserStore()
  const token = userStore.token
  if (token && config.headers) {
    config.headers.Authorization = `Bearer ${token}`
  }

  // 如果是POST/PUT/PATCH请求且没有设置Content-Type，设置为json
  if (['post', 'put', 'patch'].includes(config.method?.toLowerCase() || '')) {
    if (!config.headers['Content-Type']) {
      config.headers['Content-Type'] = 'application/json'
    }
  }

  console.log(`[请求] ${config.method?.toUpperCase()} ${config.url}`, config)
  return config
}

const requestErrorInterceptor = (error: any) => {
  console.error('[请求错误]', error)
  // 如果错误对象有 config，尝试清除 pendingRequests
  const requestErrorConfig = error.config as any
  if (error.config?._requestKey && !requestErrorConfig?.skipGlobalLoading) {
    const requestKey = error.config._requestKey
    if (pendingRequests.value.has(requestKey)) {
      pendingRequests.value.delete(requestKey)
      updateGlobalLoading()
    }
  }
  ElMessage.error('Request configuration error')
  return Promise.reject(error)
}

service.interceptors.request.use(requestInterceptor, requestErrorInterceptor)

// 用于防止重复刷新token的标志
let isRefreshing = false
// 存储等待token刷新的请求队列
let failedQueue: Array<{
  resolve: (value: any) => void
  reject: (error: any) => void
}> = []

// 处理队列中的请求
const processQueue = (error: any, token: string | null = null) => {
  failedQueue.forEach(({ resolve, reject }) => {
    if (error) {
      reject(error)
    } else {
      resolve(token)
    }
  })

  failedQueue = []
}

/**
 * 响应拦截器工厂函数
 * 创建统一的响应拦截器，避免代码重复
 */
const createResponseInterceptor = (serviceInstance: any, logPrefix: string = '') => {
  // 响应成功时的处理
  const responseSuccessHandler = (response: any) => {
    // 请求完成后，从pendingRequests中移除
    // 优先使用 config._requestKey，如果没有则使用 buildRequestKey 生成
    const requestKey = response.config?._requestKey || buildRequestKey(response.config)
    const responseConfig = response.config as any
    // 如果配置了 skipGlobalLoading，则不需要从 pendingRequests 中移除
    if (!responseConfig?.skipGlobalLoading && requestKey && pendingRequests.value.has(requestKey)) {
      pendingRequests.value.delete(requestKey)
      updateGlobalLoading()
    }

    console.log(
      `${logPrefix}[响应] ${response.config.method?.toUpperCase()} ${response.config.url}`,
      response,
    )

    const { data } = response
    const customConfig = response.config as any
    console.log(data)

    // 检查业务状态码
    // 如果返回的是blob类型，直接认为请求成功，否则判断业务code或success
    if (
      response.request?.responseType === 'blob' ||
      (data && (data.code === 200 || data.success || data.status === 'success'))
    ) {
      // 请求成功
      if (customConfig.showSuccessMessage && data.message) {
        ElMessage.success(data.message)
      }
      return response
    } else {
      // 业务逻辑错误
      let errorMessage = data.message || 'Request failed'

      // 根据不同的业务状态码进行处理
      switch (data.code) {
        case 401:
          // 未授权，清除token并跳转到登录页
          const userStore = useUserStore()
          userStore.clearUserData()
          errorMessage = 'Login expired, please log in again'
          // 跳转到登录页
          window.location.href = '/login'
          break
        case 403:
          errorMessage = 'No permission to access this resource'
          break
        case 404:
          errorMessage = 'Requested resource not found'
          break
        case 500:
          errorMessage = 'Internal server error'
          break
        default:
          break
      }
      ElMessage.error(response.data.message || errorMessage)
      return Promise.reject(new Error(errorMessage))
    }
  }

  // 响应失败时的处理
  const responseErrorHandler = async (error: any) => {
    // 如果是取消请求的错误，尝试清除 pendingRequests
    if (axios.isCancel(error)) {
      console.log(`${logPrefix}[请求已取消] ${error.message}`)
      // 尝试从 error.config 中获取 requestKey（Cancel 错误可能没有 config）
      const cancelError = error as any
      if (cancelError.config) {
        const requestKey = cancelError.config._requestKey || buildRequestKey(cancelError.config)
        const cancelConfig = cancelError.config as any
        // 如果配置了 skipGlobalLoading，则不需要从 pendingRequests 中移除
        if (
          !cancelConfig?.skipGlobalLoading &&
          requestKey &&
          pendingRequests.value.has(requestKey)
        ) {
          pendingRequests.value.delete(requestKey)
          updateGlobalLoading()
        }
      }
      return Promise.reject(error)
    }

    // 请求完成后，从pendingRequests中移除
    // 优先使用 config._requestKey，如果没有则使用 buildRequestKey 生成
    const requestKey =
      error.config?._requestKey || (error.config ? buildRequestKey(error.config) : null)
    const errorConfig = error.config as any
    // 如果配置了 skipGlobalLoading，则不需要从 pendingRequests 中移除
    if (!errorConfig?.skipGlobalLoading && requestKey && pendingRequests.value.has(requestKey)) {
      pendingRequests.value.delete(requestKey)
      updateGlobalLoading()
    }

    console.error(`${logPrefix}[响应错误]`, error)

    const originalRequest = error.config
    const requestConfig = originalRequest as any

    // 如果是刷新token请求返回401，直接跳转登录页，不再尝试刷新
    if (error.response?.status === 401 && requestConfig?._isRefreshTokenRequest) {
      console.log('[请求拦截器] 刷新token请求返回401，直接跳转登录页')
      const userStore = useUserStore()
      userStore.clearUserData()

      // 处理队列中的请求（如果有）
      if (isRefreshing) {
        processQueue(new Error('Token refresh failed'), null)
        isRefreshing = false
      }

      ElMessage.error('Login expired, please log in again')
      window.location.href = '/login'
      return Promise.reject(new Error('Token refresh request returned 401'))
    }

    // 处理401错误 - 自动刷新token
    if (error.response?.status === 401 && !originalRequest._retry) {
      if (isRefreshing) {
        // 如果正在刷新token，将请求加入队列
        return new Promise((resolve, reject) => {
          failedQueue.push({ resolve, reject })
        })
          .then((token) => {
            originalRequest.headers['Authorization'] = `Bearer ${token}`
            return serviceInstance(originalRequest)
          })
          .catch((err) => {
            return Promise.reject(err)
          })
      }

      originalRequest._retry = true
      isRefreshing = true
      try {
        const userStore = useUserStore()

        // 调用 userStore 的 refreshUserToken 方法
        const result = await userStore.refreshUserToken()

        if (result.success && userStore.token) {
          // 更新当前请求的Authorization头
          originalRequest.headers['Authorization'] = `Bearer ${userStore.token}`

          // 处理队列中的请求
          processQueue(null, userStore.token)

          // 重试原请求
          return serviceInstance(originalRequest)
        } else {
          throw new Error(result.message || 'Token refresh failed')
        }
      } catch (refreshError) {
        // 刷新token失败，清除用户数据并跳转到登录页
        const userStore = useUserStore()
        userStore.clearUserData()

        // 处理队列中的请求
        processQueue(refreshError, null)
        ElMessage.error('Login expired, please log in again')
        window.location.href = '/login'
        return Promise.reject(refreshError)
      } finally {
        isRefreshing = false
      }
    }

    // 处理其他错误
    let errorMessage = 'Network request failed'

    if (error.response) {
      // 服务器返回错误状态码
      const { status, statusText } = error.response

      switch (status) {
        case 400:
          errorMessage = 'Bad request'
          break
        case 403:
          errorMessage = 'Access denied'
          break
        case 404:
          errorMessage = 'Request URL not found'
          break
        case 408:
          errorMessage = 'Request timeout'
          break
        case 500:
          errorMessage = 'Internal server error'
          break
        case 502:
          errorMessage = 'Bad gateway'
          break
        case 503:
          errorMessage = 'Service unavailable'
          break
        case 504:
          errorMessage = 'Gateway timeout'
          break
        default:
          errorMessage = `Connection error ${status}: ${statusText}`
      }
    } else if (error.request) {
      // 请求已发出但没有收到响应
      if (error.code === 'ECONNABORTED') {
        errorMessage = 'Request timeout'
      } else {
        errorMessage = 'Network connection error'
      }
    } else {
      // 请求配置出错
      errorMessage = error.message || 'Request configuration error'
    }
    ElMessage.error(error.response?.data?.message || errorMessage)
    return Promise.reject(error)
  }

  return [responseSuccessHandler, responseErrorHandler]
}

// 为服务实例添加响应拦截器
service.interceptors.response.use(...createResponseInterceptor(service))

/**
 * 封装的请求方法类
 */
class Request {
  /**
   * GET请求
   * @param url 请求地址（已包含完整路径，如 /api/v1/users 或 /alarmApi/alarms）
   * @param params 请求参数
   * @param config 请求配置
   */
  static async get<T = any>(
    url: string,
    params?: any,
    config?: RequestConfig,
  ): Promise<ApiResponse<T>> {
    const response = await service.get(url, { params, ...config })
    return response.data
  }

  /**
   * POST请求
   * @param url 请求地址（已包含完整路径，如 /api/v1/users 或 /alarmApi/alarms）
   * @param data 请求数据
   * @param config 请求配置
   */
  static async post<T = any>(
    url: string,
    data?: any,
    config?: RequestConfig,
  ): Promise<ApiResponse<T>> {
    const response = await service.post(url, data, config)
    return response.data
  }

  /**
   * PUT请求
   * @param url 请求地址（已包含完整路径，如 /api/v1/users 或 /alarmApi/alarms）
   * @param data 请求数据
   * @param config 请求配置
   */
  static async put<T = any>(
    url: string,
    data?: any,
    config?: RequestConfig,
  ): Promise<ApiResponse<T>> {
    const response = await service.put(url, data, config)
    return response.data
  }

  /**
   * DELETE请求
   * @param url 请求地址（已包含完整路径，如 /api/v1/users 或 /alarmApi/alarms）
   * @param config 请求配置
   */
  static async delete<T = any>(url: string, config?: RequestConfig): Promise<ApiResponse<T>> {
    const response = await service.delete(url, config)
    return response.data
  }

  /**
   * PATCH请求
   * @param url 请求地址（已包含完整路径，如 /api/v1/users 或 /alarmApi/alarms）
   * @param data 请求数据
   * @param config 请求配置
   */
  static async patch<T = any>(
    url: string,
    data?: any,
    config?: RequestConfig,
  ): Promise<ApiResponse<T>> {
    const response = await service.patch(url, data, config)
    return response.data
  }

  /**
   * 文件上传
   * @param url 上传地址（已包含完整路径，如 /api/v1/upload 或 /alarmApi/upload）
   * @param file 文件对象
   * @param data 额外数据
   * @param config 请求配置
   */
  static async upload<T = any>(
    url: string,
    file: File,
    data?: any,
    config?: RequestConfig,
  ): Promise<ApiResponse<T>> {
    const formData = new FormData()
    formData.append('file', file)

    // 添加额外数据
    if (data) {
      Object.keys(data).forEach((key) => {
        formData.append(key, data[key])
      })
    }

    const response = await service.post(url, formData, {
      ...config,
      headers: {
        'Content-Type': 'multipart/form-data',
      },
    })
    return response.data
  }

  /**
   * 下载文件
   * @param url 下载地址（已包含完整路径，如 /api/v1/export 或 /alarmApi/export）
   * @param params 请求参数
   * @param filename 文件名
   */
  static async download(url: string, params?: any, filename?: string): Promise<void> {
    try {
      const response = await service.get(url, {
        params,
        responseType: 'blob',
      })
      console.log('response', response)
      // 创建blob链接
      const blob = new Blob([response.data])
      const downloadUrl = window.URL.createObjectURL(blob)
      console.log('downloadUrl', downloadUrl)
      // 创建下载链接
      const link = document.createElement('a')
      link.href = downloadUrl
      link.download = filename || `download_${Date.now()}`

      // 触发下载
      document.body.appendChild(link)
      link.click()

      // 清理
      document.body.removeChild(link)
      window.URL.revokeObjectURL(downloadUrl)

      ElMessage.success('File downloaded successfully')
    } catch (error) {
      console.error('File download failed:', error)
      ElMessage.error('File download failed')
    }
  }
}

// 取消所有pending请求的方法
export const cancelAllPendingRequests = () => {
  pendingRequests.value.forEach((cancelToken: any) => {
    cancelToken.cancel('路由切换，请求被取消')
  })
  pendingRequests.value.clear()
  updateGlobalLoading()
}

// 导出服务实例和Request类
export { service, Request }
export default Request
