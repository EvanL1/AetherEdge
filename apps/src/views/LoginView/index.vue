<template>
  <div class="aether-class loginPage">
    <header class="loginPage__header">
      <div class="loginPage__head-title">
        <div class="loginPage__head-icon" aria-hidden="true"></div>
        <div class="loginPage__head-text">AetherEMS</div>
      </div>
    </header>
    <div ref="loginFormContainer" class="loginPage__form">
      <ModuleCard title="AetherEMS">
        <div class="loginPage__form-content">
          <el-form
            :model="form"
            label-position="top"
            ref="formRef"
            :rules="formRules"
            class="loginPage__form-panel"
          >
            <el-form-item label="Username" prop="username">
              <el-input v-model="form.username" />
            </el-form-item>
            <el-form-item label="Password" prop="password">
              <el-input
                v-model="form.password"
                type="password"
                @keyup.enter="handleLogin(formRef)"
              />
            </el-form-item>
            <el-button
              type="primary"
              class="loginPage__submit-btn"
              @click="handleLogin(formRef)"
              :loading="globalStore.loading"
              >Login</el-button
            >
          </el-form>
        </div>
      </ModuleCard>
    </div>
  </div>
</template>

<script setup lang="ts">
import type { FormInstance, FormRules } from 'element-plus'
import { useUserStore } from '@/stores/user'
import { useGlobalStore } from '@/stores/global'
import type { LoginParams } from '@/types/user'
import { useRouter } from 'vue-router'
// import wsManager from '@/utils/websocket'

const router = useRouter()
const formRef = ref<FormInstance>()
const globalStore = useGlobalStore()
const form = reactive<LoginParams>({
  username: '',
  password: '',
})
const formRules = reactive<FormRules<LoginParams>>({
  username: [{ required: true, message: 'Please enter your username', trigger: 'blur' }],
  password: [
    { required: true, message: 'Please enter your password', trigger: 'blur' },
    // {
    //   pattern: /^(?=.*[A-Za-z])(?=.*\d)[A-Za-z\d]{4,12}$/,
    //   message: 'Password must be 6-12 characters and include both letters and numbers',
    //   trigger: 'blur',
    // },
  ],
})
const userStore = useUserStore()

const handleLogin = async (formEl: FormInstance | undefined) => {
  if (!formEl) return
  formEl.validate(async (valid: boolean) => {
    try {
      if (valid) {
        const res = await userStore.login(form)
        if (res.success) {
          const userInfo = await userStore.getUserInfo()
          if (userInfo.success) {
            // wsManager.connect()
            const redirect = router.currentRoute.value.query.redirect as string
            if (redirect) {
              router.push({ path: redirect })
            } else {
              router.push({ path: '/' })
            }
          }
        }
        // 这里可以添加登录请求逻辑
      } else {
        console.log('表单校验未通过')
      }
    } catch (error) {
      console.error('登录失败:', error)
    }
  })
}
</script>

<style lang="scss" scoped>
.aether-class.loginPage {
  width: 100%;
  height: 100%;
  background: #02081a;
  position: relative;
  overflow: hidden;
  background-image: url('../../assets/images/login-bg.png');
  background-size: cover;
  background-position: center;
  background-repeat: no-repeat;
  border: 0.01rem solid;

  border-image-source: linear-gradient(
    117.31deg,
    rgba(148, 166, 197, 0.3) 2.77%,
    rgba(148, 166, 197, 0) 32.18%,
    rgba(148, 166, 197, 0.103266) 72.63%,
    rgba(148, 166, 197, 0.3) 99.05%
  );

  // 顶部标题栏
  .loginPage__header {
    width: 100%;
    height: 0.84rem;
    display: flex;
    align-items: center;
    background: rgba(84, 98, 140, 0.3);
    border-bottom: 0.01rem solid rgba(148, 166, 197, 0.3);

    .loginPage__head-title {
      height: 100%;
      display: flex;
      align-items: center;
      margin-left: 0.3rem;

      .loginPage__head-icon {
        width: 0.24rem;
        height: 0.24rem;
        margin-right: 0.12rem;
        background-image: url('../../assets/images/aether-logo-mark.svg');
        background-size: contain;
        background-position: center;
        background-repeat: no-repeat;
      }

      .loginPage__head-text {
        // margin-left: 0.2rem;
        font-family: 'Montserrat', sans-serif;
        font-weight: 600;
        font-size: 0.3rem;
        line-height: 1.5em;
        color: #ffffff;
      }
    }

    .loginPage__form-button {
      margin-top: 0.2rem;
    }
  }

  // 登录表单区域
  .loginPage__form {
    position: absolute;
    top: 3.28rem;
    left: 14.62rem;
    width: 3.24rem;

    .loginPage__form-content {
      padding: 0.4rem 0.21rem;
    }
  }
}

:deep(.loginPage__submit-btn.el-button--primary) {
  height: 0.32rem;
  width: 2.4rem;
  margin-top: 0.2rem;
}

:deep(.loginPage__form-panel .el-form-item__label::before) {
  display: none;
}

:deep(.loginPage__form-panel .el-form-item .el-form-item__label) {
  height: 0.22rem;
}
</style>
