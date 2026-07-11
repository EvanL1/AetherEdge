import { describe, expect, it, vi } from 'vitest'
import { queryPowerTrend } from '../overview'

vi.mock('@/utils/request', () => {
  const Request = {
    get: vi.fn(),
  }
  return {
    default: Request,
    Request,
  }
})

describe('api/Statistic/overview.ts', () => {
  it('queries power trend with the provided params', async () => {
    const mockData = { success: true, data: { series: [] } }
    const RequestModule = await import('@/utils/request')
    vi.mocked(RequestModule.default.get).mockResolvedValue(mockData)

    const params = {
      start_time: '2026-04-14 00:00:00',
      end_time: '2026-04-14 23:59:59',
      point_ids: [1, 2],
    }

    const result = await queryPowerTrend(params as any)

    expect(result).toEqual(mockData)
    expect(RequestModule.default.get).toHaveBeenCalledWith('/hisApi/data/query', params)
  })
})
