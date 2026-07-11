import { nextTick } from 'vue'
import { router } from './index'
import { useUserStore } from '@/stores/user'
import { useGlobalStore } from '@/stores/global'
import { ensureRoutesInjected } from './injector'
import { cancelAllPendingRequests } from '@/utils/request'

const WHITE_LIST = ['/login']

router.beforeEach(async (to, from, next) => {
  cancelAllPendingRequests()

  const user = useUserStore()
  const global = useGlobalStore()

  // 登录页：直接放行，afterEach 会立即关闭骨架屏
  if (WHITE_LIST.includes(to.path)) {
    return next()
  }

  try {
    if (!user.token || !user.userInfo) {
      if (!user.refreshToken) {
        // 无 refreshToken，跳登录（afterEach 会关闭骨架屏）
        return next({ path: '/login' })
      }

      // 有 refreshToken：刷新 token，骨架屏保持显示
      const result = await user.refreshUserToken()
      if (result.success) {
        const res = await user.getUserInfo()
        if (!res.success) {
          user.clearUserData()
          return next({ path: '/login' })
        }
      } else {
        user.clearUserData()
        return next({ path: '/login' })
      }
    }

    // 路由尚未注入：注入后重定向，骨架屏继续覆盖，afterEach 处理关闭
    if (!user.routesInjected) {
      await ensureRoutesInjected()
      return next({ ...to, replace: true })
    }

    next()
  } catch (e) {
    console.error('Route guard error:', e)
    user.clearUserData()
    next({ path: '/login' })
  }
})

/**
 * 在导航完全结束后关闭骨架屏：
 * - nextTick 确保 router-view 已完成 DOM 更新，避免骨架屏消失后出现短暂空白
 * - 仅在 appInitializing 为 true 时执行（只有首次启动才需要）
 */
router.afterEach((to) => {
  const global = useGlobalStore()
  if (!global.appInitializing) return

  if (WHITE_LIST.includes(to.path)) {
    // 登录页不需要等 DOM，直接关闭（骨架屏是应用布局，不适合显示在登录页上）
    global.appInitializing = false
  } else {
    // 应用页面：等 DOM 更新完毕再关闭，消除空白帧
    nextTick(() => {
      global.appInitializing = false
    })
  }
})
