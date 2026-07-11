<template>
  <div class="aether-class user-management" ref="userManagementRef">
    <div class="user-management__header">
      <IconButton
        type="primary"
        :icon="userAddIcon"
        text="New user"
        custom-class="user-management__add-btn"
        @click="handleAddUser"
      />
    </div>

    <!-- 用户操作表单 -->
    <UserOperationForm ref="userFormRef" @submit="handleUserSubmit" />
    <div class="user-management__table">
      <LoadingBg :loading="loading">
        <el-table
          :data="tableData"
          class="user-management__table-content"
          table-layout="auto"
          align="left"
        >
          <el-table-column prop="id" label="ID" />
          <el-table-column prop="username" label="UserName">
            <template #default="{ row }">
              <div class="user-info">
                <div class="user-avatar">
                  <div class="user-avatar-text">{{ getAvatarName(row.username) }}</div>
                </div>
                <div class="user-name">{{ row.username }}</div>
              </div>
            </template>
          </el-table-column>

          <!-- 角色列改为纯文字类型 -->
          <el-table-column prop="role" label="Role">
            <template #default="{ row }">
              {{ row.role.name_en }}
            </template>
          </el-table-column>
          <el-table-column prop="status" label="Enabled">
            <template #default="{ row }">
              <span :class="row.is_active ? 'status-enabled' : 'status-disabled'">
                {{ row.is_active ? 'Enable' : 'Disabled' }}
              </span>
            </template>
          </el-table-column>
          <el-table-column prop="last_login" label="Last Login">
            <template #default="{ row }">
              {{ row.last_login }}
            </template>
          </el-table-column>
          <el-table-column prop="created_at" label="Created At">
            <template #default="{ row }">
              {{ row.created_at }}
            </template>
          </el-table-column>
          <el-table-column
            label="Operation"
            fixed="right"
            class-name="user-management__operation-column"
          >
            <template #default="{ row }">
              <div class="user-management__operation">
                <div class="user-management__operation-item" @click="handleEdit(row)">
                  <img :src="tableEditIcon" />
                  <span class="user-management__operation-text">Edit</span>
                </div>
                <div class="user-management__operation-item" @click="handleDelete(row)">
                  <img :src="tableDeleteIcon" />
                  <span class="user-management__operation-text">Delete</span>
                </div>
              </div>
            </template>
          </el-table-column>
        </el-table>

        <div class="user-management__pagination">
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
      </LoadingBg>
    </div>
  </div>
</template>

<script setup lang="ts">
// 正确引入SVG图标，避免部署后图片加载不出�?
import userAddIcon from '@/assets/icons/user-add.svg'
import tableEditIcon from '@/assets/icons/table-edit.svg'
import tableDeleteIcon from '@/assets/icons/table-delect.svg'

import type { UserManagementInfo } from '@/types/user'
import { useTableData, type TableConfig } from '@/composables/useTableData'
import UserOperationForm from './UserOperationForm.vue'

const userManagementRef = ref<HTMLElement | null>(null)
// 表格配置
const tableConfig: TableConfig = {
  listUrl: '/api/v1/auth/users',
  deleteUrl: '/api/v1/auth/users/{id}',
  defaultPageSize: 20,
}

// 使用表格数据管理 composable
const {
  loading,
  tableData,
  pagination: paginationData,
  handlePageSizeChange,
  fetchTableData,
  deleteRow,
  handlePageChange,
} = useTableData<UserManagementInfo>(tableConfig)

// 创建可写的分页数�?
const pagination = reactive({
  page: 1,
  pageSize: 20,
  total: 0,
})

// 同步分页数据
watch(
  paginationData,
  (newPagination) => {
    pagination.page = newPagination.page
    pagination.pageSize = newPagination.pageSize
    pagination.total = newPagination.total
  },
  { immediate: true },
)

// 表单引用
const userFormRef = ref()

// 添加用户
const handleAddUser = () => {
  userFormRef.value?.open()
}

// 处理用户表单提交
const handleUserSubmit = async (formData: any) => {
  await fetchTableData(true)
}

// 编辑用户
const handleEdit = (row: UserManagementInfo) => {
  userFormRef.value?.open(row.id, 'edit')
}

// 删除用户
const handleDelete = (row: UserManagementInfo) => {
  // ElMessageBox.confirm('Are you sure you want to delete this record?', 'Delete Confirmation', {
  //   confirmButtonText: 'Confirm',
  //   cancelButtonText: 'Cancel',
  //   type: 'warning',
  // }).then(async () => {
  deleteRow(row.id, 'Are you sure you want to delete this record?', userManagementRef.value)
  // })
}
/**
 * 获取用户名头像缩写：名和姓的首字母（如"张三"->"张三"，"John Smith"->"JS"，"王"->"王"）
 * @param name 用户名字符串
 * @returns string 头像缩写
 */
const getAvatarName = (name: string): string => {
  const nameStr = name.split(' ')
  if (nameStr.length === 1) {
    return name.charAt(0).toUpperCase()
  } else {
    return nameStr[0].charAt(0).toUpperCase() + nameStr[1].charAt(0).toUpperCase()
  }
}
</script>

<style scoped lang="scss">
.aether-class.user-management {
  position: relative;
  width: 100%;
  height: 100%;
  display: flex;
  flex-direction: column;

  .user-management__header {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    margin-bottom: 0.2rem;

    .user-management__add-btn {
      display: flex;
      align-items: center;
      gap: 0.08rem;

      .user-management__add-btn-icon {
        width: 0.14rem;
        height: 0.14rem;
        margin-right: 0.08rem;
      }
    }
  }

  .user-management__table {
    height: calc(100% - 0.52rem);

    .user-info {
      display: flex;
      align-items: center;
      gap: 0.1rem;
      height: 0.4rem;

      .user-avatar {
        width: 0.4rem;
        height: 0.4rem;
        border-radius: 50%;
        background-color: rgba(29, 134, 255, 0.2);
        display: flex;
        align-items: center;
        justify-content: center;
        font-size: 0.19rem;
        letter-spacing: 0%;
        color: rgba(29, 134, 255, 1);
      }

      .user-name {
        font-size: 0.16rem;
        letter-spacing: 0%;
        color: rgba(255, 255, 255, 1);
      }
    }

    .user-management__table-content {
      width: 100%;
      height: calc(100% - 0.92rem);
      overflow-y: auto;

      .user-management__operation {
        display: flex;
        align-items: center;
        gap: 0.2rem;

        .user-management__operation-item {
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
    }

    .user-management__pagination {
      display: flex;
      justify-content: flex-end;
      margin: 0.2rem 0;
    }
  }
}

:deep(.user-management__table-content .el-table__body-wrapper td .cell) {
  height: 0.4rem;
}
.user-management__operation-column {
  width: 1.2rem;
}

.status-enabled {
  color: #67c23a;
  font-weight: 500;
}

.status-disabled {
  color: #909399;
  font-weight: 500;
}
</style>
