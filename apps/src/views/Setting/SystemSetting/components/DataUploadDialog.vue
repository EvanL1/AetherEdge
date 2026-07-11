<template>
  <FormDialog
    ref="formDialogRef"
    :title="'MQTT Config Detail'"
    width="9.52rem"
    @close="handleClose"
  >
    <template #dialog-body>
      <el-form
        ref="FormRef"
        :model="formData"
        :rules="rules"
        label-width="1.75rem"
        label-position="right"
        class="data-upload-dialog"
        inline
      >
        <!-- 1. Client identity -->
        <div class="config-title">Client identity</div>
        <div class="config-collapse">
          <el-form-item label="Product SN:" prop="product_sn">
            <el-input v-model="formData.product_sn" placeholder="Enter product sn" />
          </el-form-item>
          <el-form-item label="Device SN:" prop="device_sn">
            <el-input v-model="formData.device_sn" placeholder="Enter device sn" />
          </el-form-item>
          <el-form-item label="Client ID:" prop="client_id">
            <el-input v-model="formData.client_id" placeholder="Enter client ID" />
          </el-form-item>
        </div>

        <!-- 2. Broker connection -->
        <div class="config-title">Broker connection</div>
        <div class="config-collapse">
          <el-form-item label="Host:" prop="broker_host" class="data-upload-dialog__full-row">
            <el-input
              v-model="formData.broker_host"
              placeholder="Enter host address"
              class="data-upload-dialog__full-field"
            />
          </el-form-item>
          <el-form-item label="Port:" prop="broker_port">
            <el-input-number
              v-model="formData.broker_port"
              :min="1"
              :max="65535"
              :controls="false"
              align="left"
              placeholder="Enter port number"
            />
          </el-form-item>
          <el-form-item label="SSL Enabled:" prop="ssl_enabled">
            <el-switch v-model="formData.ssl_enabled" />
          </el-form-item>
          <el-form-item label="Keepalive(s):" prop="broker_keepalive_secs">
            <el-input-number
              v-model="formData.broker_keepalive_secs"
              :min="1"
              :max="3600"
              :controls="false"
              align="left"
              placeholder="Enter keepalive seconds"
            />
          </el-form-item>
          <el-form-item label="AlarmSrv URL:" prop="alarmsrv_url">
            <el-input v-model="formData.alarmsrv_url" placeholder="Enter alarmsrv url" />
          </el-form-item>
        </div>

        <!-- 3. Reconnect & reporting -->
        <div class="config-title">Reconnect & reporting</div>
        <div class="config-collapse">
          <el-form-item label="Reconnect Delay(s):" prop="reconnect_delay_secs">
            <el-input-number
              v-model="formData.reconnect_delay_secs"
              :min="1"
              :max="3600"
              :controls="false"
              align="left"
              placeholder="Enter reconnect delay"
            />
          </el-form-item>
          <el-form-item label="Reconnect Max Attempts:" prop="reconnect_max_attempts">
            <el-input-number
              v-model="formData.reconnect_max_attempts"
              :min="1"
              :max="1000"
              :controls="false"
              align="left"
              placeholder="Enter max attempts"
            />
          </el-form-item>
          <el-form-item label="Report Interval(s):" prop="report_interval_secs">
            <el-input-number
              v-model="formData.report_interval_secs"
              :min="1"
              :max="3600"
              :controls="false"
              align="left"
              placeholder="Enter report interval"
            />
          </el-form-item>
          <el-form-item label="Report Batch Size:" prop="report_batch_size">
            <el-input-number
              v-model="formData.report_batch_size"
              :min="1"
              :max="1000"
              :controls="false"
              align="left"
              placeholder="Enter report batch size"
            />
          </el-form-item>
          <el-form-item
            label="Subscribe Patterns:"
            prop="subscribe_patterns"
            class="data-upload-dialog__full-row"
          >
            <el-input
              v-model="subscribePatternsText"
              type="textarea"
              :rows="2"
              placeholder="inst:*:M, inst:*:A"
            />
          </el-form-item>
          <el-form-item label="Exclude Patterns:" class="data-upload-dialog__full-row">
            <el-input
              v-model="excludePatternsText"
              type="textarea"
              :rows="2"
              placeholder="comma separated patterns"
            />
          </el-form-item>
        </div>

        <!-- 4. System monitor -->
        <div class="config-title">System monitor</div>
        <div class="config-collapse">
          <el-form-item label="Monitor Enabled:" prop="system_monitor_enabled">
            <el-switch v-model="formData.system_monitor_enabled" />
          </el-form-item>
          <el-form-item label="Monitor Interval(s):" prop="system_monitor_interval_secs">
            <el-input-number
              v-model="formData.system_monitor_interval_secs"
              :min="1"
              :max="3600"
              :controls="false"
              align="left"
              placeholder="Enter monitor interval"
            />
          </el-form-item>
        </div>
      </el-form>
    </template>

    <template #dialog-footer>
      <div class="dialog-footer">
        <el-button type="primary" plain @click="openTlsDialog">TLS Certificate Config</el-button>
        <div class="dialog-footer__right">
          <el-button @click="close">Cancel</el-button>
          <el-button type="primary" @click="submitDialog" :loading="submitLoading"
            >Submit</el-button
          >
        </div>
      </div>
    </template>
  </FormDialog>
  <TlsCertificateDialog ref="tlsCertificateDialogRef" />
</template>

<script setup lang="ts">
import type { FormInstance, FormRules } from 'element-plus'
import { updateMqttConfig } from '@/api/System'
import type { FormData } from '../DataUpdataSetting.vue'
import TlsCertificateDialog from './TlsCertificateDialog.vue'

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
const submitLoading = ref(false)
const formDialogRef = ref()

const FormRef = ref<FormInstance>()
const subscribePatternsText = ref('')
const excludePatternsText = ref('')
const tlsCertificateDialogRef = ref<{ open: () => void } | null>(null)

const rules = ref<FormRules<FormData>>({
  product_sn: [{ required: true, message: 'Please enter product SN', trigger: 'blur' }],
  device_sn: [{ required: true, message: 'Please enter device SN', trigger: 'blur' }],
  client_id: [
    { required: true, message: 'Please enter client ID', trigger: 'blur' },
    { min: 1, max: 50, message: 'Client ID length should be 1-50 characters', trigger: 'blur' },
  ],
  broker_host: [{ required: true, message: 'Please enter host address', trigger: 'blur' }],
  alarmsrv_url: [{ required: true, message: 'Please enter alarmsrv url', trigger: 'blur' }],
  broker_keepalive_secs: [
    { required: true, message: 'Please enter keepalive seconds', trigger: 'blur' },
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
  report_interval_secs: [
    { required: true, message: 'Please enter report interval', trigger: 'blur' },
  ],
  report_batch_size: [
    { required: true, message: 'Please enter report batch size', trigger: 'blur' },
  ],
  system_monitor_enabled: [
    { required: true, message: 'Please select monitor status', trigger: 'change' },
  ],
  system_monitor_interval_secs: [
    { required: true, message: 'Please enter monitor interval', trigger: 'blur' },
  ],
  subscribe_patterns: [
    { required: true, message: 'Please enter subscribe patterns', trigger: 'blur' },
    {
      validator: (rule, value, callback) => {
        if (!subscribePatternsText.value.trim()) {
          callback(new Error('Please enter subscribe patterns'))
          return
        }
        callback()
      },
      trigger: 'blur',
    },
  ],
  reconnect_delay_secs: [
    { required: true, message: 'Please enter reconnect delay', trigger: 'blur' },
    {
      validator: (rule, value, callback) => {
        if (!value || value < 1 || value > 360000) {
          callback(new Error('Delay must be between 1 and 360000 seconds'))
        } else {
          callback()
        }
      },
      trigger: 'blur',
    },
  ],
  reconnect_max_attempts: [
    { required: true, message: 'Please enter max attempts', trigger: 'blur' },
    {
      validator: (rule, value, callback) => {
        if (!value || value < 1 || value > 10000) {
          callback(new Error('Max attempts must be between 1 and 10000'))
        } else {
          callback()
        }
      },
      trigger: 'blur',
    },
  ],
})

const emit = defineEmits(['update'])
const open = (data: FormData) => {
  formData.value = { ...data }
  subscribePatternsText.value = formData.value.subscribe_patterns.join(', ')
  excludePatternsText.value = formData.value.exclude_patterns.join(', ')
  formDialogRef.value.dialogVisible = true
}
const close = () => {
  formDialogRef.value.dialogVisible = false
}

const openTlsDialog = () => {
  tlsCertificateDialogRef.value?.open()
}
const handleClose = () => {
  // keep form data
}

const submitDialog = async () => {
  const formInstance = FormRef.value
  if (!formInstance) return

  const valid = await formInstance.validate().catch(() => false)
  if (!valid) return

  const subscribePatterns = subscribePatternsText.value
    .split(',')
    .map((item) => item.trim())
    .filter(Boolean)
  const excludePatterns = excludePatternsText.value
    .split(',')
    .map((item) => item.trim())
    .filter(Boolean)

  if (!subscribePatterns.length) {
    ElMessage.warning('Subscribe Patterns is required')
    return
  }

  try {
    submitLoading.value = true
    const raw = toRaw(formData.value)
    // 仅透传 MQTT 配置字段，避免提交历史证书相关字段。
    const params: FormData = {
      alarmsrv_url: raw.alarmsrv_url,
      broker_host: raw.broker_host,
      broker_keepalive_secs: raw.broker_keepalive_secs,
      broker_port: raw.broker_port,
      client_id: raw.client_id,
      device_sn: raw.device_sn,
      exclude_patterns: [],
      product_sn: raw.product_sn,
      reconnect_delay_secs: raw.reconnect_delay_secs,
      reconnect_max_attempts: raw.reconnect_max_attempts,
      report_batch_size: raw.report_batch_size,
      report_interval_secs: raw.report_interval_secs,
      ssl_enabled: raw.ssl_enabled,
      subscribe_patterns: [],
      system_monitor_enabled: raw.system_monitor_enabled,
      system_monitor_interval_secs: raw.system_monitor_interval_secs,
    }
    params.subscribe_patterns = subscribePatterns
    params.exclude_patterns = excludePatterns

    const res = await updateMqttConfig(params)
    if (res.success) {
      ElMessage.success('Update success')
      close()
      emit('update')
      return
    }
    ElMessage.error(res.message || 'Update failed')
  } finally {
    submitLoading.value = false
  }
}

defineExpose({ open })
</script>

<style scoped lang="scss">
.aether-class {
  .data-upload-dialog {
    max-height: 6rem;
    overflow-y: auto;
  }
  .config-title {
    font-size: 0.16rem;
    color: #fff;
    margin-bottom: 0.16rem;
    font-weight: 700;
    padding-bottom: 0.1rem;
    border-bottom: 0.01rem solid rgba(255, 255, 255, 0.1);

    &:not(:first-child) {
      margin-top: 0.22rem;
    }
  }
  .config-collapse {
    border: none;
    display: flex;
    flex-wrap: wrap;
    :deep(.el-form-item) {
      position: relative;
      margin-right: 0;
      margin-bottom: 0.2rem;
    }
  }

  :deep(.data-upload-dialog__full-row) {
    width: 100%;
  }

  :deep(.data-upload-dialog__full-field) {
    width: 100%;
  }

  .upload-hint {
    position: absolute;
    top: 0.27rem;
    left: 0;
    display: flex;
    align-items: center;
    gap: 0.08rem;
    font-size: 0.12rem;
    color: #fff;
    // margin-top: 0.06rem;

    .upload-hint__progress {
      color: #ff6900;
    }
  }

  .dialog-footer {
    display: flex;
    justify-content: space-between;
    align-items: center;
    width: 100%;
  }

  .dialog-footer__right {
    display: flex;
    gap: 0.1rem;
    margin-left: auto;
  }
}
</style>
