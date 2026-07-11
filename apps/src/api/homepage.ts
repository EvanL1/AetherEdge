/**
 * 首页点位配置 API
 * 用于获取首页 Energy Card、Station、Device、拓扑图等区域的可配置点位列表
 */

import { Request } from '@/utils/request'

/** 首页计算点位项（API 返回的单项） */
export interface HomepagePointItem {
  id: number
  name: string
  formula: string
  unit: string
  imgurl: string
  description: string
  created_at: string
  updated_at: string
}

/** 首页点位列表响应 */
export interface HomepagePointsResponse {
  items: HomepagePointItem[]
  total: number
  page: number
  limit: number
  pages: number
}

/** 用于首页展示的点位（含 value 占位，供 WebSocket 更新） */
export interface HomepageDisplayPoint {
  id: number
  name: string
  unit: string
  value: string | number
}

/**
 * 获取首页计算点位列表
 * @param limit 每页数量，默认 100
 */
export function getHomepagePoints(limit = 100) {
  return Request.get<HomepagePointsResponse>('/api/v1/homepage', {
    limit,
  })
}
