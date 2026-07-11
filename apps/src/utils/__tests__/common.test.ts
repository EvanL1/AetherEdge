import { describe, it, expect } from 'vitest'
import { formatNumber, formatTimestamp } from '../common'

describe('formatNumber', () => {
  it('returns "-" for null', () => {
    expect(formatNumber(null)).toBe('-')
  })

  it('returns "-" for undefined', () => {
    expect(formatNumber(undefined)).toBe('-')
  })

  it('returns integer as-is', () => {
    expect(formatNumber(42)).toBe('42')
  })

  it('keeps short decimals unchanged (≤3 digits)', () => {
    expect(formatNumber(3.14)).toBe('3.14')
    expect(formatNumber(1.5)).toBe('1.5')
    expect(formatNumber(0.123)).toBe('0.123')
  })

  it('rounds to 3 decimal places when >3 digits', () => {
    expect(formatNumber(3.14159)).toBe('3.142')
    expect(formatNumber(1.23456)).toBe('1.235')
  })

  it('parses numeric strings correctly', () => {
    expect(formatNumber('3.14159')).toBe('3.142')
    expect(formatNumber('42')).toBe('42')
  })

  it('returns original string for non-numeric strings', () => {
    expect(formatNumber('abc')).toBe('abc')
  })

  it('handles zero', () => {
    expect(formatNumber(0)).toBe('0')
  })

  it('handles negative numbers', () => {
    expect(formatNumber(-3.14159)).toBe('-3.142')
  })
})

describe('formatTimestamp', () => {
  it('returns "-" for null', () => {
    expect(formatTimestamp(null)).toBe('-')
  })

  it('returns "-" for undefined', () => {
    expect(formatTimestamp(undefined)).toBe('-')
  })

  it('returns "-" for 0', () => {
    expect(formatTimestamp(0)).toBe('-')
  })

  it('returns "-" for negative timestamp', () => {
    expect(formatTimestamp(-1)).toBe('-')
  })

  it('returns "-" for NaN string', () => {
    expect(formatTimestamp('not-a-number')).toBe('-')
  })

  it('returns a non-empty string for a valid timestamp', () => {
    const result = formatTimestamp(1700000000000)
    expect(typeof result).toBe('string')
    expect(result).not.toBe('-')
    expect(result.length).toBeGreaterThan(0)
  })

  it('handles string timestamp', () => {
    const result = formatTimestamp('1700000000000')
    expect(typeof result).toBe('string')
    expect(result).not.toBe('-')
  })
})
