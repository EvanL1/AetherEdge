<template>
  <div class="aether-class data-update-setting">
    <ModuleCard title="MQTT Config">
      <el-form
        ref="formRef"
        :model="formData"
        :rules="rules"
        class="data-update-form"
        label-width="1.35rem"
        label-position="right"
      >
        <!-- 连接状态来自 GET /netApi/mqtt/status，只读展示 -->

        <!-- 仅展示 Host、Port 两项（保持原始校验/数据结构不变） -->
        <div class="config-collapse">
          <div class="collapse-content">
            <el-form-item label="Connected Status:">
              <div class="connection-status">
                <el-tag :type="connected ? 'success' : 'info'" effect="dark" round>
                  {{ connected ? 'Connected' : 'Disconnected' }}
                </el-tag>
                <el-button
                  class="connection-status__refresh"
                  :icon="Refresh"
                  circle
                  text
                  type="primary"
                  :loading="statusRefreshLoading"
                  :disabled="mqttCardActionsDisabled"
                  title="Refresh status"
                  @click="handleRefreshStatusClick"
                />
              </div>
            </el-form-item>
            <el-form-item label="Host:" prop="broker_host">
              <el-input v-model="formData.broker_host" placeholder="Enter host address" />
            </el-form-item>
            <el-form-item label="Port:" prop="broker_port">
              <el-input-number
                v-model="formData.broker_port"
                :min="1"
                :max="65535"
                :controls="false"
                align="left"
                class="data-update-form__full-field"
                placeholder="Enter port number"
              />
            </el-form-item>
          </div>
        </div>
      </el-form>
      <template #footer>
        <div class="card__content-footer">
          <el-button type="primary" :disabled="mqttCardActionsDisabled" @click="openDetail"
            >Detail</el-button
          >
          <div class="card__content-footer__right">
            <el-button
              v-if="connected"
              :disabled="mqttCardActionsDisabled"
              :loading="mqttOperationLoading"
              @click="handleDisconnectClick"
            >
              Disconnect
            </el-button>
            <el-button
              type="primary"
              :disabled="mqttCardActionsDisabled"
              :loading="mqttOperationLoading"
              @click="handleReconnectClick"
            >
              Reconnect
            </el-button>
            <el-button
              type="primary"
              :disabled="mqttCardActionsDisabled"
              :loading="submitLoading"
              @click="handleSubmit"
            >
              Submit
            </el-button>
          </div>
        </div>
      </template>
    </ModuleCard>
    <!-- 详情弹窗 -->
    <DataUploadDialog ref="detailDialogRef" @update="handleUpdate" />
  </div>
</template>

<script setup lang="ts">
import type { FormInstance, FormRules } from 'element-plus'
import DataUploadDialog from './components/DataUploadDialog.vue'
import { Refresh } from '@element-plus/icons-vue'

import {
  getMqttConfig,
  updateMqttConfig,
  disconnectMqtt,
  reconnectMqtt,
  getMqttStatus,
} from '@/api/System'
const formRef = ref<FormInstance>()

// 保留简化后的展示，不再使用折叠面板

export interface FormData {
  alarmsrv_url: string
  broker_host: string
  broker_keepalive_secs: number
  broker_port: number
  client_id: string
  device_sn: string
  exclude_patterns: string[]
  product_sn: string
  reconnect_delay_secs: number
  reconnect_max_attempts: number
  report_batch_size: number
  report_interval_secs: number
  ssl_enabled: boolean
  subscribe_patterns: string[]
  system_monitor_enabled: boolean
  system_monitor_interval_secs: number
}
interface MqttStatusData {
  broker?: string
  connected?: boolean
  device_sn?: string
  product_sn?: string
  ssl?: boolean
}

const connected = ref(false)
const mqttStatusSnapshot = ref<MqttStatusData | null>(null)
const mqttOperationLoading = ref(false)
const statusRefreshLoading = ref(false)
const submitLoading = ref(false)

/** 重连/断开轮询中，或提交配置/重连流程中，卡片内操作按钮均不可点 */
const mqttCardActionsDisabled = computed(() => mqttOperationLoading.value || submitLoading.value)

const handleRefreshStatusClick = async () => {
  if (statusRefreshLoading.value || mqttCardActionsDisabled.value) return
  try {
    statusRefreshLoading.value = true
    await refreshMqttStatus()
  } finally {
    statusRefreshLoading.value = false
  }
}

const POLL_INTERVAL_MS = 1000
const POLL_MAX_ATTEMPTS = 10
let pollTimer: ReturnType<typeof setInterval> | null = null

const stopStatusPolling = () => {
  if (pollTimer !== null) {
    clearInterval(pollTimer)
    pollTimer = null
  }
}

const applyStatusPayload = (data: MqttStatusData | undefined) => {
  if (!data) return
  mqttStatusSnapshot.value = data
  if (typeof data.connected === 'boolean') {
    connected.value = data.connected
  }
}

/** 拉取一次状态并更新 UI */
const refreshMqttStatus = async () => {
  const response = await getMqttStatus()
  if (response.success && response.data) {
    applyStatusPayload(response.data as MqttStatusData)
  }
}

/**
 * 在发起重连/断开后轮询，直到 connected 与期望值一致；最多请求 POLL_MAX_ATTEMPTS 次状态。
 */
const pollUntilConnectedIs = (expected: boolean): Promise<void> => {
  stopStatusPolling()
  let attempts = 0
  let tickBusy = false
  return new Promise((resolve) => {
    const finish = () => {
      stopStatusPolling()
      resolve()
    }
    const tick = async () => {
      if (tickBusy) return
      tickBusy = true
      try {
        attempts += 1
        if (attempts > POLL_MAX_ATTEMPTS) {
          mqttOperationLoading.value = false
          ElMessage.warning(
            'Status synchronization timed out, please refresh the page or try again later',
          )
          finish()
          return
        }
        try {
          const response = await getMqttStatus()
          if (response.success && response.data) {
            applyStatusPayload(response.data as MqttStatusData)
            if (connected.value === expected) {
              mqttOperationLoading.value = false
              finish()
              return
            }
          }
        } catch {
          // 忽略单次失败，继续轮询
        }
      } finally {
        tickBusy = false
      }
    }
    void tick()
    pollTimer = setInterval(() => void tick(), POLL_INTERVAL_MS)
  })
}

/** 与点击 Reconnect 相同：请求重连后轮询直至已连接 */
const executeReconnectFlow = async () => {
  try {
    mqttOperationLoading.value = true
    const response = await reconnectMqtt()
    if (!response.success) {
      mqttOperationLoading.value = false
      ElMessage.error(response.message || 'Reconnect failed')
      await refreshMqttStatus()
      return
    }
    if (response.message) {
      ElMessage.success(response.message)
    }
    await pollUntilConnectedIs(true)
  } catch {
    mqttOperationLoading.value = false
    await refreshMqttStatus()
  }
}
const formData = ref<FormData>({
  alarmsrv_url: 'http://localhost:6007',
  broker_host: '127.0.0.1',
  broker_keepalive_secs: 120,
  broker_port: 1883,
  client_id: 'auto',
  device_sn: 'auto',
  exclude_patterns: [],
  product_sn: '',
  reconnect_delay_secs: 10,
  reconnect_max_attempts: 50,
  report_batch_size: 50,
  report_interval_secs: 50,
  ssl_enabled: false,
  subscribe_patterns: [],
  system_monitor_enabled: true,
  system_monitor_interval_secs: 10,
})
const buildMqttConfigPayload = (raw: FormData): FormData => {
  return {
    alarmsrv_url: raw.alarmsrv_url,
    broker_host: raw.broker_host,
    broker_keepalive_secs: raw.broker_keepalive_secs,
    broker_port: raw.broker_port,
    client_id: raw.client_id,
    device_sn: raw.device_sn,
    exclude_patterns: raw.exclude_patterns || [],
    product_sn: raw.product_sn,
    reconnect_delay_secs: raw.reconnect_delay_secs,
    reconnect_max_attempts: raw.reconnect_max_attempts,
    report_batch_size: raw.report_batch_size,
    report_interval_secs: raw.report_interval_secs,
    ssl_enabled: raw.ssl_enabled,
    subscribe_patterns: raw.subscribe_patterns || [],
    system_monitor_enabled: raw.system_monitor_enabled,
    system_monitor_interval_secs: raw.system_monitor_interval_secs,
  }
}
// 表单验证规则
const rules = ref<FormRules<FormData>>({
  client_id: [
    { required: true, message: 'Please enter client ID', trigger: 'blur' },
    { min: 1, max: 50, message: 'Client ID length should be 1-50 characters', trigger: 'blur' },
  ],
  broker_host: [
    { required: true, message: 'Please enter host address', trigger: 'blur' },
    // {
    //   pattern: /^((25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.){3}(25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)$|^[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$/,
    //   message: 'Please enter a valid host address (IP or domain)',
    //   trigger: 'blur'
    // }
  ],
  broker_port: [
    { required: true, message: 'Please enter port number', trigger: 'blur' },
    {
      type: 'number',
      min: 1,
      max: 65535,
      message: 'Port must be between 1 and 65535',
      trigger: 'blur',
    },
  ],
  ssl_enabled: [{ required: true, message: 'Please select SSL enable status', trigger: 'change' }],
})

// 表单提交：成功后与点击 Reconnect 相同（重连 + 轮询）
const handleSubmit = async () => {
  if (!formRef.value) return
  const valid = await formRef.value.validate().catch(() => false)
  if (!valid) return
  try {
    submitLoading.value = true
    const res = await updateMqttConfig(buildMqttConfigPayload(toRaw(formData.value)))
    if (!res.success) {
      ElMessage.error(res.message || 'Update failed')
      return
    }
    ElMessage.success(res.message || 'Update success')
    await executeReconnectFlow()
  } finally {
    submitLoading.value = false
  }
}
const getMqttConfigData = async () => {
  const response = await getMqttConfig()
  formData.value = {
    ...formData.value,
    ...(response.data || {}),
  }
}
const getMqttStatusData = async () => {
  await refreshMqttStatus()
}

const handleReconnectClick = () => executeReconnectFlow()

const handleDisconnectClick = async () => {
  try {
    mqttOperationLoading.value = true
    const response = await disconnectMqtt()
    if (!response.success) {
      mqttOperationLoading.value = false
      ElMessage.error(response.message || 'Disconnect failed')
      await refreshMqttStatus()
      return
    }
    if (response.message) {
      ElMessage.success(response.message)
    }
    await pollUntilConnectedIs(false)
  } catch {
    mqttOperationLoading.value = false
    await refreshMqttStatus()
  }
}

onMounted(() => {
  getMqttConfigData()
  getMqttStatusData()
})

onUnmounted(() => {
  stopStatusPolling()
})

// 打开详情弹窗（仅UI交互，不改变原有业务逻辑）
const detailDialogRef = ref()
const openDetail = () => {
  detailDialogRef.value?.open(formData.value)
}
const handleUpdate = async () => {
  await getMqttConfigData()
  await executeReconnectFlow()
}
</script>

<style scoped lang="scss">
.data-update-setting {
  width: 100%;
  height: 100%;

  .data-update-form {
    width: 100%;
    height: 100%;
    // margin: 0.2rem 0;
    overflow-y: auto;

    // 折叠面板样式
    .config-collapse {
      border: none;
      background: transparent;

      // &>div {
      //   margin-bottom: 0.4rem;

      //   &:last-child {
      //     margin-bottom: 0;
      //   }
      // }

      // :deep(.el-collapse-item) {
      //   margin-bottom: 0.2rem;
      //   border: 1px solid rgba(255, 255, 255, 0.1);
      //   border-radius: 0.08rem;
      //   overflow: hidden;
      //   background: rgba(44, 66, 106, 0.1);

      //   .el-collapse-item__header {
      //     background: linear-gradient(90deg, rgba(44, 66, 106, 0.8) 0%, rgba(44, 66, 106, 0.4) 100%);
      //     color: #ffffff;
      //     font-size: 0.16rem;
      //     font-weight: 600;
      //     padding: 0.12rem 0.2rem;
      //     border-bottom: 1px solid rgba(255, 255, 255, 0.1);
      //     height: auto;
      //     line-height: 1.5;

      //     .el-collapse-item__arrow {
      //       color: #ffffff;
      //       font-size: 0.14rem;
      //       margin-right: 0.1rem;
      //     }

      //     &:hover {
      //       background: linear-gradient(90deg, rgba(44, 66, 106, 0.9) 0%, rgba(44, 66, 106, 0.5) 100%);
      //     }
      //   }

      //   .el-collapse-item__wrap {
      //     border: none;
      //     background: transparent;

      //     .el-collapse-item__content {
      //       padding: 0;
      //       background: rgba(44, 66, 106, 0.1);
      //     }
      //   }
      // }
    }

    // 折叠面板标题样式
    .collapse-title {
      display: flex;
      align-items: center;
      justify-content: space-between;
      width: 100%;
      padding-right: 0.1rem;

      .collapse-title__text {
        flex: 1;
        font-size: 0.16rem;
        font-weight: 600;
        color: #ffffff;
      }
    }

    // 折叠面板内容样式
    .collapse-content {
      padding: 0.2rem 0;

      .el-form-item {
        // margin-bottom: 0.2rem;

        &:last-child {
          margin-bottom: 0;
        }
      }
    }

    // 输入框样式优化
    :deep(.el-input) {
      width: 100%;
    }

    :deep(.data-update-form__full-field) {
      width: 100%;
    }
  }

  // 连接状态样式
  .connection-status {
    display: flex;
    align-items: center;

    // gap: 0.08rem;
    width: 100%;
    justify-content: space-between;

    .connection-status__addr {
      display: inline-flex;
      align-items: center;
      flex-wrap: wrap;
      gap: 0.06rem;
      font-size: 0.12rem;
      color: rgba(255, 255, 255, 0.75);
    }

    .connection-status__addr-sep {
      color: rgba(255, 255, 255, 0.35);
    }

    .connection-status__refresh {
      flex-shrink: 0;
      margin-left: 0.04rem;
    }

    .loading-icon {
      font-size: 0.16rem;
      color: #ff6900;
      animation: rotate 1s linear infinite;
    }
  }

  @keyframes rotate {
    from {
      transform: rotate(0deg);
    }

    to {
      transform: rotate(360deg);
    }
  }

  .card__content-footer {
    display: flex;
    padding: 0.3rem 0 0.1rem 0;
    width: 100%;
    justify-content: space-between;
    align-items: center;
    gap: 0.1rem;
    flex-wrap: wrap;
  }

  .card__content-footer__right {
    display: flex;
    align-items: center;
    gap: 0.1rem;
    flex-wrap: wrap;
    margin-left: auto;
  }
}
</style>
