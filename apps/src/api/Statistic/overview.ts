import Request, { type ApiResponse } from '@/utils/request'
import type { QueryPowerTrendParams } from '@/types/Statistics/OverView'

// 查询指定测点的功率趋势数据
export const queryPowerTrend = (params: QueryPowerTrendParams): Promise<ApiResponse<any>> => {
  return Request.get('/hisApi/data/query', params)
}
