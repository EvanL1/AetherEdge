import { describe, it, expect, vi } from 'vitest'
import { userManagementApi } from '../userManagement'

vi.mock('@/utils/request', () => ({
  Request: {
    get: vi.fn(),
    post: vi.fn(),
    put: vi.fn(),
    delete: vi.fn(),
  },
}))

describe('User Management API', () => {
  it('should get user list', async () => {
    const mockData = { users: [], total: 0 }
    const { Request } = await import('@/utils/request')
    vi.mocked(Request.get).mockResolvedValue(mockData)

    const params = { page: 1, pageSize: 10 }
    const result = await userManagementApi.getUserList(params)
    expect(result).toEqual(mockData)
    expect(Request.get).toHaveBeenCalledWith('/api/v1/user/list', params)
  })

  it('should add user', async () => {
    const mockData = { success: true, user: { id: 1 } }
    const { Request } = await import('@/utils/request')
    vi.mocked(Request.post).mockResolvedValue(mockData)

    const userData = { name: 'new user', email: 'new@example.com' }
    const result = await userManagementApi.addUser(userData)
    expect(result).toEqual(mockData)
    expect(Request.post).toHaveBeenCalledWith('/api/v1/user/add', userData)
  })

  it('should update user', async () => {
    const mockData = { success: true }
    const { Request } = await import('@/utils/request')
    vi.mocked(Request.put).mockResolvedValue(mockData)

    const userData = { id: 1, name: 'updated user' }
    const result = await userManagementApi.updateUser(userData)
    expect(result).toEqual(mockData)
    expect(Request.put).toHaveBeenCalledWith('/api/v1/user/update', userData)
  })

  it('should delete user', async () => {
    const mockData = { success: true }
    const { Request } = await import('@/utils/request')
    vi.mocked(Request.delete).mockResolvedValue(mockData)

    const params = { id: 1 }
    const result = await userManagementApi.deleteUser(params)
    expect(result).toEqual(mockData)
    expect(Request.delete).toHaveBeenCalledWith('/api/v1/user/delete', { params })
  })
})
