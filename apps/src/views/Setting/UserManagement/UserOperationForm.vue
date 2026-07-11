<template>
  <FormDialog width="9.24rem" ref="dialogRef" :title="dialogTitle">
    <template #dialog-body>
      <el-form
        ref="formRef"
        :model="form"
        :rules="rules"
        label-width="1.24rem"
        class="user-form"
        label-position="right"
        inline
      >
        <el-form-item label="UserName:" prop="username">
          <el-input
            v-model="form.username"
            placeholder="Enter username"
            :disabled="mode === 'edit'"
          />
        </el-form-item>

        <el-form-item label="Role:" prop="role_id">
          <div class="role-group" ref="roleGroupRef">
            <el-select v-model="form.role_id" placeholder="Select role" :append-to="roleGroupRef">
              <el-option
                v-for="item in roleOptions"
                :key="item.value"
                :label="item.label"
                :value="item.value"
              />
            </el-select>
          </div>
        </el-form-item>

        <!-- Status 独占一行，去除 inline 并设置宽00% -->
        <el-form-item
          v-if="mode === 'edit'"
          label="Enabled:"
          prop="is_active"
          class="user-form__status-row"
        >
          <el-switch v-model="form.is_active" />
        </el-form-item>

        <el-form-item label="Password:" prop="password">
          <el-input
            v-model="form.password"
            type="password"
            :placeholder="mode === 'create' ? 'Enter password' : 'Enter new password (optional)'"
            show-password
          />
        </el-form-item>

        <el-form-item label="Confirm Password:" prop="confirmPassword">
          <el-input
            v-model="form.confirmPassword"
            type="password"
            :placeholder="
              mode === 'create' ? 'Confirm password' : 'Confirm new password (optional)'
            "
            show-password
          />
        </el-form-item>
      </el-form>
    </template>

    <template #dialog-footer>
      <el-button type="warning" @click="onCancel">Cancel</el-button>
      <el-button type="primary" @click="onSubmit">Submit</el-button>
    </template>
  </FormDialog>
</template>

<script setup lang="ts">
import type { FormInstance } from 'element-plus'
import { userApi } from '@/api/user'
import type { UserFormModel, DialogExpose } from '@/types/userManagement'

const formRef = ref<FormInstance>()
const dialogRef = ref<DialogExpose>()

const getDefaultForm = (): UserFormModel => ({
  username: '',
  role_id: 1,
  is_active: true,
  password: '',
  confirmPassword: '',
})

const form = ref<UserFormModel>(getDefaultForm())
const roleGroupRef = ref<HTMLElement>()
const roleOptions = [
  { label: 'Admin', value: 1 },
  { label: 'Engineer', value: 2 },
  { label: 'Viewer', value: 3 },
]
const roleId = ref<number>(1)
// 密码验证函数（编辑模式下可选）
const validatePassword = (rule: any, value: string, callback: any) => {
  if (mode.value === 'create') {
    // 创建模式：必填
    if (value === '') {
      callback(new Error('Please input password'))
      return
    }
  } else {
    // 编辑模式：可选，如果为空则通过验证
    if (value === '') {
      callback()
      return
    }
  }

  // 密码长度验证：6-12个字符
  if (value.length < 6 || value.length > 12) {
    callback(new Error('Password must be 6-12 characters'))
    return
  }

  // 必须包含数字和英文字母
  const hasNumber = /\d/.test(value)
  const hasLetter = /[a-zA-Z]/.test(value)
  if (!hasNumber || !hasLetter) {
    callback(new Error('Password must contain numbers and letters'))
    return
  }

  callback()
}

// 密码确认验证函数
const validateConfirmPassword = (rule: any, value: string, callback: any) => {
  if (mode.value === 'create') {
    // 创建模式：必填，必须匹配密码
    if (value === '') {
      callback(new Error('Please confirm your password'))
    } else if (value !== form.value.password) {
      callback(new Error('Passwords do not match'))
    } else {
      callback()
    }
  } else {
    // 编辑模式：如果填写了密码，则确认密码必填且必须匹配
    if (form.value.password && value === '') {
      callback(new Error('Please confirm your password'))
    } else if (form.value.password && value !== form.value.password) {
      callback(new Error('Passwords do not match'))
    } else {
      callback()
    }
  }
}

const rules = computed(() => {
  const baseRules = {
    username: [
      { required: true, message: 'Please input username', trigger: 'blur' },
      { min: 3, max: 20, message: 'Length should be 3 to 20 characters', trigger: 'blur' },
    ],
    role_id: [{ required: true, message: 'Please select role', trigger: 'change' }],
    is_active: [{ required: true, message: 'Please select status', trigger: 'change' }],
    password: [{ validator: validatePassword, trigger: 'blur' }],
    confirmPassword: [{ validator: validateConfirmPassword, trigger: 'blur' }],
  }

  return baseRules
})

const mode = ref<'create' | 'edit'>('create')
const dialogTitle = computed(() => (mode.value === 'edit' ? 'Edit User' : 'Add User'))
function deepClone<T>(obj: T): T {
  return JSON.parse(JSON.stringify(obj))
}

async function open(userId: number, openMode: 'create' | 'edit' = 'create') {
  mode.value = openMode
  form.value = getDefaultForm()
  if (userId) {
    roleId.value = userId
    const user = await userApi.getUserDetail(userId)

    if (user.success) {
      form.value.username = user.data.username
      form.value.role_id = user.data.role.id
      form.value.is_active = user.data.is_active
    }
  }
  nextTick(() => {
    setTimeout(() => {
      formRef.value?.clearValidate()
    }, 100)
  })
  dialogRef.value && (dialogRef.value.dialogVisible = true)
}

function close() {
  dialogRef.value && (dialogRef.value.dialogVisible = false)
}

const emit = defineEmits<{
  (e: 'submit', value: UserFormModel): void
  (e: 'cancel'): void
}>()

function onCancel() {
  close()
  emit('cancel')
}

async function onSubmit() {
  formRef.value?.validate(async (valid) => {
    if (!valid) return
    if (mode.value === 'create') {
      const res = await userApi.addUser({
        username: form.value.username,
        password: form.value.password,
        role_id: form.value.role_id,
      })
      if (res.success) {
        ElMessage.success('User added successfully')
        emit('submit', form.value)
      }
      close()
    } else if (mode.value === 'edit') {
      const updateData: any = {
        role_id: form.value.role_id,
        is_active: form.value.is_active,
      }
      // 如果填写了新密码，则添加到更新数据中
      if (form.value.password) {
        updateData.password = form.value.password
      }
      const res = await userApi.updateUser(roleId.value, updateData)
      if (res.success) {
        ElMessage.success('User updated successfully')
        emit('submit', form.value)
        close()
      }
    }
  })
}

defineExpose({ open, close })
</script>

<style scoped lang="scss">
.aether-class {
  // .user-form {
  //   &.el-form--inline .el-form-item{
  //     margin-right: 0 !important;
  //   }
  // }
  .monitor-data-group,
  .role-group,
  .condition-group {
    width: 100%;
    // display: flex;
    // gap: 0.16rem;
  }

  .status-group {
    width: 100%;
    display: flex;
  }

  :deep(.el-input__inner) {
    width: 2.4rem;
  }

  .user-form__status-row {
    // width: 100%;
    width: 3.8rem;
    display: block;

    :deep(.el-form-item__content) {
      width: 6.6rem;
    }
  }
}
</style>
