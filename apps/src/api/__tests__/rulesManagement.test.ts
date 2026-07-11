import { beforeEach, describe, expect, it, vi } from 'vitest'
import {
  createRule,
  deleteRule,
  disableRule,
  enableRule,
  getRuleDetail,
  listRules,
  submitRuleChain,
  updateRule,
} from '../rulesManagement'

vi.mock('@/utils/request', () => {
  const Request = {
    get: vi.fn(),
    post: vi.fn(),
    put: vi.fn(),
    delete: vi.fn(),
  }
  return {
    default: Request,
    Request,
  }
})

describe('api/rulesManagement.ts', () => {
  beforeEach(() => {
    vi.clearAllMocks()
  })

  it('lists and fetches rule details', async () => {
    const RequestModule = await import('@/utils/request')
    vi.mocked(RequestModule.default.get)
      .mockResolvedValueOnce({ success: true, data: { list: [] } })
      .mockResolvedValueOnce({ success: true, data: { id: 'rule-1' } })

    await listRules()
    await getRuleDetail('rule-1')

    expect(RequestModule.default.get).toHaveBeenNthCalledWith(1, '/ruleApi/api/rules')
    expect(RequestModule.default.get).toHaveBeenNthCalledWith(2, '/ruleApi/api/rules/rule-1')
  })

  it('creates, updates and deletes rules', async () => {
    const RequestModule = await import('@/utils/request')
    vi.mocked(RequestModule.default.post).mockResolvedValue({ success: true })
    vi.mocked(RequestModule.default.put).mockResolvedValue({ success: true })
    vi.mocked(RequestModule.default.delete).mockResolvedValue({ success: true })

    const createPayload = { name: 'Rule A', description: 'desc' }
    const updatePayload = { id: 'rule-1', name: 'Rule B', description: 'updated' }

    await createRule(createPayload as any)
    await updateRule(updatePayload as any)
    await deleteRule('rule-1')

    expect(RequestModule.default.post).toHaveBeenNthCalledWith(
      1,
      '/ruleApi/api/rules',
      createPayload,
    )
    expect(RequestModule.default.put).toHaveBeenCalledWith(
      '/ruleApi/api/rules/rule-1',
      updatePayload,
    )
    expect(RequestModule.default.delete).toHaveBeenCalledWith('/ruleApi/api/rules/rule-1')
  })

  it('enables, disables and submits rule chains', async () => {
    const RequestModule = await import('@/utils/request')
    vi.mocked(RequestModule.default.post)
      .mockResolvedValueOnce({ success: true })
      .mockResolvedValueOnce({ success: true })
      .mockResolvedValueOnce({ success: true })

    const chainPayload = {
      id: 'rule-2',
      name: 'Rule Chain',
      description: 'dispatch',
      flow_json: { nodes: [], edges: [] },
    }

    await enableRule('rule-2')
    await disableRule('rule-2')
    await submitRuleChain(chainPayload as any)

    expect(RequestModule.default.post).toHaveBeenNthCalledWith(
      1,
      '/ruleApi/api/rules/rule-2/enable',
    )
    expect(RequestModule.default.post).toHaveBeenNthCalledWith(
      2,
      '/ruleApi/api/rules/rule-2/disable',
    )
    expect(RequestModule.default.post).toHaveBeenNthCalledWith(
      3,
      '/ruleApi/api/rules',
      chainPayload,
    )
  })
})
