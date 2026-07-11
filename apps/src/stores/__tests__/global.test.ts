import { describe, it, expect, beforeEach } from 'vitest'
import { setActivePinia, createPinia } from 'pinia'
import { useGlobalStore } from '../global'

describe('Global Store', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
  })

  it('should initialize with default values', () => {
    const store = useGlobalStore()
    expect(store.isCollapse).toBe(false)
    expect(store.alarmNum).toBe(0)
  })

  it('should update isCollapse state', () => {
    const store = useGlobalStore()
    store.isCollapse = true
    expect(store.isCollapse).toBe(true)
  })

  it('should update alarmNum state', () => {
    const store = useGlobalStore()
    store.alarmNum = 5
    expect(store.alarmNum).toBe(5)
  })
})
