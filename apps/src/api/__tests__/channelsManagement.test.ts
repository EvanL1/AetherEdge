import { beforeEach, describe, expect, it, vi } from 'vitest'
import {
  ChangeChannelEnabled,
  batchUpdateMappingPoint,
  controlChannelStatus,
  createChannel,
  getAllChannels,
  getChannelDetail,
  getChannelMappings,
  getChannelsByIds,
  getMappingPoints,
  getPointsTables,
  getUnmappedPoints,
  postAdjustmentBatch,
  postControlBatch,
  postPointsBatch,
  publishPointValue,
  updateChannel,
} from '../channelsManagement'

vi.mock('@/utils/request', () => ({
  Request: {
    get: vi.fn(),
    post: vi.fn(),
    put: vi.fn(),
  },
}))

describe('api/channelsManagement.ts', () => {
  beforeEach(() => {
    vi.clearAllMocks()
  })

  it('updates channel enabled state and channel detail', async () => {
    const { Request } = await import('@/utils/request')
    vi.mocked(Request.put)
      .mockResolvedValueOnce({ success: true })
      .mockResolvedValueOnce({ success: true })
    vi.mocked(Request.get).mockResolvedValueOnce({ success: true, data: { id: 1 } })

    await ChangeChannelEnabled(1, true)
    await getChannelDetail(1)
    await updateChannel(1, { name: 'CH-1' } as any)

    expect(Request.put).toHaveBeenNthCalledWith(1, '/comApi/api/channels/1/enabled', {
      enabled: true,
    })
    expect(Request.get).toHaveBeenCalledWith('/comApi/api/channels/1', null, { timeout: 60000 })
    expect(Request.put).toHaveBeenNthCalledWith(2, '/comApi/api/channels/1', { name: 'CH-1' })
  })

  it('creates channels and controls their status', async () => {
    const { Request } = await import('@/utils/request')
    vi.mocked(Request.post)
      .mockResolvedValueOnce({ success: true })
      .mockResolvedValueOnce({ success: true })

    await createChannel({ name: 'CH-2' } as any)
    await controlChannelStatus(2, 'restart')

    expect(Request.post).toHaveBeenNthCalledWith(1, '/comApi/api/channels', { name: 'CH-2' })
    expect(Request.post).toHaveBeenNthCalledWith(2, '/comApi/api/channels/2/control', {
      operation: 'restart',
    })
  })

  it('gets point tables and mapping related resources', async () => {
    const { Request } = await import('@/utils/request')
    vi.mocked(Request.get)
      .mockResolvedValueOnce({ success: true, data: { list: [] } })
      .mockResolvedValueOnce({ success: true, data: { list: [] } })
      .mockResolvedValueOnce({ success: true, data: { list: [] } })
      .mockResolvedValueOnce({ success: true, data: { list: [] } })

    await getPointsTables(3, 'T' as any, { timeout: 3000 })
    await getUnmappedPoints(3, 'S' as any)
    await getMappingPoints(3, 'C' as any, 9)
    await getChannelMappings(3)

    expect(Request.get).toHaveBeenNthCalledWith(
      1,
      '/comApi/api/channels/3/points',
      { type: 'T' },
      { timeout: 3000 },
    )
    expect(Request.get).toHaveBeenNthCalledWith(2, '/comApi/api/channels/3/unmapped-points', {
      type: 'S',
    })
    expect(Request.get).toHaveBeenNthCalledWith(3, '/comApi/api/channels/3/C/points/9/mapping')
    expect(Request.get).toHaveBeenNthCalledWith(4, '/comApi/api/channels/3/mappings', null)
  })

  it('publishes write, control batch, adjustment batch and points batch payloads', async () => {
    const { Request } = await import('@/utils/request')
    vi.mocked(Request.post)
      .mockResolvedValueOnce({ success: true })
      .mockResolvedValueOnce({ success: true })
      .mockResolvedValueOnce({ success: true })
      .mockResolvedValueOnce({ success: true })
    vi.mocked(Request.put).mockResolvedValueOnce({ success: true })

    const writePayload = { commands: [{ point_id: 1, value: 1 }] }
    const batchPayload = [{ point_id: 2, value: 0 }]
    const mappingPayload = { mappings: [{ point_id: 3, target_id: 4 }] }
    const pointsPayload = { create: [{ id: 5 }], update: [], delete: [] }

    await publishPointValue(8, writePayload as any)
    await postControlBatch(8, batchPayload)
    await postAdjustmentBatch(8, batchPayload)
    await batchUpdateMappingPoint(8, mappingPayload as any)
    await postPointsBatch(8, pointsPayload as any)

    expect(Request.post).toHaveBeenNthCalledWith(1, '/comApi/api/channels/8/write', writePayload)
    expect(Request.post).toHaveBeenNthCalledWith(2, '/comApi/api/channels/8/control/batch', {
      commands: batchPayload,
    })
    expect(Request.post).toHaveBeenNthCalledWith(3, '/comApi/api/channels/8/adjustment/batch', {
      commands: batchPayload,
    })
    expect(Request.put).toHaveBeenCalledWith('/comApi/api/channels/8/mappings', mappingPayload)
    expect(Request.post).toHaveBeenNthCalledWith(
      4,
      '/comApi/api/channels/8/points/batch',
      pointsPayload,
    )
  })

  it('gets all channels and short-circuits empty id searches', async () => {
    const { Request } = await import('@/utils/request')
    vi.mocked(Request.get).mockResolvedValueOnce({ success: true, data: { list: [] } })

    await getAllChannels()
    const emptyResult = await getChannelsByIds([])

    expect(Request.get).toHaveBeenCalledWith('/comApi/api/channels/list')
    expect(emptyResult).toEqual({ success: true, data: { list: [] } })
  })

  it('searches channels by ids when ids are provided', async () => {
    const mockData = { success: true, data: { list: [{ id: 6 }] } }
    const { Request } = await import('@/utils/request')
    vi.mocked(Request.get).mockResolvedValue(mockData)

    const result = await getChannelsByIds([6, 7], { timeout: 5000 })

    expect(result).toEqual(mockData)
    expect(Request.get).toHaveBeenCalledWith(
      '/comApi/api/channels/search',
      { ids: '6,7' },
      { timeout: 5000 },
    )
  })
})
