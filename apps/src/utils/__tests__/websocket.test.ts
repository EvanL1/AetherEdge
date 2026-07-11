import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { ListenerConfig, SubscriptionConfig } from '@/types/websocket'

type UserState = {
  isLoggedIn: boolean
  token: string
}

class MockWebSocket {
  static CONNECTING = 0
  static OPEN = 1
  static instances: MockWebSocket[] = []

  public url: string
  public readyState = MockWebSocket.CONNECTING
  public send = vi.fn()
  public close = vi.fn((code?: number, reason?: string) => {
    this.readyState = 3
    this.onclose?.({ code: code ?? 1000, reason: reason ?? '' } as CloseEvent)
  })
  public onopen: ((event: Event) => void) | null = null
  public onmessage: ((event: MessageEvent) => void) | null = null
  public onclose: ((event: CloseEvent) => void) | null = null
  public onerror: ((event: Event) => void) | null = null

  constructor(url: string) {
    this.url = url
    MockWebSocket.instances.push(this)
  }
}

const loadManager = async (userState: UserState = { isLoggedIn: true, token: 'token-1' }) => {
  vi.resetModules()
  MockWebSocket.instances = []
  vi.stubGlobal('WebSocket', MockWebSocket as unknown as typeof WebSocket)
  ;(globalThis.WebSocket as any).OPEN = MockWebSocket.OPEN
  ;(globalThis.WebSocket as any).CONNECTING = MockWebSocket.CONNECTING

  const warning = vi.fn()
  const error = vi.fn()

  vi.doMock('@/stores/user', () => ({
    useUserStore: () => userState,
  }))

  vi.doMock('element-plus', () => ({
    ElMessage: {
      warning,
      error,
      success: vi.fn(),
    },
  }))

  const module = await import('../websocket')

  return {
    wsManager: module.default,
    messageMocks: {
      warning,
      error,
    },
  }
}

describe('utils/websocket.ts', () => {
  beforeEach(() => {
    vi.useFakeTimers()
    vi.spyOn(console, 'log').mockImplementation(() => {})
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    vi.spyOn(console, 'error').mockImplementation(() => {})
  })

  afterEach(() => {
    vi.runOnlyPendingTimers()
    vi.useRealTimers()
    vi.unstubAllGlobals()
    vi.clearAllMocks()
  })

  it('rejects connect when user is not logged in', async () => {
    const { wsManager } = await loadManager({ isLoggedIn: false, token: '' })

    await expect(wsManager.connect()).rejects.toThrow('User not logged in or token invalid')
    expect(MockWebSocket.instances).toHaveLength(0)
    expect(wsManager.status.value).toBe('disconnected')
  }, 15000)

  it('creates a websocket connection and flushes pending subscriptions on open', async () => {
    const { wsManager } = await loadManager()
    const listeners: Partial<ListenerConfig> = {
      onDataUpdate: vi.fn(),
    }
    const config: SubscriptionConfig = {
      source: 'inst',
      channels: [101],
      dataTypes: ['T'],
      interval: 1000,
    }

    const subscriptionId = wsManager.subscribe(config, listeners)
    const connectPromise = wsManager.connect()
    const socket = MockWebSocket.instances[0]
    socket.readyState = MockWebSocket.OPEN
    socket.onopen?.({} as Event)
    await connectPromise

    expect(socket.url).toBe('/ws')
    expect(wsManager.status.value).toBe('connected')
    expect(wsManager.getStats().subscriptions).toBe(1)
    expect(subscriptionId).toBeTruthy()
    expect(socket.send).toHaveBeenCalledTimes(1)

    const subscribePayload = JSON.parse(socket.send.mock.calls[0][0])
    expect(subscribePayload.type).toBe('subscribe')
    expect(subscribePayload.data).toEqual({
      channels: [101],
      data_types: ['T'],
      interval: 1000,
      source: 'inst',
    })

    wsManager.disconnect()
  })

  it('reuses the same subscription id for duplicate subscriptions', async () => {
    const { wsManager } = await loadManager()
    const config: SubscriptionConfig = {
      source: 'homepage',
      interval: 2000,
    }

    const firstId = wsManager.subscribe(config, { onAlarm: vi.fn() })
    const secondId = wsManager.subscribe(config, { onDataUpdate: vi.fn() })

    expect(secondId).toBe(firstId)
    expect(wsManager.getStats().subscriptions).toBe(1)

    const connectPromise = wsManager.connect()
    const socket = MockWebSocket.instances[0]
    socket.readyState = MockWebSocket.OPEN
    socket.onopen?.({} as Event)
    await connectPromise

    expect(socket.send).toHaveBeenCalledTimes(1)
    const subscribePayload = JSON.parse(socket.send.mock.calls[0][0])
    expect(subscribePayload.type).toBe('subscribe')
    expect(subscribePayload.data).toEqual({
      source: 'homepage',
      interval: 2000,
    })

    wsManager.disconnect()
  })

  it('dispatches batch, alarm, error and alarm count messages to matching listeners', async () => {
    const { wsManager, messageMocks } = await loadManager()
    const onBatchDataUpdate = vi.fn()
    const onAlarm = vi.fn()
    const onError = vi.fn()
    const onAlarmNum = vi.fn()

    wsManager.setGlobalListeners({ onAlarm, onError, onAlarmNum })
    wsManager.subscribe(
      { source: 'rule', channels: [7], interval: 1000 },
      { onBatchDataUpdate, onAlarm, onError, onAlarmNum },
    )

    const connectPromise = wsManager.connect()
    const socket = MockWebSocket.instances[0]
    socket.readyState = MockWebSocket.OPEN
    socket.onopen?.({} as Event)
    await connectPromise

    socket.onmessage?.({
      data: JSON.stringify({
        id: 'msg-2',
        type: 'data_batch',
        timestamp: '2026-04-14T00:00:00.000Z',
        data: { rule_id: 7, execution_path: [{ id: 'start' }] },
      }),
    } as MessageEvent)
    socket.onmessage?.({
      data: JSON.stringify({
        id: 'msg-3',
        type: 'alarm',
        timestamp: '2026-04-14T00:00:01.000Z',
        data: { alarm_id: 'a-1', message: 'overheat' },
      }),
    } as MessageEvent)
    socket.onmessage?.({
      data: JSON.stringify({
        id: 'msg-4',
        type: 'error',
        timestamp: '2026-04-14T00:00:02.000Z',
        data: { code: 'E_1', message: 'server error' },
      }),
    } as MessageEvent)
    socket.onmessage?.({
      data: JSON.stringify({
        id: 'msg-5',
        type: 'alarm_num',
        timestamp: '2026-04-14T00:00:03.000Z',
        data: { current_alarms: 3 },
      }),
    } as MessageEvent)

    expect(onBatchDataUpdate).toHaveBeenCalledWith(
      { rule_id: 7, execution_path: [{ id: 'start' }] },
      '2026-04-14T00:00:00.000Z',
    )
    expect(onAlarm).toHaveBeenCalledWith({ alarm_id: 'a-1', message: 'overheat' })
    expect(onError).toHaveBeenCalledWith({ code: 'E_1', message: 'server error' })
    expect(onAlarmNum).toHaveBeenCalledWith({ current_alarms: 3 })
    expect(messageMocks.error).toHaveBeenCalledWith('WebSocket错误: server error')

    wsManager.disconnect()
  })

  it('sends unsubscribe messages for active subscriptions and removes unknown pending ids silently', async () => {
    const { wsManager } = await loadManager()
    const config: SubscriptionConfig = {
      source: 'comsrv',
      channels: [22],
      dataTypes: ['C'],
      interval: 500,
    }

    const subscriptionId = wsManager.subscribe(config)
    wsManager.unsubscribe('unknown-id')

    const connectPromise = wsManager.connect()
    const socket = MockWebSocket.instances[0]
    socket.readyState = MockWebSocket.OPEN
    socket.onopen?.({} as Event)
    await connectPromise

    socket.send.mockClear()
    wsManager.unsubscribe(subscriptionId)

    expect(socket.send).toHaveBeenCalledTimes(1)
    const unsubscribePayload = JSON.parse(socket.send.mock.calls[0][0])
    expect(unsubscribePayload.type).toBe('unsubscribe')
    expect(unsubscribePayload.data).toEqual({
      channels: [22],
      source: 'comsrv',
    })
    expect(wsManager.getStats().subscriptions).toBe(0)

    wsManager.disconnect()
  })

  it('clears runtime state on disconnect', async () => {
    const { wsManager } = await loadManager()

    wsManager.subscribe({
      source: 'homepage',
      interval: 1000,
    })

    const connectPromise = wsManager.connect()
    const socket = MockWebSocket.instances[0]
    socket.readyState = MockWebSocket.OPEN
    socket.onopen?.({} as Event)
    await connectPromise

    wsManager.disconnect()

    expect(socket.close).toHaveBeenCalledWith(1000, 'manual disconnect')
    expect(wsManager.status.value).toBe('disconnected')
    expect(wsManager.getStats().subscriptions).toBe(0)
  })
})
