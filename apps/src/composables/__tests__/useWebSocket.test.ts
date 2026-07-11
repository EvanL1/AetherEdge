import { defineComponent, h } from 'vue'
import { mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import useWebSocket from '../useWebSocket'
import type { SubscriptionConfig } from '@/types/websocket'

const {
  mockStatus,
  mockIsConnected,
  mockIsConnecting,
  subscribeMock,
  unsubscribeMock,
  connectMock,
  getStatsMock,
} = vi.hoisted(() => ({
  mockStatus: { value: 'disconnected' as 'connecting' | 'connected' | 'disconnected' | 'error' },
  mockIsConnected: {
    get value() {
      return this.__status.value === 'connected'
    },
    __status: undefined as unknown as { value: string },
  },
  mockIsConnecting: {
    get value() {
      return this.__status.value === 'connecting'
    },
    __status: undefined as unknown as { value: string },
  },
  subscribeMock: vi.fn(),
  unsubscribeMock: vi.fn(),
  connectMock: vi.fn(),
  getStatsMock: vi.fn(),
}))
mockIsConnected.__status = mockStatus
mockIsConnecting.__status = mockStatus

vi.mock('@/utils/websocket', () => ({
  default: {
    status: mockStatus,
    isConnected: mockIsConnected,
    isConnecting: mockIsConnecting,
    subscribe: subscribeMock,
    unsubscribe: unsubscribeMock,
    connect: connectMock,
    getStats: getStatsMock,
  },
}))

const createHost = (
  config: SubscriptionConfig,
  listeners: Record<string, unknown> = {},
  onSetup?: (api: ReturnType<typeof useWebSocket>) => void,
) =>
  defineComponent({
    setup() {
      const api = useWebSocket(config, listeners)
      onSetup?.(api)
      return () => h('div')
    },
  })

describe('composables/useWebSocket.ts', () => {
  beforeEach(() => {
    mockStatus.value = 'disconnected'
    subscribeMock.mockReset()
    unsubscribeMock.mockReset()
    connectMock.mockReset()
    getStatsMock.mockReset()

    subscribeMock.mockReturnValue('sub-1')
    connectMock.mockResolvedValue(undefined)
    getStatsMock.mockReturnValue({
      status: 'disconnected',
      isConnected: false,
      subscriptions: 0,
    })
  })

  it('subscribes on mount and connects when socket is idle', async () => {
    const config: SubscriptionConfig = {
      source: 'inst',
      channels: [101],
      dataTypes: ['T'],
      interval: 1000,
    }
    const listeners = { onDataUpdate: vi.fn() }
    let api: ReturnType<typeof useWebSocket> | undefined

    const wrapper = mount(
      createHost(config, listeners, (exposed) => {
        api = exposed
      }),
    )

    await Promise.resolve()

    expect(subscribeMock).toHaveBeenCalledWith(config, listeners)
    expect(connectMock).toHaveBeenCalledOnce()
    expect(api?.subscriptionId.value).toBe('sub-1')
    expect(api?.status.value).toBe('disconnected')
    expect(api?.isConnected.value).toBe(false)
    expect(api?.stats.value).toEqual({
      status: 'disconnected',
      isConnected: false,
      subscriptions: 0,
    })

    wrapper.unmount()
  })

  it('does not reconnect when websocket is already connected', () => {
    mockStatus.value = 'connected'
    const config: SubscriptionConfig = { source: 'homepage', interval: 2000 }

    const wrapper = mount(createHost(config))

    expect(subscribeMock).toHaveBeenCalledWith(config, {})
    expect(connectMock).not.toHaveBeenCalled()

    wrapper.unmount()
  })

  it('does not reconnect when websocket is already connecting', () => {
    mockStatus.value = 'connecting'
    const config: SubscriptionConfig = {
      source: 'rule',
      channels: [88],
      interval: 500,
    }

    const wrapper = mount(createHost(config))

    expect(subscribeMock).toHaveBeenCalledWith(config, {})
    expect(connectMock).not.toHaveBeenCalled()

    wrapper.unmount()
  })

  it('unsubscribes the current subscription on unmount', () => {
    const config: SubscriptionConfig = {
      source: 'inst',
      channels: [9],
      dataTypes: ['S'],
      interval: 500,
    }

    const wrapper = mount(createHost(config))
    wrapper.unmount()

    expect(unsubscribeMock).toHaveBeenCalledWith('sub-1')
  })

  it('supports manual subscribe and unsubscribe flows', () => {
    const baseConfig: SubscriptionConfig = {
      source: 'inst',
      channels: [1],
      dataTypes: ['T'],
      interval: 1000,
    }
    const customConfig: SubscriptionConfig = {
      source: 'comsrv',
      channels: [2],
      dataTypes: ['C'],
      interval: 2000,
    }
    const customListeners = { onAlarm: vi.fn() }
    let api: ReturnType<typeof useWebSocket> | undefined

    const wrapper = mount(
      createHost(baseConfig, {}, (exposed) => {
        api = exposed
      }),
    )

    subscribeMock.mockReturnValueOnce('sub-2')

    const subscriptionId = api!.subscribe(customConfig, customListeners)
    api!.unsubscribe(subscriptionId)

    expect(subscriptionId).toBe('sub-2')
    expect(subscribeMock).toHaveBeenLastCalledWith(customConfig, customListeners)
    expect(unsubscribeMock).toHaveBeenCalledWith('sub-2')
    expect(api!.subscriptionId.value).toBe('')

    wrapper.unmount()
  })

})
