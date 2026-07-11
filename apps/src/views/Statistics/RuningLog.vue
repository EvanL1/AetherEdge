<template>
  <div class="aether-class running-log">
    <div class="running-log__content">
      <!-- 表格工具�?-->
      <div class="running-log__toolbar">
        <IconButton
          type="primary"
          :icon="alarmExportIcon"
          text="Export"
          custom-class="running-log__export-btn"
          @click="handleExport"
        />
      </div>
      <div class="running-log__container">
        <div class="running-log__item" v-for="item in runningLog" :key="item.id">
          [{{ item.time }}] [{{ item.type }}] {{ item.msg }}
        </div>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
// 正确引入SVG图标，避免部署后图片加载不出�?
import alarmExportIcon from '@/assets/icons/alarm-export.svg'

const handleExport = () => {
  console.log('导出')
}
const runningLog = ref([
  {
    id: 1,
    time: '2025-08-10 10:00:00',
    type: 'info',
    msg: 'EMS system startup initialized. Version: 1.6.4',
  },
  {
    id: 2,
    time: '2025-08-10 10:01:12',
    type: 'info',
    msg: 'Database connection established successfully.',
  },
  {
    id: 3,
    time: '2025-08-10 10:02:05',
    type: 'warning',
    msg: 'Battery voltage low detected on Device-002.',
  },
  {
    id: 4,
    time: '2025-08-10 10:03:20',
    type: 'error',
    msg: 'Failed to synchronize time with NTP server.',
  },
  {
    id: 5,
    time: '2025-08-10 10:04:10',
    type: 'info',
    msg: 'User Admin logged in from 192.168.1.10.',
  },
  {
    id: 6,
    time: '2025-08-10 10:05:30',
    type: 'info',
    msg: 'Device Router-001 added to the system.',
  },
  {
    id: 7,
    time: '2025-08-10 10:06:45',
    type: 'info',
    msg: 'Firmware update started for Device-002.',
  },
  {
    id: 8,
    time: '2025-08-10 10:07:50',
    type: 'success',
    msg: 'Firmware update completed for Device-002.',
  },
  {
    id: 9,
    time: '2025-08-10 10:08:15',
    type: 'info',
    msg: 'Alarm acknowledged by user operator.',
  },
  {
    id: 10,
    time: '2025-08-10 10:09:00',
    type: 'error',
    msg: 'Network timeout occurred on Device-003.',
  },
  {
    id: 11,
    time: '2025-08-10 10:10:22',
    type: 'info',
    msg: 'System health check completed. All services running.',
  },
  {
    id: 12,
    time: '2025-08-10 10:11:35',
    type: 'warning',
    msg: 'High temperature detected in Server Room.',
  },
  {
    id: 13,
    time: '2025-08-10 10:12:40',
    type: 'info',
    msg: 'Backup process started.',
  },
  {
    id: 14,
    time: '2025-08-10 10:13:55',
    type: 'success',
    msg: 'Backup completed successfully.',
  },
  {
    id: 15,
    time: '2025-08-10 10:15:00',
    type: 'info',
    msg: 'User operator logged out.',
  },
])
</script>

<style scoped lang="scss">
.aether-class.running-log {
  height: 100%;
  width: 100%;
  .running-log__content {
    width: 100%;
    height: 100%;
    display: flex;
    flex-direction: column;

    .running-log__toolbar {
      padding-bottom: 0.2rem;
      display: flex;
      align-items: center;
      justify-content: flex-end;

      .running-log__export-btn {
        display: flex;
        align-items: center;
        gap: 0.1rem;
        .running-log__export-icon {
          width: 0.16rem;
          height: 0.16rem;
          margin-right: 0.1rem;
        }
      }
    }

    .running-log__container {
      width: 100%;
      flex: 1;
      padding: 0.4rem;
      overflow-y: auto;
      background-color: rgba(84, 98, 140, 0.2);
      border: 0.01rem solid;
      border-image: linear-gradient(
          117.75deg,
          rgba(148, 166, 197, 0.3) 2.73%,
          rgba(148, 166, 197, 0) 31.68%,
          rgba(148, 166, 197, 0.103266) 71.5%,
          rgba(148, 166, 197, 0.3) 97.51%
        )
        1;
      backdrop-filter: blur(0.1rem);
      .running-log__item {
        font-family: Arimo;
        font-size: 0.18rem;
        line-height: 0.315rem;
        letter-spacing: 8%;
        color: #fff;
      }
    }
  }
}
</style>
