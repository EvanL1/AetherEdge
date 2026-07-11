export interface QueryPowerTrendParams {
  redis_key: string
  start_time?: string
  end_time?: string
  page?: number
  page_size?: number
  point_id: string
  interval?: number
}
