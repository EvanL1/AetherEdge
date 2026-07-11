/**
 * WebSocket 连接管理 Composable
 * 提供连接状态、统一订阅/退订操作，封装订阅的挂载/卸载时机。
 */

import { computed, onMounted, onUnmounted, ref } from 'vue'
import wsManager from '@/utils/websocket'
import type { ListenerConfig, SubscriptionConfig } from '@/types/websocket'

/**
 * 创建 WebSocket 订阅上下文
 * @param config 订阅配置
 * @param listeners 监听配置（可选，支持部分监听器）
 */
export default function useWebSocket(
  config: SubscriptionConfig,
  listeners: Partial<ListenerConfig> = {},
) {
  const status = computed(() => wsManager.status.value)
  const isConnected = computed(() => wsManager.isConnected.value)
  const isConnecting = computed(() => wsManager.isConnecting.value)

  const stats = computed(() => wsManager.getStats())

  const subscriptionId = ref<string>('')

  /** 订阅 */
  const subscribe = (
    customConfig?: SubscriptionConfig,
    customListeners?: Partial<ListenerConfig>,
  ) => {
    const finalConfig = customConfig || config
    const finalListeners = customListeners || listeners
    const id = wsManager.subscribe(finalConfig, finalListeners)
    subscriptionId.value = id
    return id
  }

  /** 取消订阅 */
  const unsubscribe = (customSubscriptionId?: string) => {
    const id = customSubscriptionId || subscriptionId.value
    if (id) {
      wsManager.unsubscribe(id)
      if (id === subscriptionId.value) {
        subscriptionId.value = ''
      }
    }
  }

  onMounted(() => {
    // 先记录订阅，再建立连接，方便断线重连时自动恢复
    subscribe(config, listeners)
    // 如果已连接或正在连接，则不需要重复调用 connect
    if (!wsManager.isConnected.value && !wsManager.isConnecting.value) {
      wsManager.connect().catch(() => {
        // 重连机制在 wsManager 内部处理
      })
    }
  })

  onUnmounted(() => {
    // 取消订阅
    unsubscribe()
  })

  return {
    status,
    isConnected,
    isConnecting,
    stats,
    subscriptionId,
    subscribe,
    unsubscribe,
  }
}
