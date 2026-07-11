<template>
  <div class="aether-class rule-management" ref="ruleManagementRef">
    <LoadingBg :loading="loading">
      <div class="rule-management__header">
        <div class="rule-management__search-form" ref="levelSelectRef">
          <el-form :model="filters" :inline="true" class="test-form rule-management__toolbar-form">
            <el-form-item label="Keyword:">
              <el-input v-model="filters.keyword" placeholder="Please enter keyword" />
            </el-form-item>
            <el-form-item label="Alarm Level:">
              <el-select
                v-model="filters.warning_level"
                placeholder="Please select level"
                clearable
                :append-to="levelSelectRef"
              >
                <el-option label="Critical Alarm" :value="1" />
                <el-option label="Warning Alarm" :value="2" />
                <el-option label="Info Alarm" :value="3" />
              </el-select>
            </el-form-item>
            <el-form-item label="Enabled:">
              <el-select
                v-model="filters.enabled"
                placeholder="Please select enabled"
                clearable
                :append-to="levelSelectRef"
              >
                <el-option label="Enabled" :value="true" />
                <el-option label="Disabled" :value="false" />
              </el-select>
            </el-form-item>
          </el-form>
          <div class="form-oprations">
            <IconButton
              type="warning"
              :icon="tableRefreshIcon"
              text="Reload"
              custom-class="rule-management__btn"
              @click="reloadFilters"
            />
            <IconButton
              type="primary"
              :icon="tableSearchIcon"
              text="Search"
              custom-class="rule-management__btn"
              @click="fetchTableData(true)"
            />
            <IconButton
              v-permission="['Admin']"
              type="primary"
              :icon="userAddIcon"
              text="New rule"
              custom-class="rule-management__btn"
              @click="handleAddUser"
            />
          </div>
        </div>
      </div>
      <div class="rule-management__table">
        <el-table :data="tableData" class="rule-management__table-content" align="left">
          <!-- <el-table-column prop="id" label="ID" class-name="table-ellipsis" width="80" /> -->
          <el-table-column
            prop="rule_name"
            label="Rule Name"
            class-name="table-ellipsis"
            min-width="120"
          />
          <el-table-column prop="warning_level" label="Alarm Level">
            <template #default="{ row }">
              <span
                class="rule-management__table-level-text"
                :class="`alarm-level--${row.warning_level}`"
              >
                {{ warningLevelText[row.warning_level as 1 | 2 | 3] || '-' }}
              </span>
            </template>
          </el-table-column>
          <el-table-column
            prop="monitor_data"
            label="Monitor Data"
            class-name="table-ellipsis"
            min-width="100"
          >
            <template #default="{ row }">
              <span class="table-ellipsis__text">{{ formatMonitorData(row) }}</span>
            </template>
          </el-table-column>
          <el-table-column
            prop="condition"
            label="Condition"
            show-overflow-tooltip
            class-name="table-ellipsis"
            min-width="80"
          >
            <template #default="{ row }">
              <span class="table-ellipsis__text">{{ formatCondition(row) }}</span>
            </template>
          </el-table-column>
          <!-- <el-table-column prop="notification" label="Notification" show-overflow-tooltip>
          <template #default="{ row }">
            {{ Array.isArray(row.notification) ? row.notification.join(', ') : row.notification }}
          </template>
        </el-table-column> -->
          <el-table-column
            prop="description"
            label="Description"
            show-overflow-tooltip
            class-name="table-ellipsis"
            min-width="120"
          >
            <template #default="{ row }">
              <span class="table-ellipsis__text">{{ row.description || '-' }}</span>
            </template>
          </el-table-column>
          <el-table-column
            prop="created_at"
            label="Created At"
            class-name="table-ellipsis"
            min-width="120"
          >
            <template #default="{ row }">
              <span class="table-ellipsis__text">{{ formatDateTime(row.created_at) }}</span>
            </template>
          </el-table-column>
          <el-table-column prop="enabled" label="Enabled" min-width="80">
            <template #default="{ row }">
              <el-switch
                :model-value="row.enabled"
                :loading="switchLoadingId === row.id"
                @change="handleSwitchChange(row)"
              />
            </template>
          </el-table-column>
          <el-table-column label="Operation" fixed="right" v-permission="['Admin']" min-width="120">
            <template #default="{ row }">
              <div class="rule-management__operation">
                <div class="rule-management__operation-item" @click="handleEdit(row)">
                  <img :src="tableEditIcon" />
                  <span class="rule-management__operation-text">Edit</span>
                </div>
                <div class="rule-management__operation-item" @click="handleDelete(row)">
                  <img :src="tableDeleteIcon" />
                  <span class="rule-management__operation-text">Delete</span>
                </div>
              </div>
            </template>
          </el-table-column>
        </el-table>

        <div class="rule-management__pagination">
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
    <RulesOperationForm
      ref="rulesOperationFormRef"
      @submit="fetchTableData(true)"
      @cancel="handleRuleCancel"
    />
  </div>
</template>

<script setup lang="ts">
// 正确引入SVG图标，避免部署后图片加载不出�?
import tableRefreshIcon from '@/assets/icons/table-refresh.svg'
import tableSearchIcon from '@/assets/icons/table-search.svg'
import userAddIcon from '@/assets/icons/user-add.svg'
import tableEditIcon from '@/assets/icons/table-edit.svg'
import tableDeleteIcon from '@/assets/icons/table-delect.svg'
import RulesOperationForm from './RulesOperationForm.vue'
import type { RuleInfo } from '@/types/ruleManagement'

import { useTableData, type TableConfig } from '@/composables/useTableData'
import { enableRule, disableRule } from '@/api/alarm'

const ruleManagementRef = ref<HTMLElement | null>(null)
const tableConfig: TableConfig = {
  listUrl: '/alarmApi/rules',
  deleteUrl: '/alarmApi/rules/{id}',
  defaultPageSize: 20,
}
const warningLevelText = {
  1: 'Critical Alarm',
  2: 'Warning Alarm',
  3: 'Info Alarm',
}
const {
  loading,
  tableData,
  pagination,
  handlePageSizeChange,
  fetchTableData,
  filters,
  handlePageChange,
  reloadFilters,
  deleteRow,
} = useTableData<RuleInfo>(tableConfig)

filters.keyword = ''
filters.warning_level = null
filters.enabled = null

const levelSelectRef = ref<HTMLElement | null>(null)

const rulesOperationFormRef = ref()
const switchLoadingId = ref<string | number | null>(null)

// 格式�?MonitorData
const formatMonitorData = (row: RuleInfo) => {
  if (!row) return '-'
  return [row.service_type || 'comsrv', row.channel_id, row.data_type, row.point_id]
    .filter((v) => v !== null && v !== undefined && v !== '')
    .join(' / ')
}

const formatCondition = (row: RuleInfo) => {
  if (!row || !row.operator || row.value === null || row.value === undefined) return '-'
  return `${row.operator} ${row.value}`
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

// 添加规则
const handleAddUser = () => {
  rulesOperationFormRef.value?.open(undefined, 'create')
}

// 编辑规则
const handleEdit = (row: RuleInfo) => {
  rulesOperationFormRef.value?.open(row.id, 'edit')
}

// 删除规则
const handleDelete = async (row: RuleInfo) => {
  deleteRow(
    row.id,
    `Are you sure you want to delete rule "${row.rule_name}"?`,
    ruleManagementRef.value,
  )
}
const handleSwitchChange = async (row: RuleInfo) => {
  switchLoadingId.value = row.id
  try {
    if (!row.enabled) {
      const res = await enableRule(row.id)
      if (res.message) {
        row.enabled = true
      }
    } else {
      const res = await disableRule(row.id)
      if (res.message) {
        row.enabled = false
      }
    }
  } finally {
    switchLoadingId.value = null
  }
}

// 处理规则表单取消
const handleRuleCancel = () => {
  console.log('Rule form cancelled')
}
</script>

<style scoped lang="scss">
.aether-class.rule-management {
  position: relative;
  height: 100%;
  width: 100%;
  display: flex;
  flex-direction: column;

  .rule-management__header {
    // margin-bottom: 0.2rem;

    .rule-management__search-form {
      position: relative;
      display: flex;
      align-items: center;
      justify-content: space-between;
      padding-bottom: 0.2rem;

      .form-oprations {
        display: flex;
        align-items: flex-start;
        gap: 0.1rem;
      }
    }

    .rule-management__btn {
      display: flex;
      align-items: center;
      gap: 0.08rem;

      .rule-management__btn-icon {
        width: 0.14rem;
        height: 0.14rem;
        margin-right: 0.08rem;
      }
    }
  }

  .rule-management__table {
    height: calc(100% - 0.52rem);
    // max-width: 16.6rem;
    display: flex;
    flex-direction: column;

    .rule-management__table-content {
      height: calc(100% - 0.92rem);
      overflow-y: auto;

      .rule-management__operation {
        display: flex;
        align-items: center;
        gap: 0.2rem;

        .rule-management__operation-item {
          cursor: pointer;
          display: flex;
          align-items: center;

          img {
            width: 0.14rem;
            height: 0.14rem;
            margin-right: 0.04rem;
            object-fit: contain;
          }
        }
      }

      .rule-management__table-level-text {
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }
    }

    .rule-management__pagination {
      display: flex;
      justify-content: flex-end;
      margin: 0.2rem 0;
    }
  }

  :deep(.rule-management__table-content .el-switch) {
    height: 0.22rem;
  }

  :deep(.rule-management__toolbar-form.el-form--inline .el-form-item) {
    margin-bottom: 0;
  }

  :deep(.rule-management__table-content .table-ellipsis .cell) {
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
