import dayjs from 'dayjs'

export type TimeRange = {
  start?: string
  end?: string
}

// 最近 X 小时
export const getRecentHoursRange = (hours: number): TimeRange => {
  const end = dayjs()
  const start = end.subtract(hours, 'hour')
  return {
    start: start.toISOString(),
    end: end.toISOString(),
  }
}

// 最近 X 天
export const getRecentDaysRange = (days: number): TimeRange => {
  const end = dayjs()
  const start = end.subtract(days, 'day')
  return {
    start: start.toISOString(),
    end: end.toISOString(),
  }
}

// 最近一周
export const getRecentWeekRange = () => getRecentDaysRange(7)
