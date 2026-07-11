import { describe, it, expect } from 'vitest'
import { staticRoutes } from '../static-routes'

describe('Static Routes', () => {
  it('should have correct route structure', () => {
    expect(Array.isArray(staticRoutes)).toBe(true)
    expect(staticRoutes).toHaveLength(2)
  })

  it('should contain login route', () => {
    const loginRoute = staticRoutes.find((route) => route.path === '/login')
    expect(loginRoute).toBeDefined()
    expect(loginRoute?.name).toBe('login')
    expect(loginRoute?.meta?.activeNav).toBe('/login')
    expect(loginRoute?.meta?.roles).toEqual(['Admin', 'Viewer', 'Engineer'])
  })

  it('should contain main route', () => {
    const mainRoute = staticRoutes.find((route) => route.path === '/')
    expect(mainRoute).toBeDefined()
    expect(mainRoute?.name).toBe('main')
    expect(mainRoute?.meta?.activeNav).toBe('/home')
    expect(mainRoute?.meta?.roles).toEqual(['Admin', 'Viewer', 'Engineer'])
    expect(mainRoute?.children).toEqual([])
  })

  it('should have correct component imports', () => {
    const loginRoute = staticRoutes.find((route) => route.path === '/login')
    const mainRoute = staticRoutes.find((route) => route.path === '/')

    expect(loginRoute?.component).toBeDefined()
    expect(mainRoute?.component).toBeDefined()
  })
})
