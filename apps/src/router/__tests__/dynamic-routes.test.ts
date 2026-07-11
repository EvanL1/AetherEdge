import { describe, it, expect } from 'vitest'
import { dynamicRoutes } from '../dynamic-routes'

describe('Dynamic Routes', () => {
  it('should have correct route structure', () => {
    expect(Array.isArray(dynamicRoutes)).toBe(true)
    expect(dynamicRoutes.length).toBeGreaterThan(0)
  })

  it('should contain home route', () => {
    const homeRoute = dynamicRoutes.find((route) => route.path === '/home')
    expect(homeRoute).toBeDefined()
    expect(homeRoute?.name).toBe('home')
    expect(homeRoute?.meta?.title).toBe('Home')
  })

  it('should contain devices route with children', () => {
    const devicesRoute = dynamicRoutes.find((route) => route.path === '/devices')
    expect(devicesRoute).toBeDefined()
    expect(devicesRoute?.meta?.isSubMenu).toBe(true)
    expect(devicesRoute?.children).toBeDefined()
    expect(devicesRoute?.children?.length).toBeGreaterThan(0)
  })

  it('should contain alarm route', () => {
    const alarmRoute = dynamicRoutes.find((route) => route.path === '/alarm')
    expect(alarmRoute).toBeDefined()
    expect(alarmRoute?.meta?.title).toBe('Alarm')
    expect(alarmRoute?.children).toBeDefined()
  })

  it('should contain control route', () => {
    const controlRoute = dynamicRoutes.find((route) => route.path === '/control')
    expect(controlRoute).toBeDefined()
    expect(controlRoute?.meta?.title).toBe('Control')
  })

  it('should contain statistics route', () => {
    const statisticsRoute = dynamicRoutes.find((route) => route.path === '/statistics')
    expect(statisticsRoute).toBeDefined()
    expect(statisticsRoute?.meta?.title).toBe('Statistics')
  })

  it('should contain setting route', () => {
    const settingRoute = dynamicRoutes.find((route) => route.path === '/setting')
    expect(settingRoute).toBeDefined()
    expect(settingRoute?.meta?.title).toBe('Setting')
  })

  it('should have correct roles for routes', () => {
    const homeRoute = dynamicRoutes.find((route) => route.path === '/home')
    expect(homeRoute?.meta?.roles).toEqual(['Admin', 'Viewer', 'Engineer'])
  })
})
