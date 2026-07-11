import { Request } from '@/utils/request'
import type { ApiResponse } from '@/types/user'
import type {
  DeviceInstanceDetailResponse,
  ProductListResponse,
  DeviceInstanceDetail,
  AddDeviceInstanceDetail,
  InstancePointList,
  InstanceMappingList,
} from '@/types/deviceConfiguration'

export const getInstanceDetail = (
  instanceId: number,
): Promise<ApiResponse<DeviceInstanceDetailResponse>> => {
  return Request.get(`/modApi/api/instances/${instanceId}`)
}
/*
获取产品列表
*/
export const getProducts = (): Promise<ApiResponse<ProductListResponse>> => {
  return Request.get('/modApi/api/products')
}
export const createInstance = (data: AddDeviceInstanceDetail) => {
  return Request.post('/modApi/api/instances', data)
}
export const updateInstance = (data: DeviceInstanceDetail) => {
  return Request.put(`/modApi/api/instances/${data.instance_id}`, data)
}
/*
获取设备实例点位
*/
export const getInstancePoints = (instanceId: number): Promise<ApiResponse<InstancePointList>> => {
  return Request.get(`/modApi/api/instances/${instanceId}/points`)
}

export const executeAction = (
  instanceId: number,
  data: { value: string | number; point_id: string },
): Promise<ApiResponse<any>> => {
  return Request.post(`/modApi/api/instances/${instanceId}/action`, data)
}

/** 下发测量值 */
export const executeMeasurement = (
  instanceId: number,
  data: { point_id: string; value: number },
): Promise<ApiResponse<any>> => {
  return Request.post(`/modApi/api/instances/${instanceId}/measurement`, data)
}

/** 获取设备实例点位映射 */
export const getInstanceMappings = (
  instanceId: number,
): Promise<ApiResponse<InstanceMappingList>> => {
  return Request.get(`/modApi/api/instances/${instanceId}/routing`)
}

/** 按新结构批量更新设备实例映射 */
export const updateInstanceMappings = (
  instanceId: number,
  data: { mappings: any[] },
): Promise<ApiResponse<any>> => {
  return Request.put(`/modApi/api/instances/${instanceId}/mappings`, data)
}

/** 批量更新设备实例路由（新接口） */
export const updateInstanceRouting = (
  instanceId: number,
  data: Array<{
    channel_id: number
    channel_point_id: number
    four_remote: string
    point_id: number
  }>,
): Promise<ApiResponse<any>> => {
  return Request.put(`/ruleApi/api/instances/${instanceId}/routing`, data)
}

export const getAllInstances = () => {
  return Request.get('/modApi/api/instances/list')
}

/** 批量获取实例信息（用于回显优化） */
export const getInstancesByIds = (ids: number[]) => {
  if (!ids || ids.length === 0) {
    return Promise.resolve({ success: true, data: { list: [] } })
  }
  const idsParam = ids.join(',')
  return Request.get(`/modApi/api/instances/search`, { ids: idsParam })
}
