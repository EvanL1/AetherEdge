<template>
  <div class="aether-class alarm-records">
    <LoadingBg :loading="loading">
      <!-- 表格工具�?-->
      <div class="alarm-records__toolbar">
        <div class="alarm-records__toolbar-left" ref="toolbarLeftRef">
          <el-form :model="filters" inline class="alarm-records__toolbar-form">
            <el-form-item label="Alarm Level:">
              <el-select
                v-model="filters.warning_level"
                :append-to="toolbarLeftRef"
                clearable
                placeholder="Please select alarm level"
              >
                <el-option label="Critical Alarm" :value="1" />
                <el-option label="Warning Alarm" :value="2" />
                <el-option label="Info Alarm" :value="3" />
              </el-select>
            </el-form-item>
            <el-form-item label="Start Time:">
              <el-date-picker
                v-model="startTimeDisplay"
                type="datetime"
                placeholder="Please select start time"
                format="YYYY-MM-DD HH:mm:ss"
                :disabled-date="disableStartDate"
                :disabled-time="disableStartTime"
                @change="handleStartTimeChange"
                :teleported="false"
                clearable
              />
            </el-form-item>
            <el-form-item label="End Time:">
              <el-date-picker
                v-model="endTimeDisplay"
                type="datetime"
                placeholder="Please select end time"
                format="YYYY-MM-DD HH:mm:ss"
                :disabled-date="disableEndDate"
                :disabled-time="disableEndTime"
                @change="handleEndTimeChange"
                :teleported="false"
                clearable
              />
            </el-form-item>
          </el-form>
        </div>

        <div class="alarm-records__toolbar-right">
          <IconButton
            type="warning"
            :icon="reloadIcon"
            text="Reload"
            custom-class="alarm-records__export-btn"
            @click="reloadFilters"
          />
          <IconButton
            type="primary"
            :icon="searchIcon"
            text="Search"
            custom-class="alarm-records__export-btn"
            @click="fetchTableData(true)"
          />
          <IconButton
            type="primary"
            :icon="alarmExportIcon"
            text="Export"
            custom-class="alarm-records__export-btn"
            @click="exportData(`Alarm_History_${Date.now().toString()}.csv`)"
          />
        </div>
      </div>

      <!-- 表格 -->
      <div class="alarm-records__table">
        <el-table :data="tableData" class="alarm-records__table-content">
          <el-table-column
            prop="rule_name"
            label="Name"
            min-width="1.2rem"
            class-name="table-ellipsis"
          />
          <el-table-column
            prop="channel_id"
            label="Channel ID"
            min-width="1.2rem"
            class-name="table-ellipsis"
          />
          <el-table-column prop="warning_level" label="Level" min-width="1rem">
            <template #default="scope">
              <span
                class="alarm-records__table-level-text"
                :class="`alarm-level--${scope.row.warning_level}`"
              >
                {{ warningLevelText[scope.row.warning_level as 1 | 2 | 3] || '-' }}
              </span>
            </template>
          </el-table-column>
          <el-table-column
            prop="triggered_at"
            label="Start Time"
            min-width="1.6rem"
            class-name="table-ellipsis"
          >
            <template #default="{ row }">
              <span class="table-ellipsis__text">{{ formatDateTime(row.triggered_at) }}</span>
            </template>
          </el-table-column>
          <el-table-column
            prop="recovered_at"
            label="End Time"
            min-width="1.6rem"
            class-name="table-ellipsis"
          >
            <template #default="{ row }">
              <span class="table-ellipsis__text">{{ formatDateTime(row.recovered_at) }}</span>
            </template>
          </el-table-column>
        </el-table>

        <!-- 分页组件 -->
        <div class="alarm-records__pagination">
          <el-pagination
            v-model:current-page="pagination.page"
            v-model:page-size="pagination.pageSize"
            :page-sizes="[10, 20, 50, 100]"
            :total="pagination.total"
            layout="total, sizes, prev, pager, next"
            @size-change="handlePageSizeChange"
            @current-change="handlePageChange"
          />
        </div>
      </div>
    </LoadingBg>
  </div>
</template>

<script setup lang="ts">
import type { HistoryAlarmData } from '@/types/alarm'
import { useTableData } from '@/composables/useTableData'

import alarmExportIcon from '@/assets/icons/alarm-export.svg'
import searchIcon from '@/assets/icons/table-search.svg'
import reloadIcon from '@/assets/icons/table-refresh.svg'
const toolbarLeftRef = ref<HTMLElement | null>(null)
const warningLevelText = {
  1: 'Critical Alarm',
  2: 'Warning Alarm',
  3: 'Info Alarm',
}

// 日期选择器显示用的 Date 对象（与 filters 中的 Unix 时间戳分开）
const startTimeDisplay = ref<Date | null>(null)
const endTimeDisplay = ref<Date | null>(null)

// 使用 useTableData composable
const {
  loading,
  tableData,
  pagination,
  handlePageSizeChange,
  handlePageChange,
  fetchTableData,
  filters,
  exportData,
  reloadFilters: _reloadFilters,
} = useTableData<HistoryAlarmData>({
  listUrl: '/alarmApi/alert-events',
  exportUrl: '/alarmApi/alert-events/export',
  enableExport: true,
  defaultPageSize: 20,
})

// 重置时同步清空日期选择器显示值
const reloadFilters = () => {
  startTimeDisplay.value = null
  endTimeDisplay.value = null
  _reloadFilters()
}

// 初始化filters
filters.warning_level = null
filters.start_time = null
filters.end_time = null

// 处理开始时间变化
const handleStartTimeChange = (value: Date | null) => {
  startTimeDisplay.value = value
  // 记录原始 Date 以便禁用规则计算
  filters.startTime = value || null
  // 如果开始时间晚于或等于结束时间，清空结束时间
  if (value && filters.endTime && value.getTime() >= new Date(filters.endTime).getTime()) {
    filters.endTime = null
    filters.end_time = null
    endTimeDisplay.value = null
  }
  // 转为后端需要的 Unix 秒时间戳
  filters.start_time = value ? Math.floor(value.getTime() / 1000) : null
}

// 处理结束时间变化
const handleEndTimeChange = (value: Date | null) => {
  // 记录原始 Date 以便禁用规则计算
  const adjusted: Date | null = value ? new Date(value) : null
  // 若时间未指定（00:00:00），默认设置到当天 23:59:59
  if (
    adjusted &&
    adjusted.getHours() === 0 &&
    adjusted.getMinutes() === 0 &&
    adjusted.getSeconds() === 0
  ) {
    adjusted.setHours(23, 59, 59, 999)
  }
  endTimeDisplay.value = adjusted
  filters.endTime = adjusted || null
  // 如果结束时间早于或等于开始时间，清空开始时间
  if (
    adjusted &&
    filters.startTime &&
    adjusted.getTime() <= new Date(filters.startTime).getTime()
  ) {
    filters.startTime = null
    filters.start_time = null
    startTimeDisplay.value = null
  }
  // 转为后端需要的 Unix 秒时间戳
  filters.end_time = adjusted ? Math.floor(adjusted.getTime() / 1000) : null
}

// 禁用开始时间的日期选择
const disableStartDate = (time: Date) => {
  if (!filters.endTime) return false
  // 开始日期不得晚于结束日期（同日允许，具体时间由 disableStartTime 控制）
  return time.getTime() > new Date(filters.endTime).getTime()
}

// 禁用开始时间的时间选择
const disableStartTime = (date: Date, type: string) => {
  if (!filters.endTime || type !== 'minute') return {}
  const endTime = new Date(filters.endTime)
  if (date.getDate() === endTime.getDate()) {
    return {
      disabledHours: () =>
        Array.from({ length: 24 }, (_, i) => i).filter((h) => h > endTime.getHours()),
      disabledMinutes: () =>
        Array.from({ length: 60 }, (_, i) => i).filter((m) => m > endTime.getMinutes()),
    }
  }
  return {}
}

// 禁用结束时间的日期选择
const disableEndDate = (time: Date) => {
  if (!filters.startTime) return false
  // 结束日期不得早于开始日期（同日允许，具体时间由 disableEndTime 控制）
  return time.getTime() < new Date(filters.startTime).getTime()
}

// 禁用结束时间的时间选择
const disableEndTime = (date: Date, type: string) => {
  if (!filters.startTime || type !== 'minute') return {}
  const startTime = new Date(filters.startTime)
  if (date.getDate() === startTime.getDate()) {
    return {
      disabledHours: () =>
        Array.from({ length: 24 }, (_, i) => i).filter((h) => h < startTime.getHours()),
      disabledMinutes: () =>
        Array.from({ length: 60 }, (_, i) => i).filter((m) => m < startTime.getMinutes()),
    }
  }
  return {}
}

// 格式化时间（支持 Unix 秒时间戳和日期字符串）
const formatDateTime = (dateTime: number | string | null | undefined): string => {
  if (dateTime === null || dateTime === undefined || dateTime === '') return '-'
  try {
    // Unix 时间戳为秒，需转换为毫秒
    const date = typeof dateTime === 'number' ? new Date(dateTime * 1000) : new Date(dateTime)
    if (isNaN(date.getTime())) return String(dateTime)
    const year = date.getFullYear()
    const month = String(date.getMonth() + 1).padStart(2, '0')
    const day = String(date.getDate()).padStart(2, '0')
    const hours = String(date.getHours()).padStart(2, '0')
    const minutes = String(date.getMinutes()).padStart(2, '0')
    const seconds = String(date.getSeconds()).padStart(2, '0')
    return `${year}-${month}-${day} ${hours}:${minutes}:${seconds}`
  } catch {
    return String(dateTime)
  }
}

// 处理导出
</script>

<style scoped lang="scss">
.aether-class.alarm-records {
  position: relative;
  height: 100%;
  display: flex;
  flex-direction: column;

  .alarm-records__toolbar {
    padding-bottom: 0.2rem;
    display: flex;
    align-items: center;
    justify-content: space-between;

    .alarm-records__toolbar-left {
      position: relative;
      display: flex;
      align-items: center;
      gap: 0.16rem;
    }

    .alarm-records__toolbar-right {
      display: flex;
      align-items: center;
      gap: 0.1rem;

      .alarm-records__export-btn {
        display: flex;
        align-items: center;
        gap: 0.1rem;

        .alarm-records__export-icon {
          width: 0.16rem;
          height: 0.16rem;
          margin-right: 0.08rem;
        }
      }
    }
  }

  .alarm-records__table {
    height: calc(100% - 0.52rem);
    width: 100%;
    display: flex;
    flex-direction: column;

    .alarm-records__table-content {
      width: 100%;
      height: calc(100% - 0.92rem);
      overflow-y: auto;

      .alarm-records__table-level-text {
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }
    }

    .alarm-records__pagination {
      padding: 0.2rem 0;
      display: flex;
      justify-content: flex-end;
    }
  }

  :deep(.alarm-records__toolbar-form.el-form--inline .el-form-item) {
    margin-bottom: 0;
  }

  :deep(.alarm-records__table-content .table-ellipsis .cell) {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .table-ellipsis__text {
    display: block;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .alarm-level--1 {
    color: #da2d2c;
  }

  .alarm-level--2 {
    color: #ff6e08;
  }

  .alarm-level--3 {
    color: #fe9900;
  }
}
</style>
