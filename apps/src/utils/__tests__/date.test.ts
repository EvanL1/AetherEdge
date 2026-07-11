import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest'
import { getRecentHoursRange, getRecentDaysRange, getRecentWeekRange } from '../date'

describe('date utilities', () => {
  const FIXED_NOW = new Date('2024-01-15T12:00:00.000Z')

  beforeEach(() => {
    vi.useFakeTimers()
    vi.setSystemTime(FIXED_NOW)
  })

  afterEach(() => {
    vi.useRealTimers()
  })

  describe('getRecentHoursRange', () => {
    it('returns ISO strings for start and end', () => {
      const range = getRecentHoursRange(6)
      expect(range.start).toBeDefined()
      expect(range.end).toBeDefined()
      expect(typeof range.start).toBe('string')
      expect(typeof range.end).toBe('string')
    })

    it('end is current time', () => {
      const range = getRecentHoursRange(6)
      expect(new Date(range.end!).getTime()).toBeCloseTo(FIXED_NOW.getTime(), -3)
    })

    it('start is N hours before end', () => {
      const range = getRecentHoursRange(6)
      const diff = new Date(range.end!).getTime() - new Date(range.start!).getTime()
      expect(diff).toBe(6 * 60 * 60 * 1000)
    })

    it('handles 1 hour', () => {
      const range = getRecentHoursRange(1)
      const diff = new Date(range.end!).getTime() - new Date(range.start!).getTime()
      expect(diff).toBe(1 * 60 * 60 * 1000)
    })
  })

  describe('getRecentDaysRange', () => {
    it('start is N days before end', () => {
      const range = getRecentDaysRange(7)
      const diff = new Date(range.end!).getTime() - new Date(range.start!).getTime()
      expect(diff).toBe(7 * 24 * 60 * 60 * 1000)
    })

    it('handles 30 days', () => {
      const range = getRecentDaysRange(30)
      const diff = new Date(range.end!).getTime() - new Date(range.start!).getTime()
      expect(diff).toBe(30 * 24 * 60 * 60 * 1000)
    })
  })

  describe('getRecentWeekRange', () => {
    it('is equivalent to getRecentDaysRange(7)', () => {
      const week = getRecentWeekRange()
      const days7 = getRecentDaysRange(7)
      expect(week.start).toBe(days7.start)
      expect(week.end).toBe(days7.end)
    })
  })
})
