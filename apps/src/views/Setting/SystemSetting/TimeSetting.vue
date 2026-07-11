<template>
  <div class="aether-class time-setting">
    <ModuleCard title="Time Setting:" :isShowFooter="true">
      <el-form
        ref="formRef"
        :model="formData"
        class="time-setting-form"
        label-width="1.35rem"
        label-position="right"
        :rules="rules"
      >
        <el-form-item label="Time Zone:" prop="timeZone">
          <div class="time-setting-form-item" ref="timeSettingFormItemRef">
            <el-select
              v-model="formData.timeZone"
              placeholder="Please select"
              :append-to="timeSettingFormItemRef"
              class="time-setting-form__full-field"
            >
              <el-option label="GMT+8" value="GMT+8" />
              <el-option label="UTC-0" value="UTC-0" />
            </el-select>
          </div>
        </el-form-item>
        <el-form-item label="Synchronization:" prop="Synchronization">
          <el-radio-group v-model="formData.synchronization">
            <el-radio label="auto">auto</el-radio>
            <el-radio label="manual">Manual</el-radio>
          </el-radio-group>
        </el-form-item>
      </el-form>
      <template #footer>
        <div class="card__content-footer">
          <el-button type="primary">Submit</el-button>
        </div>
      </template>
    </ModuleCard>
  </div>
</template>

<script setup lang="ts">
import type { FormInstance } from 'element-plus'

const formRef = ref<FormInstance>()
const formData = ref({
  timeZone: 'GMT+8',
  synchronization: 'auto',
})
const timeSettingFormItemRef = ref<HTMLElement | null>(null)
const rules = ref({
  timeZone: [{ required: true, message: 'Please select time zone', trigger: 'change' }],
  synchronization: [
    { required: true, message: 'Please select synchronization', trigger: 'change' },
  ],
})
</script>

<style scoped lang="scss">
.time-setting {
  width: 100%;
  height: 100%;

  .time-setting-form {
    width: 100%;
    height: 100%;
    padding: 0.2rem 0;

    .time-setting-form-item {
      width: 100%;
      height: 0.32rem;
      display: flex;
      align-items: center;
      justify-content: center;
    }

    :deep(.time-setting-form__full-field) {
      width: 100%;
    }
  }

  .card__content-footer {
    display: flex;
    padding: 0.3rem 0 0.1rem 0;
    width: 100%;
    justify-content: flex-end;
    align-items: center;
  }
}

:deep(.el-popper.is-light) {
  background-color: rgba(41, 60, 100, 0.9);
}
</style>
