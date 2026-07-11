import { describe, it, expect, vi } from 'vitest'
import {
  setRem,
  initResponsive,
  pxToResponsive,
  pxToRem,
  getCurrentScale,
  getCurrentFontSize,
} from '../responsive'

// Mock window and document
Object.defineProperty(window, 'innerWidth', {
  writable: true,
  configurable: true,
  value: 1920,
})

Object.defineProperty(document, 'documentElement', {
  writable: true,
  configurable: true,
  value: {
    style: {
      fontSize: '100px',
    },
  },
})

describe('Responsive Utils', () => {
  describe('setRem', () => {
    it('should set root font size based on screen width', () => {
      const mockHtml = {
        style: {
          fontSize: '',
        },
      }

      Object.defineProperty(document, 'documentElement', {
        value: mockHtml,
        writable: true,
      })

      setRem()
      expect(mockHtml.style.fontSize).toBe('100px')
    })
  })

  describe('pxToResponsive', () => {
    it('should convert design px to responsive px', () => {
      const result = pxToResponsive(100)
      expect(result).toBe(100) // At 1920px width, scale is 1
    })
  })

  describe('pxToRem', () => {
    it('should convert design px to rem', () => {
      const result = pxToRem(100)
      expect(result).toBe('1rem')
    })
  })

  describe('getCurrentScale', () => {
    it('should return current scale ratio', () => {
      const scale = getCurrentScale()
      expect(scale).toBe(1) // At 1920px width, scale is 1
    })
  })

  describe('getCurrentFontSize', () => {
    it('should return current font size', () => {
      // Mock getComputedStyle to return a valid CSSStyleDeclaration
      const mockComputedStyle = {
        fontSize: '16px',
      }

      vi.spyOn(window, 'getComputedStyle').mockReturnValue(mockComputedStyle as any)

      const fontSize = getCurrentFontSize()
      expect(fontSize).toBe(16)
    })
  })

  describe('initResponsive', () => {
    it('should initialize responsive configuration', () => {
      const addEventListenerSpy = vi.spyOn(window, 'addEventListener')
      initResponsive()
      expect(addEventListenerSpy).toHaveBeenCalledWith('resize', expect.any(Function))
    })
  })
})
