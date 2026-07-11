import { describe, expect, it, vi } from 'vitest'
import { getHomepagePoints } from '../homepage'

vi.mock('@/utils/request', () => ({
  Request: {
    get: vi.fn(),
  },
}))

describe('api/homepage.ts', () => {
  it('gets homepage points with the default limit', async () => {
    const mockData = { success: true, data: { items: [], total: 0 } }
    const { Request } = await import('@/utils/request')
    vi.mocked(Request.get).mockResolvedValue(mockData)

    const result = await getHomepagePoints()

    expect(result).toEqual(mockData)
    expect(Request.get).toHaveBeenCalledWith('/api/v1/homepage', { limit: 100 })
  })

  it('gets homepage points with a custom limit', async () => {
    const mockData = { success: true, data: { items: [{ id: 1 }], total: 1 } }
    const { Request } = await import('@/utils/request')
    vi.mocked(Request.get).mockResolvedValue(mockData)

    const result = await getHomepagePoints(20)

    expect(result).toEqual(mockData)
    expect(Request.get).toHaveBeenCalledWith('/api/v1/homepage', { limit: 20 })
  })
})
