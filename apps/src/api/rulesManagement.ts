import Request from '@/utils/request'
import type { Rule, CreateRulePayload, UpdateRulePayload } from '@/types/ruleConfiguration'
import type { RuleChainPayload } from '@/types/ruleConfiguration'

export const listRules = async () => {
  return await Request.get<{ list: Rule[] }>('/ruleApi/api/rules')
}

export const getRuleDetail = async (id: string) => {
  return await Request.get<RuleChainPayload>(`/ruleApi/api/rules/${id}`)
}

export const createRule = async (payload: CreateRulePayload) => {
  return await Request.post<Rule>('/ruleApi/api/rules', payload)
}

export const updateRule = async (
  payload: RuleChainPayload | { name: string; description: string; id: string },
) => {
  return await Request.put<Rule>(`/ruleApi/api/rules/${payload.id}`, payload)
}

export const deleteRule = async (id: string) => {
  return await Request.delete(`/ruleApi/api/rules/${id}`)
}

export const enableRule = async (id: string) => {
  return await Request.post(`/ruleApi/api/rules/${id}/enable`)
}

export const disableRule = async (id: string) => {
  return await Request.post(`/ruleApi/api/rules/${id}/disable`)
}

export const submitRuleChain = async (payload: RuleChainPayload) => {
  return await Request.post('/ruleApi/api/rules', payload)
}
