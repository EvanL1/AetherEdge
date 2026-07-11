import { beforeEach, describe, expect, it, vi } from 'vitest'

type GuardFn = (
  to: { path: string; [key: string]: unknown },
  from: { path: string; [key: string]: unknown },
  next: (value?: unknown) => void,
) => unknown | Promise<unknown>

type MockUserStore = {
  token: string
  userInfo: Record<string, unknown> | null
  refreshToken: string
  routesInjected: boolean
  refreshUserToken: ReturnType<typeof vi.fn>
  getUserInfo: ReturnType<typeof vi.fn>
  clearUserData: ReturnType<typeof vi.fn>
}

const createUserStore = (overrides: Partial<MockUserStore> = {}): MockUserStore => ({
  token: '',
  userInfo: null,
  refreshToken: '',
  routesInjected: false,
  refreshUserToken: vi.fn(),
  getUserInfo: vi.fn(),
  clearUserData: vi.fn(),
  ...overrides,
})

const loadGuard = async (userStore: MockUserStore) => {
  vi.resetModules()

  let guard: GuardFn | undefined
  const beforeEachSpy = vi.fn((callback: GuardFn) => {
    guard = callback
  })
  const ensureRoutesInjected = vi.fn()
  const cancelAllPendingRequests = vi.fn()

  vi.doMock('../index', () => ({
    router: {
      beforeEach: beforeEachSpy,
    },
  }))

  vi.doMock('@/stores/user', () => ({
    useUserStore: () => userStore,
  }))

  vi.doMock('../injector', () => ({
    ensureRoutesInjected,
  }))

  vi.doMock('@/utils/request', () => ({
    cancelAllPendingRequests,
  }))

  await import('../guard')

  if (!guard) {
    throw new Error('Route guard was not registered')
  }

  return {
    guard,
    beforeEachSpy,
    ensureRoutesInjected,
    cancelAllPendingRequests,
  }
}

describe('router/guard.ts', () => {
  beforeEach(() => {
    vi.clearAllMocks()
  })

  it('allows whitelisted routes without auth checks', async () => {
    const userStore = createUserStore()
    const { guard, cancelAllPendingRequests, ensureRoutesInjected } = await loadGuard(userStore)
    const next = vi.fn()

    await guard({ path: '/login' }, { path: '/' }, next)

    expect(cancelAllPendingRequests).toHaveBeenCalledOnce()
    expect(ensureRoutesInjected).not.toHaveBeenCalled()
    expect(userStore.refreshUserToken).not.toHaveBeenCalled()
    expect(next).toHaveBeenCalledWith()
  })

  it('redirects to login when token and refresh token are missing', async () => {
    const userStore = createUserStore()
    const { guard } = await loadGuard(userStore)
    const next = vi.fn()

    await guard({ path: '/home' }, { path: '/login' }, next)

    expect(next).toHaveBeenCalledWith({ path: '/login' })
    expect(userStore.refreshUserToken).not.toHaveBeenCalled()
    expect(userStore.clearUserData).not.toHaveBeenCalled()
  })

  it('refreshes token and reinjects routes when session can be restored', async () => {
    const userStore = createUserStore({
      refreshToken: 'refresh-token',
      refreshUserToken: vi.fn().mockResolvedValue({ success: true }),
      getUserInfo: vi.fn().mockResolvedValue({ success: true }),
    })
    const { guard, ensureRoutesInjected } = await loadGuard(userStore)
    const next = vi.fn()
    const to = { path: '/dashboard', query: { tab: 'overview' } }

    await guard(to, { path: '/login' }, next)

    expect(userStore.refreshUserToken).toHaveBeenCalledOnce()
    expect(userStore.getUserInfo).toHaveBeenCalledOnce()
    expect(ensureRoutesInjected).toHaveBeenCalledOnce()
    expect(next).toHaveBeenCalledWith({ ...to, replace: true })
  })

  it('clears user state when token refresh fails', async () => {
    const userStore = createUserStore({
      refreshToken: 'refresh-token',
      refreshUserToken: vi.fn().mockResolvedValue({ success: false }),
    })
    const { guard } = await loadGuard(userStore)
    const next = vi.fn()

    await guard({ path: '/dashboard' }, { path: '/login' }, next)

    expect(userStore.refreshUserToken).toHaveBeenCalledOnce()
    expect(userStore.getUserInfo).not.toHaveBeenCalled()
    expect(userStore.clearUserData).toHaveBeenCalledOnce()
    expect(next).toHaveBeenCalledWith({ path: '/login' })
  })

  it('clears user state when fetching user info after refresh fails', async () => {
    const userStore = createUserStore({
      refreshToken: 'refresh-token',
      refreshUserToken: vi.fn().mockResolvedValue({ success: true }),
      getUserInfo: vi.fn().mockResolvedValue({ success: false }),
    })
    const { guard, ensureRoutesInjected } = await loadGuard(userStore)
    const next = vi.fn()

    await guard({ path: '/dashboard' }, { path: '/login' }, next)

    expect(userStore.refreshUserToken).toHaveBeenCalledOnce()
    expect(userStore.getUserInfo).toHaveBeenCalledOnce()
    expect(ensureRoutesInjected).not.toHaveBeenCalled()
    expect(userStore.clearUserData).toHaveBeenCalledOnce()
    expect(next).toHaveBeenCalledWith({ path: '/login' })
  })

  it('continues navigation after routes were already injected', async () => {
    const userStore = createUserStore({
      token: 'access-token',
      userInfo: { id: 1 },
      routesInjected: true,
    })
    const { guard, ensureRoutesInjected } = await loadGuard(userStore)
    const next = vi.fn()

    await guard({ path: '/dashboard' }, { path: '/login' }, next)

    expect(ensureRoutesInjected).not.toHaveBeenCalled()
    expect(next).toHaveBeenCalledWith()
  })

  it('falls back to login when guard execution throws', async () => {
    const userStore = createUserStore({
      token: 'access-token',
      userInfo: { id: 1 },
      routesInjected: false,
    })
    const consoleErrorSpy = vi.spyOn(console, 'error').mockImplementation(() => {})
    const { guard, ensureRoutesInjected } = await loadGuard(userStore)
    ensureRoutesInjected.mockRejectedValueOnce(new Error('inject failed'))
    const next = vi.fn()

    await guard({ path: '/dashboard' }, { path: '/login' }, next)

    expect(ensureRoutesInjected).toHaveBeenCalledOnce()
    expect(userStore.clearUserData).toHaveBeenCalledOnce()
    expect(next).toHaveBeenCalledWith({ path: '/login' })
    expect(consoleErrorSpy).toHaveBeenCalled()

    consoleErrorSpy.mockRestore()
  })
})
