<script setup lang="ts">
import { watch, nextTick } from 'vue'
import { ElNotification } from 'element-plus'
import en from 'element-plus/es/locale/lang/en'
import wsManager from '@/utils/websocket'
import { useRouter } from 'vue-router'
import { useGlobalStore } from '@/stores/global'
import { useUserStore } from '@/stores/user'
import { storeToRefs } from 'pinia'
import AppSkeleton from '@/components/common/AppSkeleton.vue'

const locale = en
const router = useRouter()
const globalStore = useGlobalStore()
const userStore = useUserStore()
const { appInitializing } = storeToRefs(globalStore)

const handleAlarmDetail = () => {
  ElNotification.closeAll()
  router.push({ name: 'alarmCurrentRecords' })
}

let idCount = 0
const alarmMap = new Map()

const initWebSocket = async () => {
  try {
    await wsManager.connect()
    console.log('[main] websocket connected')

    wsManager.setGlobalListeners({
      onConnect: () => {
        console.log('[main] websocket connected')
      },
      onDisconnect: () => {
        console.log('[main] websocket disconnected')
      },
      onError: (error) => {
        console.error('[main] websocket error:', error)
      },
      onAlarm: (alarm) => {
        if (alarm.status == 1) {
          const currentId = idCount++
          const notification = ElNotification({
            title: 'Alarm',
            type: 'error',
            customClass: 'alarm-notification alarm-notification--error',
            showClose: true,
            dangerouslyUseHTMLString: true,
            message: `
              <div class="alarm-notification-content">
                <span class="alarm-notification-msg">${alarm.message}</span>
                <div class="alarm-notification-footer">
                  <button id="to-detail-btn-${currentId}" class="alarm-detail-btn">to detail</button>
                </div>
              </div>
            `,
            duration: 0,
            onClose: () => {
              const buttonId = `to-detail-btn-${currentId}`
              const eventInfo = document.getElementById(buttonId)

              if (eventInfo) {
                eventInfo.removeEventListener('click', handleAlarmDetail)
                console.log(`[App] cleaned alarm button listener: ${buttonId}`)
              }

              alarmMap.delete(alarm.alarm_id)
            },
          })

          alarmMap.set(alarm.alarm_id, notification)

          nextTick(() => {
            const buttonId = `to-detail-btn-${currentId}`
            const btn = document.getElementById(buttonId)
            if (btn) {
              btn.addEventListener('click', handleAlarmDetail)
            }
          })
        } else {
          ElNotification.success({
            title: 'Alarm Recovered',
            message: alarm.message,
            customClass: 'alarm-notification alarm-notification--success',
            duration: 3000,
          })

          const existingNotification = alarmMap.get(alarm.alarm_id)
          if (existingNotification) {
            existingNotification.close()
            alarmMap.delete(alarm.alarm_id)
            console.log(`[App] closed alarm notification: ${alarm.alarm_id}`)
          }
        }

        console.log('[main] alarm:', alarm)
      },
      onAlarmNum: (alarmNum) => {
        console.log('[main] alarm count updated:', alarmNum)
        globalStore.alarmNum = alarmNum.current_alarms
      },
    })
  } catch (error) {
    console.error('[main] websocket connect failed:', error)
  }
}

watch(
  () => userStore.isLoggedIn,
  (isLoggedIn) => {
    if (isLoggedIn) {
      console.log('[main] user logged in, connecting websocket')
      initWebSocket()
    } else {
      console.log('[main] user logged out, disconnecting websocket')
      wsManager.disconnect()
    }
  },
  { immediate: true },
)
</script>

<template>
  <el-config-provider :locale="locale">
    <router-view />
    <!-- 骨架屏以覆盖层形式显示，保证 router-view 始终渲染，MainLayout 能正常挂载 -->
    <AppSkeleton v-if="appInitializing" class="app-skeleton-overlay" />
  </el-config-provider>
</template>

<style>
body {
  margin: 0;
  padding: 0;
  overflow: hidden;
  box-sizing: border-box;
}

#app {
  height: 100vh;
  width: 100vw;
  margin: 0;
  padding: 0;
  overflow: hidden;
  position: relative;
}

.app-skeleton-overlay {
  position: fixed;
  inset: 0;
  z-index: 9999;
}
</style>
