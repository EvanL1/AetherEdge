import { defineStore } from 'pinia'

// 定义全局store，包含菜单数据和根据角色筛选菜单的方法
export const useGlobalStore = defineStore(
  'global',
  () => {
    const isCollapse = ref(false)
    const alarmNum = ref(0)
    const loading = ref(false)
    // 应用初始化中（鉴权 + 路由注入），初始为 true，不持久化
    const appInitializing = ref(true)
    return {
      isCollapse,
      alarmNum,
      loading,
      appInitializing,
    }
  },
  {
    // 只持久化 token、refreshToken 和 userInfo，routesInjected 不持久化
    persist: {
      key: 'global',
      storage: localStorage,
      pick: ['isCollapse', 'alarmNum'],
    },
  },
)
