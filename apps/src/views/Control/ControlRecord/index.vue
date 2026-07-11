<template>
  <div class="aether-class alarm-records">
    <LoadingBg :loading="loading">
      <!-- 表格工具栏 -->
      <div class="alarm-records__toolbar">
        <div class="alarm-records__toolbar-left" ref="toolbarLeftRef">
          <el-form :model="filters" :inline="true" class="test-form alarm-records__toolbar-form">
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
                {{ levelTextList[scope.row.warning_level as 1 | 2 | 3] || '-' }}
              </span>
            </template>
          </el-table-column>
          <el-table-column
            prop="triggered_at"
            label="Start Time"
            min-width="1.2rem"
            class-name="table-ellipsis"
          >
            <template #default="{ row }">
              <span class="table-ellipsis__text">{{ formatDateTime(row.triggered_at) }}</span>
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
import type { CurrentAlarmData } from '@/types/alarm'
import { useTableData, type TableConfig } from '@/composables/useTableData'

import reloadIcon from '@/assets/icons/table-refresh.svg'
import searchIcon from '@/assets/icons/table-search.svg'
const levelTextList = {
  1: 'Critical Alarm',
  2: 'Warning Alarm',
  3: 'Info Alarm',
}
const toolbarLeftRef = ref<HTMLElement | null>(null)
// 表格配置
const tableConfig: TableConfig = {
  listUrl: '/alarmApi/alerts',
  defaultPageSize: 20,
}

// 使用 useTableData composable
const {
  loading,
  tableData,
  pagination,
  handlePageSizeChange,
  fetchTableData,
  filters,
  reloadFilters,
  handlePageChange,
} = useTableData<CurrentAlarmData>(tableConfig)

filters.warning_level = null

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
</script>

<style scoped lang="scss">
.aether-class.alarm-records {
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
        }
      }
    }
  }

  :deep(.alarm-records__toolbar-form.el-form--inline .el-form-item) {
    margin-bottom: 0;
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
