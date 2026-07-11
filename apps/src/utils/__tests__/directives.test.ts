import { describe, it, expect, vi } from 'vitest'
import { permissionDirective } from '../directives'

vi.mock('@/stores/user', () => ({
  useUserStore: vi.fn(() => ({
    roles: ['Admin'],
  })),
}))

describe('Directives', () => {
  describe('permissionDirective', () => {
    it('should be defined', () => {
      expect(permissionDirective).toBeDefined()
      expect(permissionDirective.mounted).toBeDefined()
    })

    it('should remove element when user has no permission', () => {
      const mockElement = {
        parentNode: {
          removeChild: vi.fn(),
        },
      }

      const mockBinding = {
        value: ['Editor'], // User is Admin but needs Editor role
      }

      permissionDirective.mounted(mockElement as any, mockBinding as any)

      expect(mockElement.parentNode.removeChild).toHaveBeenCalledWith(mockElement)
    })

    it('should keep element when user has permission', () => {
      const mockElement = {
        parentNode: {
          removeChild: vi.fn(),
        },
      }

      const mockBinding = {
        value: ['Admin'], // User is Admin and has Admin role
      }

      permissionDirective.mounted(mockElement as any, mockBinding as any)

      expect(mockElement.parentNode.removeChild).not.toHaveBeenCalled()
    })
  })
})
