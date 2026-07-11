// // 设备配置相关类型定义

// // 关联点类型
// export interface RelatedPoint {
//   id: number
//   name: string
//   type: string
// }

// // 量测属性类型
// export interface MeasurementProperty {
//   id: number
//   name: string
//   dataType: string
//   unit: string
//   defaultValue: string
//   required: boolean
//   relatedPoints: RelatedPoint[]
// }

// // 控制属性类型
// export interface ControlProperty {
//   id: number
//   name: string
//   dataType: string
//   controlType: string
//   unit: string
//   defaultValue: string
//   required: boolean
//   relatedPoints: RelatedPoint[]
// }

// // 设备模型类型
// export interface Model {
//   id: number
//   name: string
//   description: string
//   measurementProperties: MeasurementProperty[]
//   controlProperties: ControlProperty[]
//   activePropertyTab: string // 用于控制属性标签页的激活状态
//   originalMeasurementData?: MeasurementProperty[] // 量测属性原始数据备份
//   originalControlData?: ControlProperty[] // 控制属性原始数据备份
//   isEditingMeasurement?: boolean
//   isEditingControl?: boolean
// }

// // 模型表单类型
// export interface ModelForm {
//   name: string
//   description: string
// }

// // 对话框暴露类型
// export interface DialogExpose {
//   dialogVisible: boolean
// }

// // 属性类型
// export interface property {
//   id: number
//   name: string
//   dataType: string
//   unit: string
//   defaultValue: string
//   required: boolean
//   controlType?: string
//   relatedPoints: RelatedPoint[]
// }

// // 组件Props类型
// export interface Props {
//   properties: property[]
//   isEditing: boolean
//   onUpdate: (properties: property[]) => void
// }
export interface DeviceInstanceBasic {
  instance_id: number
  instance_name: string
  product_name: string
}
export interface AddDeviceInstanceDetail {
  instance_name: string
  product_name: string
  properties: {
    [key: string]: string | number
  }
}
export interface DeviceInstanceDetail extends DeviceInstanceBasic {
  properties: {
    [key: string]: string | number
  }
}
export interface DeviceInstanceDetailResponse {
  instance: DeviceInstanceDetail
}
// 量测配置信息
export interface MeasurementConfig {
  id: string
  name: string
  value: string | number
  config: {
    channelName: string
    channelType: string
    point: string
    setting: boolean
  }
}

// 动作配置信息
export interface ActionConfig {
  id: string
  name: string
  config: {
    channelName: string
    channelType: string
    point: string
    setting: boolean
  }
}
export interface InstanceMappingItem {
  channel: {
    four_remote: string
    id: number
    point_id: number
  }
  description: string
  enabled: boolean
  point_id: number
}
export interface InstanceMappingList {
  measurement: InstanceMappingItem[]
  action: InstanceMappingItem[]
}
// 产品选项
export interface ProductOption {
  value: string
  label: string
}

// 产品列表
export interface ProductListItem {
  product_name: string
  parent_name: string | null
}
export interface ProductListResponse {
  count: number
  products: ProductListItem[]
}
export interface InstancePointRouting {
  channel_id: number
  channel_point_id: number
  channel_type: 'T' | 'S' | 'C' | 'A'
  enabled: boolean
  channel_name: string
  channel_point_name: string
}
//点位信息
export interface InstancePointItem {
  point_index: number
  name: string
  unit: string
  description: string
  routing?: InstancePointRouting
}

export interface InstanceActionItem extends InstancePointItem {
  action_id: number
}
export interface InstanceMeasurementItem extends InstancePointItem {
  measurement_id: number
}
export interface InstancePropertyItem extends InstancePointItem {
  property_id: number
}
export interface InstancePointList {
  actions: {
    [key: string]: InstanceActionItem
  }
  measurements: {
    [key: string]: InstanceMeasurementItem
  }
  properties: {
    [key: string]: InstancePropertyItem
  }
}
