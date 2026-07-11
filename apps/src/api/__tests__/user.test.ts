import { describe, it, expect, vi } from 'vitest'
import { userApi } from '../user'

vi.mock('@/utils/request', () => ({
  Request: {
    get: vi.fn(),
    post: vi.fn(),
    put: vi.fn(),
    delete: vi.fn(),
  },
}))

vi.mock('crypto-js/md5', () => ({
  default: vi.fn((str) => `md5_${str}`),
}))

describe('User API', () => {
  it('should login user', async () => {
    const mockData = {
      success: true,
      data: { access_token: 'test-token', refresh_token: 'refresh-token' },
    }
    const { Request } = await import('@/utils/request')
    vi.mocked(Request.post).mockResolvedValue(mockData)

    const credentials = { username: 'test', password: 'password' }
    const result = await userApi.login(credentials)
    expect(result).toEqual(mockData)
    expect(Request.post).toHaveBeenCalledWith('/api/v1/auth/login', {
      username: 'test',
      password: expect.any(String),
    })
  })

  it('should logout user', async () => {
    const mockData = { success: true }
    const { Request } = await import('@/utils/request')
    vi.mocked(Request.post).mockResolvedValue(mockData)

    const result = await userApi.logout('refresh-token')
    expect(result).toEqual(mockData)
    expect(Request.post).toHaveBeenCalledWith('/api/v1/auth/logout', {
      refresh_token: 'refresh-token',
    })
  })

  it('should get user info', async () => {
    const mockData = { success: true, data: { id: 1, username: 'test', email: 'test@example.com' } }
    const { Request } = await import('@/utils/request')
    vi.mocked(Request.get).mockResolvedValue(mockData)

    const result = await userApi.getUserInfo()
    expect(result).toEqual(mockData)
    expect(Request.get).toHaveBeenCalledWith('/api/v1/auth/me')
  })

  it('should add user', async () => {
    const mockData = { success: true, data: { id: 1, username: 'newuser' } }
    const { Request } = await import('@/utils/request')
    vi.mocked(Request.post).mockResolvedValue(mockData)

    const userData = { username: 'newuser', password: 'password', email: 'new@example.com' }
    const result = await userApi.addUser(userData)
    expect(result).toEqual(mockData)
    expect(Request.post).toHaveBeenCalledWith('/api/v1/auth/register', {
      username: 'newuser',
      password: expect.any(String),
      email: 'new@example.com',
    })
  })

  it('should update user', async () => {
    const mockData = { success: true, data: { id: 1, username: 'updated' } }
    const { Request } = await import('@/utils/request')
    vi.mocked(Request.put).mockResolvedValue(mockData)

    const userData = { username: 'updated' }
    const result = await userApi.updateUser(1, userData)
    expect(result).toEqual(mockData)
    expect(Request.put).toHaveBeenCalledWith('/api/v1/auth/users/1', userData)
  })

  it('should delete user', async () => {
    const mockData = { success: true }
    const { Request } = await import('@/utils/request')
    vi.mocked(Request.delete).mockResolvedValue(mockData)

    const result = await userApi.deleteUser(1)
    expect(result).toEqual(mockData)
    expect(Request.delete).toHaveBeenCalledWith('/api/v1/auth/users/1')
  })
})
