import { beforeEach, describe, expect, it, vi } from 'vitest'
import {
  createInstance,
  executeAction,
  executeMeasurement,
  getAllInstances,
  getInstanceDetail,
  getInstanceMappings,
  getInstancePoints,
  getInstancesByIds,
  getProducts,
  updateInstance,
  updateInstanceMappings,
  updateInstanceRouting,
} from '../devicesManagement'

vi.mock('@/utils/request', () => ({
  Request: {
    get: vi.fn(),
    post: vi.fn(),
    put: vi.fn(),
  },
}))

describe('api/devicesManagement.ts', () => {
  beforeEach(() => {
    vi.clearAllMocks()
  })

  it('gets instance detail', async () => {
    const mockData = { success: true, data: { instance_id: 7 } }
    const { Request } = await import('@/utils/request')
    vi.mocked(Request.get).mockResolvedValue(mockData)

    const result = await getInstanceDetail(7)

    expect(result).toEqual(mockData)
    expect(Request.get).toHaveBeenCalledWith('/modApi/api/instances/7')
  })

  it('gets products and all instances', async () => {
    const { Request } = await import('@/utils/request')
    vi.mocked(Request.get)
      .mockResolvedValueOnce({ success: true, data: { list: [{ product_id: 1 }] } })
      .mockResolvedValueOnce({ success: true, data: { list: [{ instance_id: 2 }] } })

    await expect(getProducts()).resolves.toEqual({
      success: true,
      data: { list: [{ product_id: 1 }] },
    })
    await expect(getAllInstances()).resolves.toEqual({
      success: true,
      data: { list: [{ instance_id: 2 }] },
    })

    expect(Request.get).toHaveBeenNthCalledWith(1, '/modApi/api/products')
    expect(Request.get).toHaveBeenNthCalledWith(2, '/modApi/api/instances/list')
  })

  it('creates and updates an instance', async () => {
    const { Request } = await import('@/utils/request')
    vi.mocked(Request.post).mockResolvedValue({ success: true, data: { instance_id: 1 } })
    vi.mocked(Request.put).mockResolvedValue({ success: true })

    const createPayload = { name: 'PCS-1', product_id: 9 }
    const updatePayload = { instance_id: 1, name: 'PCS-1A' }

    await createInstance(createPayload as any)
    await updateInstance(updatePayload as any)

    expect(Request.post).toHaveBeenCalledWith('/modApi/api/instances', createPayload)
    expect(Request.put).toHaveBeenCalledWith('/modApi/api/instances/1', updatePayload)
  })

  it('gets instance points and mappings', async () => {
    const { Request } = await import('@/utils/request')
    vi.mocked(Request.get)
      .mockResolvedValueOnce({ success: true, data: { list: [] } })
      .mockResolvedValueOnce({ success: true, data: { mappings: [] } })

    await getInstancePoints(3)
    await getInstanceMappings(3)

    expect(Request.get).toHaveBeenNthCalledWith(1, '/modApi/api/instances/3/points')
    expect(Request.get).toHaveBeenNthCalledWith(2, '/modApi/api/instances/3/routing')
  })

  it('executes action, measurement and mapping updates', async () => {
    const { Request } = await import('@/utils/request')
    vi.mocked(Request.post)
      .mockResolvedValueOnce({ success: true })
      .mockResolvedValueOnce({ success: true })
    vi.mocked(Request.put)
      .mockResolvedValueOnce({ success: true })
      .mockResolvedValueOnce({ success: true })

    const actionPayload = { point_id: '42', value: 'OPEN' }
    const measurementPayload = { point_id: '8', value: 12.5 }
    const mappingsPayload = { mappings: [{ point_id: 1, channel_id: 2 }] }
    const routingPayload = [
      { channel_id: 1, channel_point_id: 2, four_remote: '遥控', point_id: 3 },
    ]

    await executeAction(5, actionPayload)
    await executeMeasurement(5, measurementPayload)
    await updateInstanceMappings(5, mappingsPayload)
    await updateInstanceRouting(5, routingPayload)

    expect(Request.post).toHaveBeenNthCalledWith(1, '/modApi/api/instances/5/action', actionPayload)
    expect(Request.post).toHaveBeenNthCalledWith(
      2,
      '/modApi/api/instances/5/measurement',
      measurementPayload,
    )
    expect(Request.put).toHaveBeenNthCalledWith(
      1,
      '/modApi/api/instances/5/mappings',
      mappingsPayload,
    )
    expect(Request.put).toHaveBeenNthCalledWith(
      2,
      '/ruleApi/api/instances/5/routing',
      routingPayload,
    )
  })

  it('returns an empty list without making a request when instance ids are empty', async () => {
    const { Request } = await import('@/utils/request')

    const result = await getInstancesByIds([])

    expect(result).toEqual({ success: true, data: { list: [] } })
    expect(Request.get).not.toHaveBeenCalled()
  })

  it('searches instances by ids when ids are provided', async () => {
    const mockData = { success: true, data: { list: [{ instance_id: 1 }] } }
    const { Request } = await import('@/utils/request')
    vi.mocked(Request.get).mockResolvedValue(mockData)

    const result = await getInstancesByIds([1, 2, 3])

    expect(result).toEqual(mockData)
    expect(Request.get).toHaveBeenCalledWith('/modApi/api/instances/search', { ids: '1,2,3' })
  })
})
