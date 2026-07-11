import { defineComponent, h } from 'vue'
import { mount } from '@vue/test-utils'
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { useTableData } from '../useTableData'

vi.mock('@/utils/request', () => ({
  Request: {
    get: vi.fn(),
    post: vi.fn(),
    delete: vi.fn(),
    download: vi.fn(),
  },
}))

vi.mock('element-plus', () => ({
  ElMessage: {
    success: vi.fn(),
    error: vi.fn(),
    warning: vi.fn(),
  },
  ElMessageBox: {
    confirm: vi.fn(),
  },
}))

describe('useTableData', () => {
  const mockConfig = {
    listUrl: '/test/list',
    defaultPageSize: 20,
    deleteUrl: '/test/delete/{id}',
  }

  const mountComposable = (onSetup?: (api: ReturnType<typeof useTableData>) => void) => {
    const Host = defineComponent({
      setup() {
        const api = useTableData(mockConfig)
        onSetup?.(api)
        return () => h('div')
      },
    })

    return mount(Host)
  }

  beforeEach(() => {
    vi.clearAllMocks()
  })

  it('should initialize with default values', async () => {
    const { Request } = await import('@/utils/request')
    vi.mocked(Request.get).mockResolvedValue({
      success: true,
      data: { list: [], total: 0 },
    })

    let api!: ReturnType<typeof useTableData>
    const wrapper = mountComposable((exposed) => {
      api = exposed
    })
    await Promise.resolve()

    expect(api.loading.value).toBe(false)
    expect(api.tableData.value).toEqual([])
    expect(api.pagination.page).toBe(1)
    expect(api.pagination.pageSize).toBe(20)
    expect(api.pagination.total).toBe(0)

    wrapper.unmount()
  })

  it('should fetch table data successfully', async () => {
    const mockResponse = {
      success: true,
      data: {
        list: [{ id: 1, name: 'test' }],
        total: 1,
      },
    }

    const { Request } = await import('@/utils/request')
    vi.mocked(Request.get).mockResolvedValue(mockResponse)

    let api!: ReturnType<typeof useTableData>
    const wrapper = mountComposable((exposed) => {
      api = exposed
    })
    await api.fetchTableData()

    expect(api.tableData.value).toEqual([{ id: 1, name: 'test' }])
    expect(api.pagination.total).toBe(1)
    expect(Request.get).toHaveBeenCalledWith('/test/list', expect.any(Object))

    wrapper.unmount()
  })

  it('should include filters in query params when fetching data', async () => {
    const mockResponse = {
      success: true,
      data: { list: [], total: 0 },
    }

    const { Request } = await import('@/utils/request')
    vi.mocked(Request.get).mockResolvedValue(mockResponse)

    let api!: ReturnType<typeof useTableData>
    const wrapper = mountComposable((exposed) => {
      api = exposed
    })
    api.filters.status = 'active'
    await api.fetchTableData(true)

    expect(Request.get).toHaveBeenCalledWith(
      '/test/list',
      expect.objectContaining({
        status: 'active',
        page: 1,
      }),
    )

    wrapper.unmount()
  })

  it('should handle pagination changes', async () => {
    const mockResponse = {
      success: true,
      data: { list: [], total: 0 },
    }

    const { Request } = await import('@/utils/request')
    vi.mocked(Request.get).mockResolvedValue(mockResponse)

    let api!: ReturnType<typeof useTableData>
    const wrapper = mountComposable((exposed) => {
      api = exposed
    })

    await api.handlePageChange(2)
    expect(Request.get).toHaveBeenCalledWith('/test/list', expect.objectContaining({ page: 2 }))

    await api.handlePageSizeChange(50)
    expect(Request.get).toHaveBeenCalledWith(
      '/test/list',
      expect.objectContaining({ page: 1, page_size: 50 }),
    )

    wrapper.unmount()
  })

  it('should handle delete row', async () => {
    const mockResponse = { success: true }
    const { Request } = await import('@/utils/request')
    vi.mocked(Request.delete).mockResolvedValue(mockResponse)
    const { ElMessageBox } = await import('element-plus')
    vi.mocked(ElMessageBox.confirm).mockResolvedValue('confirm' as any)

    let api!: ReturnType<typeof useTableData>
    const wrapper = mountComposable((exposed) => {
      api = exposed
    })
    const result = await api.deleteRow('1')
    await Promise.resolve()
    await Promise.resolve()

    expect(result).toBe(false)
    expect(ElMessageBox.confirm).toHaveBeenCalled()
    expect(Request.delete).toHaveBeenCalledWith('/test/delete/1')

    wrapper.unmount()
  })

  it('should clear filters and keyword when reloading filters', async () => {
    const mockResponse = {
      success: true,
      data: { list: [], total: 0 },
    }

    const { Request } = await import('@/utils/request')
    vi.mocked(Request.get).mockResolvedValue(mockResponse)

    let api!: ReturnType<typeof useTableData>
    const wrapper = mountComposable((exposed) => {
      api = exposed
    })
    api.filters.status = 'active'

    api.reloadFilters()

    expect(Request.get).toHaveBeenCalledWith(
      '/test/list',
      expect.objectContaining({
        page: 1,
      }),
    )
    expect(api.filters.status).toBeNull()
    expect(api.queryParams.keyword).toBe('')

    wrapper.unmount()
  })
})
