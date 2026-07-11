/**
 * WebSocket 连接管理器
 * 负责连接生命周期、订阅管理、重连恢复、监听分发。
 * 统一管理全局与页面级订阅，断线自动恢复被动断开前的订阅。
 */

import { ref, reactive, computed } from 'vue'
import { ElMessage } from 'element-plus'
import { useUserStore } from '@/stores/user'
import type {
  ConnectionStatus,
  ClientMessage,
  ServerMessage,
  SubscriptionConfig,
  ListenerConfig,
  SubscribeMessage,
  UnsubscribeMessage,
  PingMessage,
  UnsubscribeAckMessage,
} from '@/types/websocket'

/** WebSocket 连接配置 */
interface WebSocketConfig {
  url: string
  reconnectInterval?: number
  maxReconnectAttempts?: number
  heartbeatInterval?: number
  heartbeatTimeout?: number
}

/** 订阅记录 */
interface SubscriptionRecord {
  id: string // subscriptionId
  config: SubscriptionConfig
  listeners: Partial<ListenerConfig>
}

/** 待订阅信息记录，用于重连或等待 ACK */
interface PendingSubscription {
  subscriptionId: string
  config: SubscriptionConfig
  listeners: Partial<ListenerConfig>
  timestamp: number
}

class WebSocketManager {
  private ws: WebSocket | null = null
  private config: WebSocketConfig
  private reconnectAttempts = 0
  private heartbeatTimer: number | null = null
  private heartbeatTimeoutTimer: number | null = null
  private reconnectTimer: number | null = null
  private messageIdCounter = 0
  private isManualDisconnect = false
  private connectingPromise: Promise<void> | null = null // 连接 Promise 缓存，避免并发调用

  public readonly status = ref<ConnectionStatus>('disconnected')
  public readonly isConnected = computed(() => this.status.value === 'connected')
  public readonly isConnecting = computed(() => this.status.value === 'connecting')

  private subscriptions = new Map<string, SubscriptionRecord>()
  private pendingSubscriptionsMap = new Map<string, PendingSubscription>()
  private globalListeners: Partial<ListenerConfig> = {}

  public readonly connectionStats = reactive({
    connectTime: 0,
    disconnectTime: 0,
    messageCount: 0,
    errorCount: 0,
    latency: 0,
  })

  /** 构造函数，合并默认配置 */
  constructor(config: WebSocketConfig) {
    this.config = {
      reconnectInterval: 2000,
      maxReconnectAttempts: Infinity,
      heartbeatInterval: 5000,
      heartbeatTimeout: 3000,
      ...config,
    }
  }

  /** 生成唯一消息 ID */
  private generateMessageId(): string {
    return `${Date.now()}-${Math.random().toString(36).substring(2, 15)}`
  }

  /** 拷贝订阅配置，避免引用被修改 */
  private cloneConfig(config: SubscriptionConfig): SubscriptionConfig {
    return {
      ...config,
      channels: [...(config.channels || [])],
      dataTypes: config.dataTypes ? [...config.dataTypes] : undefined,
    }
  }

  /** 发送订阅消息 */
  private sendSubscribeMessage(config: SubscriptionConfig, messageId: string): void {
    let subscribeMessage: SubscribeMessage
    if (config.source === 'rule') {
      // rule 类型订阅：使用 channels 数组存储规则ID
      subscribeMessage = {
        id: messageId,
        type: 'subscribe',
        timestamp: '',
        data: {
          source: 'rule',
          channels: config.channels!,
          interval: config.interval!,
        },
      }
    } else if (config.source === 'homepage') {
      // homepage 类型订阅
      subscribeMessage = {
        id: messageId,
        type: 'subscribe',
        timestamp: '',
        data: {
          source: 'homepage',
          interval: config.interval ?? 1000,
        },
      }
    } else {
      // inst 或 comsrv 类型订阅
      subscribeMessage = {
        id: messageId,
        type: 'subscribe',
        timestamp: '',
        data: {
          channels: config.channels!,
          data_types: config.dataTypes!,
          interval: config.interval!,
          source: config.source,
        },
      }
    }
    this.sendMessage(subscribeMessage)
    console.log('[WebSocket] 发送订阅', messageId, config)
  }

  /** 发送取消订阅消息 */
  private sendUnsubscribeMessage(config: SubscriptionConfig, messageId: string): void {
    let unsubscribeMessage: UnsubscribeMessage
    if (config.source === 'rule') {
      // rule 类型取消订阅：使用 channels 数组存储规则ID
      unsubscribeMessage = {
        id: messageId,
        type: 'unsubscribe',
        timestamp: '',
        data: {
          source: 'rule',
          channels: config.channels!,
        },
      }
    } else if (config.source === 'homepage') {
      // homepage 类型取消订阅
      unsubscribeMessage = {
        id: messageId,
        type: 'unsubscribe',
        timestamp: '',
        data: {
          source: 'homepage',
        },
      }
    } else {
      // inst 或 comsrv 类型取消订阅
      unsubscribeMessage = {
        id: messageId,
        type: 'unsubscribe',
        timestamp: '',
        data: {
          channels: config.channels!,
          source: config.source,
        },
      }
    }

    this.sendMessage(unsubscribeMessage)
    console.log('[WebSocket] 发送取消订阅', messageId, config)
  }

  /** WebSocket 发送统一入口，补充时间戳 */
  private sendMessage(message: ClientMessage): void {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
      throw new Error('WebSocket is not connected')
    }
    message.timestamp = new Date().toISOString()
    this.ws.send(JSON.stringify(message))
    console.log('[WebSocket] 发送消息', message)
  }

  /** 处理待订阅队列，批量发送并记录 */
  private processPendingSubscriptions(): void {
    if (this.pendingSubscriptionsMap.size === 0) return

    console.log('[WebSocket] 处理待订阅队列:', this.pendingSubscriptionsMap.size)
    for (const [messageId, subInfo] of this.pendingSubscriptionsMap) {
      this.sendSubscribeMessage(subInfo.config, messageId)
      this.subscriptions.set(subInfo.subscriptionId, {
        id: subInfo.subscriptionId,
        config: subInfo.config,
        listeners: subInfo.listeners,
      })
    }
  }

  /** 重连前将已有订阅补回待订阅队列 */
  private enqueueExistingSubscriptionsForReconnect(): void {
    for (const [subscriptionId, record] of this.subscriptions) {
      const exists = Array.from(this.pendingSubscriptionsMap.values()).some(
        (pending) => pending.subscriptionId === subscriptionId,
      )

      if (!exists) {
        const messageId = this.generateMessageId()
        this.pendingSubscriptionsMap.set(messageId, {
          subscriptionId: record.id,
          config: this.cloneConfig(record.config),
          listeners: record.listeners,
          timestamp: Date.now(),
        })
      }
    }
  }

  /** 订阅入口 */
  public subscribe(
    config: SubscriptionConfig,
    listeners: Partial<ListenerConfig> = {},
    pageId?: string,
  ): string {
    const normalizedConfig = this.cloneConfig(config)
    let subscriptionId = ''
    if (pageId) {
      subscriptionId = pageId
    } else {
      subscriptionId = this.generateMessageId()
    }
    // 检查是否已存在相同的订阅
    for (const [subId, record] of this.subscriptions) {
      if (normalizedConfig.source === 'homepage') {
        if (
          record.config.source === 'homepage' &&
          record.config.interval === normalizedConfig.interval
        ) {
          record.listeners = { ...record.listeners, ...listeners }
          return record.id
        }
      } else if (
        record.config.source === normalizedConfig.source &&
        record.config.interval === normalizedConfig.interval &&
        JSON.stringify([...(record.config.channels || [])].sort()) ===
          JSON.stringify([...(normalizedConfig.channels || [])].sort()) &&
        (normalizedConfig.source === 'rule' ||
          JSON.stringify([...(record.config.dataTypes || [])].sort()) ===
            JSON.stringify([...(normalizedConfig.dataTypes || [])].sort()))
      ) {
        // 合并 listeners
        record.listeners = { ...record.listeners, ...listeners }
        return record.id
      }
    }

    // 检查待订阅队列中是否已存在
    for (const [messageId, subInfo] of this.pendingSubscriptionsMap) {
      if (normalizedConfig.source === 'homepage') {
        if (
          subInfo.config.source === 'homepage' &&
          subInfo.config.interval === normalizedConfig.interval
        ) {
          subInfo.listeners = { ...subInfo.listeners, ...listeners }
          return subInfo.subscriptionId
        }
      } else if (
        subInfo.config.source === normalizedConfig.source &&
        subInfo.config.interval === normalizedConfig.interval &&
        JSON.stringify([...(subInfo.config.channels || [])].sort()) ===
          JSON.stringify([...(normalizedConfig.channels || [])].sort()) &&
        (normalizedConfig.source === 'rule' ||
          JSON.stringify([...(subInfo.config.dataTypes || [])].sort()) ===
            JSON.stringify([...(normalizedConfig.dataTypes || [])].sort()))
      ) {
        // 合并 listeners
        subInfo.listeners = { ...subInfo.listeners, ...listeners }
        return subInfo.subscriptionId
      }
    }

    // 创建新订阅
    this.subscriptions.set(subscriptionId, {
      id: subscriptionId,
      config: normalizedConfig,
      listeners,
    })

    const messageId = this.generateMessageId()
    this.pendingSubscriptionsMap.set(messageId, {
      subscriptionId,
      config: normalizedConfig,
      listeners,
      timestamp: Date.now(),
    })

    if (this.isConnected.value) {
      this.sendSubscribeMessage(normalizedConfig, messageId)
    }

    return subscriptionId
  }

  /** 取消订阅 */
  public unsubscribe(subscriptionId: string): void {
    const record = this.subscriptions.get(subscriptionId)
    if (!record) {
      // 如果订阅不存在，尝试从待订阅队列中删除
      for (const [messageId, subInfo] of this.pendingSubscriptionsMap) {
        if (subInfo.subscriptionId === subscriptionId) {
          this.pendingSubscriptionsMap.delete(messageId)
        }
      }
      return
    }

    // 从订阅列表中删除
    this.subscriptions.delete(subscriptionId)

    // 从待订阅队列中删除
    for (const [messageId, subInfo] of this.pendingSubscriptionsMap) {
      if (subInfo.subscriptionId === subscriptionId) {
        this.pendingSubscriptionsMap.delete(messageId)
      }
    }

    // 如果已连接，发送取消订阅消息
    if (this.isConnected.value) {
      const messageId = this.generateMessageId()
      this.sendUnsubscribeMessage(record.config, messageId)
    }
  }

  /** 设置全局监听器 */
  public setGlobalListeners(listeners: Partial<ListenerConfig>): void {
    this.globalListeners = listeners
  }

  /** 绑定 WebSocket 事件 */
  private setupEventHandlers(onConnect: () => void, onError: (error: Error) => void): void {
    if (!this.ws) return

    this.ws.onopen = () => {
      console.log('[WebSocket] 连接成功')
      this.status.value = 'connected'
      this.reconnectAttempts = 0
      this.connectionStats.connectTime = Date.now()
      this.startHeartbeat()

      this.enqueueExistingSubscriptionsForReconnect()
      this.processPendingSubscriptions()

      // 调用全局监听器
      this.globalListeners.onConnect?.()
      onConnect()
    }

    this.ws.onmessage = (event) => {
      try {
        const message: ServerMessage = JSON.parse(event.data)
        this.handleMessage(message)
        this.connectionStats.messageCount++
      } catch (error) {
        console.error('[WebSocket] 消息解析失败:', error)
        this.connectionStats.errorCount++
      }
    }

    this.ws.onclose = (event) => {
      console.log('[WebSocket] 连接关闭:', event.code, event.reason)
      this.status.value = 'disconnected'
      this.connectionStats.disconnectTime = Date.now()
      this.stopHeartbeat()

      // 清理连接 Promise 缓存（连接失败时）
      if (this.connectingPromise) {
        this.connectingPromise = null
      }

      // 调用全局监听器
      this.globalListeners.onDisconnect?.()

      if (
        !this.isManualDisconnect &&
        this.reconnectAttempts < (this.config.maxReconnectAttempts ?? 0)
      ) {
        this.scheduleReconnect()
      }
    }

    this.ws.onerror = (error) => {
      console.error('[WebSocket] 连接出错:', error)
      this.status.value = 'error'
      this.connectionStats.errorCount++
      const wsError = new Error('WebSocket connection error')
      this.handleError(wsError)
      // 调用全局监听器，将 Error 转换为 ErrorMessage['data'] 格式
      this.globalListeners.onError?.({
        code: 'CONNECTION_ERROR',
        message: wsError.message,
        details: wsError.stack,
      })
      onError(wsError)
    }
  }

  /** 建立连接（含登录校验） */
  public connect(): Promise<void> {
    // 如果已连接，直接返回 resolved Promise
    if (this.ws && this.ws.readyState === WebSocket.OPEN) {
      return Promise.resolve()
    }

    // 如果正在连接，返回现有的连接 Promise，避免并发调用
    if (this.connectingPromise) {
      return this.connectingPromise
    }

    // 创建新的连接 Promise
    this.connectingPromise = new Promise((resolve, reject) => {
      const userStore = useUserStore()
      if (!userStore.isLoggedIn || !userStore.token) {
        console.log('[WebSocket] 未登录或无 token，跳过连接')
        this.connectingPromise = null
        reject(new Error('User not logged in or token invalid'))
        return
      }

      this.isManualDisconnect = false
      this.status.value = 'connecting'
      this.connectionStats.connectTime = Date.now()

      try {
        this.ws = new WebSocket(this.config.url)
        this.setupEventHandlers(
          () => {
            this.connectingPromise = null
            resolve()
          },
          (error) => {
            this.connectingPromise = null
            reject(error)
          },
        )
      } catch (error) {
        this.connectingPromise = null
        this.handleError(error as Error)
        reject(error)
      }
    })

    return this.connectingPromise
  }

  /** 服务端消息分发 */
  private handleMessage(message: ServerMessage): void {
    console.log('[WebSocket] 收到消息:', message)

    switch (message.type) {
      case 'connection_established':
        break
      case 'data_update':
        this.handleDataUpdate((message as any).data)
        break
      case 'data_batch':
        this.handleBatchDataUpdate((message as any).data, message.timestamp)
        break
      case 'alarm':
        this.handleAlarm((message as any).data)
        break
      case 'subscribe_ack':
        this.handleSubscribeAck((message as any).data)
        break
      case 'unsubscribe_ack':
        this.handleUnsubscribeAck((message as any).data)
        break
      case 'error':
        this.handleServerError((message as any).data)
        break
      case 'pong':
        this.handlePong((message as any).data)
        break
      case 'alarm_num':
        this.handleAlarmNum((message as any).data)
        break
      case 'homepage_batch':
        this.handleHomepageBatch((message as any).data, (message as any).timestamp)
        break
      default:
        console.warn('[WebSocket] 未知消息类型:', (message as any).type)
    }
  }

  /** 处理单条数据更新 */
  private handleDataUpdate(data: any): void {
    this.subscriptions.forEach((record) => {
      if (record.config.source === 'homepage') {
        record.listeners.onDataUpdate?.(data)
      } else if (
        record.config.source !== 'rule' &&
        record.config.channels?.includes(data.channel_id)
      ) {
        record.listeners.onDataUpdate?.(data)
      }
    })
  }

  /** 处理批量数据更新 */
  private handleBatchDataUpdate(data: any, timestamp: string): void {
    this.subscriptions.forEach((record) => {
      if (record.config.source === 'homepage') {
        // homepage 类型：直接透传数据
        record.listeners.onBatchDataUpdate?.(data, timestamp)
      } else if (record.config.source === 'rule') {
        // 处理 rule 类型的数据：检查 rule_id 是否匹配
        if (data.rule_id && record.config.channels?.includes(data.rule_id)) {
          record.listeners.onBatchDataUpdate?.(data, timestamp)
        }
      } else {
        // 处理 inst 或 comsrv 类型的数据
        if (data.updates) {
          const relevantUpdates = data.updates.filter((update: any) =>
            record.config.channels?.includes(update.channel_id),
          )
          if (relevantUpdates.length > 0) {
            record.listeners.onBatchDataUpdate?.({ updates: relevantUpdates }, timestamp)
          }
        }
      }
    })
  }

  /** 处理告警 */
  private handleAlarm(alarm: any): void {
    // 调用全局监听器
    this.globalListeners.onAlarm?.(alarm)
    // 调用订阅级别的监听器
    this.subscriptions.forEach((record) => {
      record.listeners.onAlarm?.(alarm)
    })
  }

  /** 处理订阅确认 */
  private handleSubscribeAck(data: any): void {
    console.log('[WebSocket] 订阅确认:', data)

    const requestId = data.request_id
    if (requestId) {
      this.pendingSubscriptionsMap.delete(requestId)
    }

    if (data.failed && data.failed.length > 0) {
      ElMessage.warning(`部分频道订阅失败: ${data.failed.join(', ')}`)
    }
  }

  /** 处理取消订阅确认 */
  private handleUnsubscribeAck(data: UnsubscribeAckMessage['data']): void {
    console.log('[WebSocket] 取消订阅确认:', data)

    if (data.failed && data.failed.length > 0) {
      ElMessage.warning(`部分频道取消订阅失败: ${data.failed.join(', ')}`)
    }
  }

  /** 处理 homepage_batch：首页点位批量推送 */
  private handleHomepageBatch(data: any, timestamp?: number | string): void {
    this.subscriptions.forEach((record) => {
      if (record.config.source === 'homepage') {
        record.listeners.onBatchDataUpdate?.(data, String(timestamp ?? ''))
      }
    })
  }

  /** 处理告警数量更新 */
  private handleAlarmNum(data: any): void {
    console.log('[WebSocket] 告警数量更新:', data)
    // 调用全局监听器
    this.globalListeners.onAlarmNum?.(data)
    // 调用订阅级别的监听器
    this.subscriptions.forEach((record) => {
      record.listeners.onAlarmNum?.(data)
    })
  }

  /** 处理服务端错误 */
  private handleServerError(error: any): void {
    console.error('[WebSocket] 服务端错误', error)
    // 调用全局监听器
    this.globalListeners.onError?.(error)
    // 调用订阅级别的监听器
    this.subscriptions.forEach((record) => {
      record.listeners.onError?.(error)
    })
    ElMessage.error(`WebSocket错误: ${error.message}`)
  }

  /** 处理心跳响应 */
  private handlePong(data: any): void {
    this.connectionStats.latency = data.latency
    if (this.heartbeatTimeoutTimer) {
      clearTimeout(this.heartbeatTimeoutTimer)
      this.heartbeatTimeoutTimer = null
    }
  }

  /** 发送 ping */
  public sendPing(): void {
    const pingMessage: PingMessage = {
      id: this.generateMessageId(),
      type: 'ping',
      timestamp: '',
    }

    this.sendMessage(pingMessage)
  }

  /** 启动心跳与超时检测 */
  private startHeartbeat(): void {
    if (this.heartbeatTimer) {
      clearInterval(this.heartbeatTimer)
    }

    this.heartbeatTimer = setInterval(() => {
      try {
        this.sendPing()
        console.log('[WebSocket] 发送心跳')
      } catch (err) {
        console.warn('[WebSocket] 心跳发送失败:', err)
      }

      if (this.heartbeatTimeoutTimer) {
        clearTimeout(this.heartbeatTimeoutTimer)
      }

      this.heartbeatTimeoutTimer = setTimeout(() => {
        console.warn('[WebSocket] 心跳超时，关闭等待重连')
        this.closeForReconnect('heartbeat timeout')
      }, this.config.heartbeatTimeout)
    }, this.config.heartbeatInterval)
  }

  /** 为重连关闭连接，保留订阅记录 */
  private closeForReconnect(reason: string): void {
    this.isManualDisconnect = false
    this.stopHeartbeat()
    if (this.ws) {
      this.ws.close(4000, reason)
      this.ws = null
    }
    this.status.value = 'disconnected'
  }

  /** 停止心跳与超时计时器 */
  private stopHeartbeat(): void {
    if (this.heartbeatTimer) {
      clearInterval(this.heartbeatTimer)
      this.heartbeatTimer = null
    }
    if (this.heartbeatTimeoutTimer) {
      clearTimeout(this.heartbeatTimeoutTimer)
      this.heartbeatTimeoutTimer = null
    }
  }

  /** 安排重连（指数退避可按需扩展） */
  private scheduleReconnect(): void {
    if (this.isManualDisconnect) return

    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer)
    }

    this.reconnectAttempts++
    const delay = this.config.reconnectInterval

    console.log(`[WebSocket] ${delay}ms后尝试重连（第${this.reconnectAttempts}次）`)

    this.reconnectTimer = setTimeout(() => {
      this.connect().catch((error) => {
        console.error('[WebSocket] 重连失败:', error)
      })
    }, delay)
  }

  /** 统一错误处理回调 */
  private handleError(error: Error): void {
    console.error('[WebSocket] 连接出错:', error)
  }

  /** 主动断开连接并清理所有状态 */
  public disconnect(): void {
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer)
      this.reconnectTimer = null
    }

    this.isManualDisconnect = true
    this.stopHeartbeat()

    // 清理连接 Promise 缓存
    this.connectingPromise = null

    if (this.ws) {
      this.ws.close(1000, 'manual disconnect')
      this.ws = null
    }

    this.subscriptions.clear()
    this.pendingSubscriptionsMap.clear()
    this.globalListeners = {}
    this.status.value = 'disconnected'
  }

  /** 获取当前连接统计信息 */
  public getStats() {
    return {
      status: this.status.value,
      isConnected: this.isConnected.value,
      subscriptions: this.subscriptions.size,
      ...this.connectionStats,
    }
  }
}

const wsManager = new WebSocketManager({
  url: '/ws',
})

export default wsManager
