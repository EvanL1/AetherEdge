<template>
  <div class="aether-class operationLog">
    <!-- 表格区域 -->
    <div class="operationLog__table">
      <el-table :data="tableData" class="operationLog__table-content">
        <el-table-column prop="user" label="User" min-width="120" />
        <el-table-column prop="role" label="Role" min-width="100" />
        <el-table-column prop="action" label="action" min-width="120" />
        <el-table-column prop="device" label="Device" min-width="120" />
        <el-table-column prop="result" label="Result" min-width="100" />
        <el-table-column prop="time" label="Time" min-width="160" />
        <el-table-column prop="ip" label="IP Address" min-width="140" />
      </el-table>

      <!-- 分页组件 -->
      <div class="operationLog__pagination">
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
  </div>
</template>

<script setup lang="ts">
import type { OperationLogRecord } from '@/types/statistics'

// 临时停用接口请求，后续恢复时可直接取消注释
// import { useTableData, type TableConfig } from '@/composables/useTableData'
// const tableConfig: TableConfig = {
//   listUrl: '/api/operation-logs',
//   defaultPageSize: 20,
// }
// const {
//   loading,
//   tableData,
//   pagination: paginationData,
//   handlePageSizeChange,
//   handlePageChange,
// } = useTableData<OperationLogRecord>(tableConfig)

// 表格数据保持为空，不发送请求
const tableData = ref<OperationLogRecord[]>([])

// 本地分页状态
const pagination = reactive({
  page: 1,
  pageSize: 20,
  total: 0,
})

const handlePageSizeChange = (pageSize: number) => {
  pagination.pageSize = pageSize
  pagination.page = 1
  pagination.total = 0
  tableData.value = []
}

const handlePageChange = (page: number) => {
  pagination.page = page
  pagination.total = 0
  tableData.value = []
}
</script>

<style scoped lang="scss">
.aether-class.operationLog {
  height: 100%;
  width: 100%;
  display: flex;

  .operationLog__table {
    width: 100%;
    height: 100%;

    .operationLog__table-content {
      width: 100%;
      height: calc(100% - 0.92rem);
      overflow-y: auto;
    }

    .operationLog__pagination {
      padding: 0.2rem 0;
      display: flex;
      justify-content: flex-end;
    }
  }
}
</style>
