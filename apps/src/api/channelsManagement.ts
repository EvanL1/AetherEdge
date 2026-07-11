import { Request } from '@/utils/request'
import type {
  ChannelDetail,
  PointType,
  updateChannelDetail,
  BatchUpdateMappingPointRequest,
  PublishPointsRequest,
  BatchPointsChangeRequest,
} from '@/types/channelConfiguration'

/**
 * 修改通道启用状态
 * @param id 通道ID
 * @param enabled 启用状态
 * @returns
 */
export const ChangeChannelEnabled = (id: number, enabled: boolean) => {
  return Request.put(`/comApi/api/channels/${id}/enabled`, { enabled })
}
/**
 * 获取通道详情
 * @param id 通道ID
 * @returns
 */
export const getChannelDetail = (id: number) => {
  return Request.get(`/comApi/api/channels/${id}`, null, { timeout: 60000 })
}
/**
 * 修改通道详情
 * @param id 通道ID
 * @param data 通道详情
 * @returns
 */
export const updateChannel = (id: number, data: updateChannelDetail) => {
  return Request.put(`/comApi/api/channels/${id}`, data)
}
/**
 * 创建通道
 * @param data 通道详情
 * @returns
 */
export const createChannel = (data: ChannelDetail) => {
  return Request.post('/comApi/api/channels', data)
}
/**
 * 控制通道状态
 * @param id 通道ID
 * @param data 通道状态 'start' | 'stop' | 'restart'
 * @returns
 */
export const controlChannelStatus = (id: number, data: 'start' | 'stop' | 'restart') => {
  return Request.post(`/comApi/api/channels/${id}/control`, { operation: data })
}

export const getPointsTables = (id: number, type?: PointType, config?: any) => {
  return Request.get(`/comApi/api/channels/${id}/points`, type ? { type } : null, config)
}

/** 获取未映射的点位列表（用于新增映射） */
export const getUnmappedPoints = (id: number, type: PointType) => {
  return Request.get(`/comApi/api/channels/${id}/unmapped-points`, { type })
}

export const getMappingPoints = (id: number, type: PointType, pointId: number) => {
  return Request.get(`/comApi/api/channels/${id}/${type}/points/${pointId}/mapping`)
}

/**
 * 获取通道映射表
 * @param id 通道ID
 */
export const getChannelMappings = (id: number) => {
  return Request.get(`/comApi/api/channels/${id}/mappings`, null)
}

// /** 发布控制值 */
// export const postControlValue = (
//   channelId: number,
//   pointId: number,
//   value: boolean | number | string,
// ) => {
//   return Request.post(`/comApi/api/channels/${channelId}/points/${pointId}/control`, { value })
// }

// /** 发布调节值 */
// export const postAdjustmentValue = (
//   channelId: number,
//   pointId: number,
//   value: boolean | number | string,
// ) => {
//   return Request.post(`/comApi/api/channels/${channelId}/points/${pointId}/adjustment`, { value })
// }
export const publishPointValue = (channelId: number, data: PublishPointsRequest) => {
  return Request.post(`/comApi/api/channels/${channelId}/write`, data)
}
/** 批量发布控制值 */
export const postControlBatch = (
  channelId: number,
  commands: Array<{ point_id: number; value: number }>,
) => {
  return Request.post(`/comApi/api/channels/${channelId}/control/batch`, { commands })
}

/** 批量发布调节值 */
export const postAdjustmentBatch = (
  channelId: number,
  commands: Array<{ point_id: number; value: number }>,
) => {
  return Request.post(`/comApi/api/channels/${channelId}/adjustment/batch`, { commands })
}

/** 更新点位映射 */
export const batchUpdateMappingPoint = (id: number, data: BatchUpdateMappingPointRequest) => {
  return Request.put(`/comApi/api/channels/${id}/mappings`, data)
}

/** 批量增删改点位（基础信息） */
export const postPointsBatch = (channelId: number, data: BatchPointsChangeRequest) => {
  return Request.post(`/comApi/api/channels/${channelId}/points/batch`, data)
}

/** 获取通道列表（用于下拉选项） */
export const getAllChannels = () => {
  return Request.get('/comApi/api/channels/list')
}

/** 批量获取通道信息（用于导入后回显） */
export const getChannelsByIds = (ids: number[], config?: any) => {
  if (!ids || ids.length === 0) {
    return Promise.resolve({ success: true, data: { list: [] } })
  }
  const idsParam = ids.join(',')
  return Request.get(`/comApi/api/channels/search`, { ids: idsParam }, config)
}
