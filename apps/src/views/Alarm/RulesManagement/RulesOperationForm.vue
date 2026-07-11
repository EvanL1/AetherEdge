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

        <!-- Trigger Type -->
        <el-form-item label="Trigger:" class="rules-form__full-row">
          <el-radio-group v-model="triggerType">
            <el-radio value="interval">定时触发 (Interval)</el-radio>
            <el-radio value="on_change">变化触发 (OnChange)</el-radio>
          </el-radio-group>
        </el-form-item>

        <!-- Interval config -->
        <el-form-item v-if="triggerType === 'interval'" label="Interval (ms):" class="rules-form__full-row">
          <el-input-number
            v-model="intervalMs"
            :min="100"
            :max="86400000"
            :controls="false"
            placeholder="1000"
            class="rules-form__value-input"
            align="left"
          />
        </el-form-item>

        <!-- OnChange config -->
        <template v-if="triggerType === 'on_change'">
          <el-form-item label="时间死区 (ms):" class="rules-form__full-row">
            <el-input-number
              v-model="timeDead"
              :min="0"
              :max="60000"
              :controls="false"
              placeholder="200"
              class="rules-form__value-input"
              align="left"
            />
          </el-form-item>

          <!-- Point refs list -->
          <el-form-item label="监听点位:" class="rules-form__full-row">
            <div class="onchange-refs">
              <div
                v-for="(ref, idx) in onChangeRefs"
                :key="`${ref.instance}-${ref.point_type}-${ref.point}`"
                class="onchange-refs__row"
              >
                <span class="onchange-refs__label">{{ pointRefLabel(ref) }}</span>
                <el-button link type="danger" size="small" @click="removeOnChangeRef(idx)">删除</el-button>
              </div>
              <!-- Add row -->
              <div ref="triggerGroupRef" class="onchange-refs__add-row">
                <el-select
                  v-model="addRowInstance"
                  placeholder="Select Instance"
                  :loading="loadingInstances"
                  popper-class="rules-dialog-popper"
                  :append-to="triggerGroupRef"
                  @change="handleAddRowInstanceChange"
                  style="width: 5.5rem"
                >
                  <el-option
                    v-for="inst in instanceList"
                    :key="inst.value"
                    :label="inst.label"
                    :value="inst.value"
                  />
                </el-select>
                <el-select
                  v-model="addRowPoints"
                  placeholder="Select Points"
                  multiple
                  collapse-tags
                  collapse-tags-tooltip
                  :disabled="!addRowInstance"
                  popper-class="rules-dialog-popper"
                  :append-to="triggerGroupRef"
                  style="width: 6rem"
                >
                  <el-option
                    v-for="pt in addRowInstancePoints"
                    :key="pt.value"
                    :label="pt.label"
                    :value="pt.value"
                  />
                </el-select>
                <el-button size="small" @click="addOnChangeRef">添加</el-button>
              </div>
            </div>
          </el-form-item>

          <!-- Advanced (value_deadband) — hidden behind disclosure -->
          <el-form-item class="rules-form__full-row">
            <el-link type="info" @click="showAdvanced = !showAdvanced" :underline="false">
              {{ showAdvanced ? '▲ 收起高级选项' : '▼ 高级选项' }}
            </el-link>
            <div v-if="showAdvanced" class="onchange-advanced">
              <span class="onchange-advanced__hint">value_deadband: null（默认，任意值变化即触发）</span>
            </div>
          </el-form-item>
        </template>

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
import { getAllInstances, getInstancePoints } from '@/api/devicesManagement'
import type { RuleFormModel, DialogExpose, Operator, PointRef } from '@/types/ruleManagement'
import type { PointType } from '@/types/channelConfiguration'

const formRef = ref<FormInstance>()
const dialogRef = ref<DialogExpose>()
const monitorDataGroupRef = ref<HTMLElement>()
const alarmLevelGroupRef = ref<HTMLElement>()
const conditionGroupRef = ref<HTMLElement>()
const triggerGroupRef = ref<HTMLElement>()

// ── Trigger type state ────────────────────────────────────────────────────────
type TriggerType = 'interval' | 'on_change'
const triggerType = ref<TriggerType>('interval')
const intervalMs = ref<number>(1000)
const timeDead = ref<number>(200)
const showAdvanced = ref(false)

// OnChange point refs — each item is {instance, point_type, point}
const onChangeRefs = ref<PointRef[]>([])

// instance list for OnChange picker
const instanceList = ref<Array<{ label: string; value: number }>>([])
const loadingInstances = ref(false)

// per-instance points cache
const instancePointsCache = ref<
  Record<number, Array<{ label: string; value: string; point_type: 'measurement' | 'action' }>>
>({})

const loadInstances = async () => {
  if (instanceList.value.length > 0) return
  try {
    loadingInstances.value = true
    const res = await getAllInstances()
    const list = Array.isArray(res?.data?.list)
      ? res.data.list
      : Array.isArray(res?.data)
        ? res.data
        : []
    instanceList.value = (list as any[]).map((it: any) => ({
      label: String(it.name || it.instance_name || `Instance ${it.instance_id}`),
      value: Number(it.instance_id),
    }))
  } catch {
    instanceList.value = []
  } finally {
    loadingInstances.value = false
  }
}

const loadInstancePoints = async (instanceId: number) => {
  if (instancePointsCache.value[instanceId]) return
  try {
    const res = await getInstancePoints(instanceId)
    if (res.success && res.data) {
      const pts: Array<{ label: string; value: string; point_type: 'measurement' | 'action' }> = []
      const m = res.data.measurements || {}
      const a = res.data.actions || {}
      for (const [key, pt] of Object.entries(m as Record<string, any>)) {
        pts.push({ label: `M: ${pt.name || key}`, value: key, point_type: 'measurement' })
      }
      for (const [key, pt] of Object.entries(a as Record<string, any>)) {
        pts.push({ label: `A: ${pt.name || key}`, value: key, point_type: 'action' })
      }
      instancePointsCache.value[instanceId] = pts
    }
  } catch {
    // ignore
  }
}

// Encode a PointRef as a unique string key for el-select multiple value
const encodeRef = (ref: PointRef) => `${ref.instance}:${ref.point_type}:${ref.point}`

// Selected instance for the OnChange add-row UI
const addRowInstance = ref<number | null>(null)
const addRowPoints = ref<string[]>([])

const addRowInstancePoints = computed(() => {
  if (!addRowInstance.value) return []
  return instancePointsCache.value[addRowInstance.value] || []
})

const handleAddRowInstanceChange = async (instanceId: number) => {
  addRowPoints.value = []
  if (instanceId) await loadInstancePoints(instanceId)
}

const addOnChangeRef = () => {
  if (!addRowInstance.value || addRowPoints.value.length === 0) return
  for (const key of addRowPoints.value) {
    const ptDef = instancePointsCache.value[addRowInstance.value]?.find((p) => p.value === key)
    const pointIndex = parseInt(key, 10)
    if (isNaN(pointIndex)) continue
    const newRef: PointRef = {
      instance: addRowInstance.value,
      point_type: ptDef?.point_type ?? 'measurement',
      point: pointIndex,
    }
    // Avoid duplicates
    const exists = onChangeRefs.value.some(
      (r) =>
        r.instance === newRef.instance &&
        r.point_type === newRef.point_type &&
        r.point === newRef.point,
    )
    if (!exists) onChangeRefs.value.push(newRef)
  }
  addRowPoints.value = []
}

const removeOnChangeRef = (idx: number) => {
  onChangeRefs.value.splice(idx, 1)
}

const pointRefLabel = (ref: PointRef) => {
  const inst = instanceList.value.find((i) => i.value === ref.instance)
  const instLabel = inst?.label ?? `Instance ${ref.instance}`
  const pts = instancePointsCache.value[ref.instance]
  const ptDef = pts?.find((p) => p.value === String(ref.point))
  const ptLabel = ptDef?.label ?? `${ref.point_type[0].toUpperCase()}:${ref.point}`
  return `${instLabel} / ${ptLabel}`
}
// ─────────────────────────────────────────────────────────────────────────────

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

    // Reset trigger state
    triggerType.value = 'interval'
    intervalMs.value = 1000
    timeDead.value = 200
    onChangeRefs.value = []
    showAdvanced.value = false
    addRowInstance.value = null
    addRowPoints.value = []

    // 加载通道列表 + 实例列表
    await Promise.all([loadChannels(), loadInstances()])

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

        // Parse trigger_config from stored JSON string
        if (rule.trigger_config) {
          try {
            const tc = typeof rule.trigger_config === 'string'
              ? JSON.parse(rule.trigger_config)
              : rule.trigger_config
            if (tc?.type === 'on_change') {
              triggerType.value = 'on_change'
              onChangeRefs.value = Array.isArray(tc.point_refs) ? tc.point_refs : []
              timeDead.value = tc.time_deadband_ms ?? 200
              // Pre-load points for each referenced instance
              const instanceIds = [...new Set(onChangeRefs.value.map((r) => r.instance))]
              await Promise.all(instanceIds.map(loadInstancePoints))
            } else if (tc?.type === 'interval') {
              triggerType.value = 'interval'
              intervalMs.value = tc.interval_ms ?? 1000
            }
          } catch {
            // ignore parse errors — fallback to interval
          }
        }

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

    // Build trigger_config
    const trigger_config =
      triggerType.value === 'interval'
        ? { type: 'interval' as const, interval_ms: intervalMs.value }
        : {
            type: 'on_change' as const,
            point_refs: onChangeRefs.value,
            time_deadband_ms: timeDead.value ?? null,
            value_deadband: null,
          }

    // 确保 service_type 固定为 comsrv
    const submitData = { ...form.value, service_type: 'comsrv', trigger_config }
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

.onchange-refs {
  display: flex;
  flex-direction: column;
  gap: 0.08rem;
  width: 100%;

  &__row {
    display: flex;
    align-items: center;
    gap: 0.12rem;
    padding: 0.04rem 0;
  }

  &__label {
    flex: 1;
    font-size: 0.14rem;
    color: var(--el-text-color-regular);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  &__add-row {
    position: relative;
    display: flex;
    align-items: center;
    gap: 0.08rem;
    margin-top: 0.08rem;
  }
}

.onchange-advanced {
  margin-top: 0.08rem;

  &__hint {
    font-size: 0.12rem;
    color: var(--el-text-color-secondary);
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
