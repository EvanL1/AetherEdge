<template>
  <FormDialog width="13.46rem" ref="dialogRef" :title="dialogTitle">
    <template #dialog-body>
      <el-form
        ref="formRef"
        :model="form"
        :rules="rules"
        label-width="0.98rem"
        class="rules-form"
        label-position="right"
        inline
      >
        <el-form-item label="Rule Name:" prop="rule_name">
          <el-input v-model="form.rule_name" placeholder="Enter rule name" />
        </el-form-item>

        <!-- 拆分Monitor Data为三个el-form-item，分别校�?-->
        <div class="monitor-data-group" ref="monitorDataGroupRef">
          <el-form-item label="Monitor Data:" prop="channel_id" class="rules-form__compact-item">
            <el-select
              v-model="form.channel_id"
              placeholder="Select Channel"
              popper-class="rules-dialog-popper"
              :append-to="monitorDataGroupRef"
              @change="handleChannelChange"
              :disabled="loadingChannels"
            >
              <el-option
                v-for="item in channelList"
                :key="item.value"
                :label="item.label"
                :value="item.value"
              />
            </el-select>
          </el-form-item>
          <el-form-item prop="data_type" class="rules-form__compact-item">
            <el-select
              v-model="form.data_type"
              placeholder="Select Point Type"
              popper-class="rules-dialog-popper"
              :append-to="monitorDataGroupRef"
              @change="handlePointTypeChange"
              :disabled="!form.channel_id || loadingPoints"
            >
              <el-option
                v-for="item in data_types"
                :key="item.value"
                :label="item.label"
                :value="item.value"
              />
            </el-select>
          </el-form-item>
          <el-form-item prop="point_id" class="rules-form__compact-item">
            <el-select
              v-model="form.point_id"
              placeholder="Select Point"
              popper-class="rules-dialog-popper"
              :append-to="monitorDataGroupRef"
              :disabled="!form.channel_id || !form.data_type || loadingPoints"
            >
              <el-option
                v-for="item in points"
                :key="item.value"
                :label="item.label"
                :value="item.value"
              />
            </el-select>
          </el-form-item>
        </div>
        <div class="alarm-level-group" ref="alarmLevelGroupRef">
          <el-form-item label="Alarm Level:" prop="warning_level">
            <el-select
              v-model="form.warning_level"
              placeholder="Select level"
              popper-class="rules-dialog-popper"
              :append-to="alarmLevelGroupRef"
            >
              <el-option
                v-for="item in alarmLevelOptions"
                :key="item.value"
                :label="item.label"
                :value="item.value"
              />
            </el-select>
          </el-form-item>
        </div>
        <!-- 拆分Condition为两个el-form-item，分别校�?-->
        <div class="condition-group" ref="conditionGroupRef">
          <el-form-item prop="operator" label="Condition:" class="rules-form__compact-item">
            <el-select
              v-model="form.operator"
              placeholder="Operator"
              popper-class="rules-dialog-popper"
              :append-to="conditionGroupRef"
            >
              <el-option label=">" value=">" />
              <el-option label=">=" value=">=" />
              <el-option label="<" value="<" />
              <el-option label="<=" value="<=" />
              <el-option label="=" value="=" />
            </el-select>
          </el-form-item>
          <el-form-item prop="value" class="rules-form__compact-item">
            <el-input-number
              v-model="form.value"
              :min="0"
              :max="999999"
              :controls="false"
              placeholder="Value"
              class="rules-form__value-input"
              align="left"
            />
          </el-form-item>
        </div>

        <el-form-item label="Enabled:" prop="enabled" class="rules-form__full-row">
          <el-switch v-model="form.enabled" />
        </el-form-item>

        <el-form-item label="Description:" prop="description" class="rules-form__full-row">
          <el-input
            v-model="form.description"
            type="textarea"
            :rows="3"
            placeholder="Enter description (optional)"
            maxlength="50"
            show-word-limit
          />
        </el-form-item>
      </el-form>
    </template>

    <template #dialog-footer>
      <el-button type="warning" @click="onCancel" style="margin-right: 0.2rem">Cancel</el-button>
      <el-button type="primary" @click="onSubmit">Submit</el-button>
    </template>
  </FormDialog>
</template>

<script setup lang="ts">
import type { FormInstance } from 'element-plus'
import { getRuleDetail, createRule, updateRule } from '@/api/alarm'
import { getAllChannels, getPointsTables } from '@/api/channelsManagement'
import type { RuleFormModel, DialogExpose, Operator } from '@/types/ruleManagement'
import type { PointType } from '@/types/channelConfiguration'

const formRef = ref<FormInstance>()
const dialogRef = ref<DialogExpose>()
const monitorDataGroupRef = ref<HTMLElement>()
const alarmLevelGroupRef = ref<HTMLElement>()
const conditionGroupRef = ref<HTMLElement>()

const getDefaultForm = (): RuleFormModel => ({
  rule_name: '',
  service_type: 'comsrv',
  channel_id: undefined,
  point_id: null,
  data_type: null,
  warning_level: null,
  operator: null,
  value: null,
  description: '',
  enabled: true,
})

const form = ref<RuleFormModel>(getDefaultForm())

// 通道列表
const channelList = ref<Array<{ label: string; value: number }>>([])
const loadingChannels = ref(false)

// 所有点位数据（包含T和S类型）
const allPointsData = ref<Array<{ point_id: number; signal_name: string; point_type?: PointType }>>(
  [],
)
// 筛选后的点位列表（用于显示）
const points = computed(() => {
  if (!form.value.data_type) {
    return []
  }
  return allPointsData.value
    .filter((point) => point.point_type === form.value.data_type)
    .map((point) => ({
      label: String(point.signal_name || `Point ${point.point_id}`),
      value: Number(point.point_id),
    }))
})
const loadingPoints = ref(false)

// Point Type 选项（T 和 S）
const data_types = [
  { label: 'T', value: 'T' },
  { label: 'S', value: 'S' },
]

const alarmLevelOptions = [
  { label: 'Critical Alarm', value: 1 },
  { label: 'Warning Alarm', value: 2 },
  { label: 'Info Alarm', value: 3 },
]

const rules = {
  rule_name: [{ required: true, message: 'Please input rule name', trigger: 'blur' }],
  channel_id: [{ required: true, message: 'Required', trigger: 'change' }],
  point_id: [{ required: true, message: 'Required', trigger: 'change' }],
  data_type: [{ required: true, message: 'Required', trigger: 'change' }],
  warning_level: [{ required: true, message: 'Please select alarm level', trigger: 'change' }],
  operator: [{ required: true, message: 'Please select operator', trigger: 'change' }],
  value: [{ required: true, message: 'Please input value', trigger: 'blur' }],
}

const mode = ref<'create' | 'edit'>('create')
const dialogTitle = computed(() => (mode.value === 'edit' ? 'Edit Rule' : 'New Rule'))

// 加载通道列表
const loadChannels = async () => {
  try {
    loadingChannels.value = true
    const res = await getAllChannels()
    const list = Array.isArray(res?.data?.list)
      ? res.data.list
      : Array.isArray(res?.data)
        ? res.data
        : Array.isArray(res)
          ? (res as any)
          : []
    channelList.value = (list as any[])
      .map((it: any) => ({
        label: String(it.name || `Channel ${it.id}`),
        value: Number(it.id),
      }))
      .filter((x) => Number.isFinite(x.value) && x.value > 0)
  } catch (error) {
    console.error('Failed to load channels:', error)
    channelList.value = []
  } finally {
    loadingChannels.value = false
  }
}

// 加载所有点位（T和S类型）
const loadAllPoints = async (channelId: number) => {
  if (!channelId) {
    allPointsData.value = []
    return
  }
  try {
    loadingPoints.value = true
    // 不传type参数，获取所有类型的点位
    const res = await getPointsTables(channelId)

    const allPoints: Array<{ point_id: number; signal_name: string; point_type: PointType }> = []

    if (res.success && res.data) {
      // 处理T类型点位（telemetry）
      const tPointsData = Array.isArray(res.data.telemetry) ? res.data.telemetry : []
      allPoints.push(
        ...tPointsData.map((point: any) => ({
          point_id: Number(point.point_id),
          signal_name: String(point.signal_name || `Point ${point.point_id}`),
          point_type: 'T' as PointType,
        })),
      )

      // 处理S类型点位（signal）
      const sPointsData = Array.isArray(res.data.signal) ? res.data.signal : []
      allPoints.push(
        ...sPointsData.map((point: any) => ({
          point_id: Number(point.point_id),
          signal_name: String(point.signal_name || `Point ${point.point_id}`),
          point_type: 'S' as PointType,
        })),
      )
    }

    allPointsData.value = allPoints
  } catch (error) {
    console.error('Failed to load points:', error)
    allPointsData.value = []
  } finally {
    loadingPoints.value = false
  }
}

// 处理通道变化
const handleChannelChange = async () => {
  form.value.data_type = null
  form.value.point_id = null
  allPointsData.value = []
  // 选择通道后立即加载所有点位
  if (form.value.channel_id) {
    await loadAllPoints(Number(form.value.channel_id))
  }
}

// 处理点位类型变化
const handlePointTypeChange = () => {
  form.value.point_id = null
  // 点位列表会根据computed自动筛选，无需额外操作
}

const rules_id = ref<string>()
async function open(rulesId?: string, openMode: 'create' | 'edit' = 'create') {
  try {
    mode.value = openMode
    form.value = getDefaultForm()
    rules_id.value = rulesId || ''

    // 加载通道列表
    await loadChannels()

    if (rulesId) {
      const res = await getRuleDetail(rules_id.value)
      if (res.success && res.data.list?.length) {
        const rule = res.data.list[0]
        form.value.rule_name = rule.rule_name
        form.value.service_type = 'comsrv'
        form.value.channel_id = rule.channel_id
        form.value.point_id = rule.point_id
        form.value.data_type = rule.data_type as 'T' | 'S'
        form.value.warning_level = rule.warning_level
        form.value.operator = rule.operator as Operator
        form.value.value = rule.value
        form.value.description = rule.description || ''
        form.value.enabled = rule.enabled

        // 如果有通道，加载所有点位
        if (form.value.channel_id) {
          await loadAllPoints(Number(form.value.channel_id))
        }
      }
    }
    nextTick(() => {
      setTimeout(() => {
        formRef.value?.clearValidate()
      }, 100)
    })
    dialogRef.value && (dialogRef.value.dialogVisible = true)
  } catch (error) {
    console.error(error)
  }
}

function close() {
  dialogRef.value && (dialogRef.value.dialogVisible = false)
}

const emit = defineEmits<{
  (e: 'submit', value: RuleFormModel): void
  (e: 'cancel'): void
}>()

function onCancel() {
  close()
  emit('cancel')
}

async function onSubmit() {
  formRef.value?.validate(async (valid) => {
    if (!valid) return
    // 确保 service_type 固定为 comsrv
    const submitData = { ...form.value, service_type: 'comsrv' }
    if (mode.value === 'create') {
      const res = await createRule(submitData)
      if (res.success) {
        emit('submit', form.value)
        close()
      } else {
        throw new Error(res.message)
      }
    } else {
      if (!rules_id.value) {
        throw new Error('rules_id is required')
      }
      const res = await updateRule(rules_id.value, submitData)
      if (res.success) {
        emit('submit', form.value)
        close()
      }
    }
  })
}

defineExpose({ open, close })
</script>

<style scoped lang="scss">
.rules-form {
  display: flex;
  flex-wrap: wrap;
  .monitor-data-group,
  .alarm-level-group,
  .condition-group {
    // width: 100%;
    position: relative;
    display: flex;
    gap: 0.16rem;
  }

  .rules-form__compact-item {
    margin-right: 0 !important;
  }

  .rules-form__full-row {
    width: 100%;
    margin-right: 0 !important;
  }

  :deep(.rules-form__value-input) {
    width: 4.96rem;
  }

  :deep(.el-switch) {
    height: 0.32rem;
  }

  :deep(.el-input__inner) {
    width: 2.4rem;
  }
}

// :deep(.el-select__popper.el-popper) {
//   top: 1.44rem !important;
// }

// // 为对话框内的下拉框设置更具体的样�?// :deep(.rules-form .el-select__popper.el-popper) {
//   top: 1.44rem !important;
// }

// // 使用自定义类名的下拉框样�?// :deep(.rules-dialog-popper) {
//   top: 1.44rem !important;
// }
</style>
