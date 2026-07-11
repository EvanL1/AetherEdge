import { describe, it, expect } from 'vitest'
import { router } from '../index'

describe('Router', () => {
  it('should create router instance', () => {
    expect(router).toBeDefined()
    expect(router.getRoutes).toBeDefined()
  })

  it('should have correct base configuration', () => {
    expect(router.options.history).toBeDefined()
  })

  it('should have static routes', () => {
    const routes = router.getRoutes()
    expect(routes.length).toBeGreaterThan(0)
  })

  it('should have login route', () => {
    const routes = router.getRoutes()
    const loginRoute = routes.find((route) => route.path === '/login')
    expect(loginRoute).toBeDefined()
  })

  it('should have main route', () => {
    const routes = router.getRoutes()
    const mainRoute = routes.find((route) => route.path === '/')
    expect(mainRoute).toBeDefined()
  })
})
