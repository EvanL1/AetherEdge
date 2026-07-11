<template>
  <div class="aether-class pv__content">
    <div class="devices-pv__tables">
      <LoadingBg :loading="globalStore.loading">
        <div class="devices-pv__tables-content">
          <DeviceMonitoringTable :leftTableData="leftTableData" :rightTableData="rightTableData" />
        </div>
      </LoadingBg>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref } from 'vue'
// import { onMounted } from 'vue'
// import useWebSocket from '@/composables/useWebSocket'
import DeviceMonitoringTable from '@/components/device/DeviceMonitoringTable.vue'
import LoadingBg from '@/components/common/LoadingBg.vue'
import { useGlobalStore } from '@/stores/global'
import type { LeftTableItem, RightTableItem } from '@/types/deviceMonitoring'
// import { getPointsTables } from '@/api/channelsManagement'
// import type { PointInfoResponse } from '@/types/channelConfiguration'

const globalStore = useGlobalStore()

const leftTableData = ref<LeftTableItem[]>([])
const rightTableData = ref<RightTableItem[]>([])

// 暂时注释掉 API 请求，待数据接口确认后启用
// onMounted(async () => {
//   try {
//     const res = await getPointsTables(6)
//     if (res?.success && res.data) {
//       const data = res.data as PointInfoResponse
//       leftTableData.value =
//         data.telemetry?.map((p) => ({
//           pointId: p.point_id,
//           name: p.signal_name || '',
//           unit: p.unit || '',
//           value: null,
//           updateTime: null,
//         })) || []
//       rightTableData.value =
//         data.signal?.map((p) => ({
//           pointId: p.point_id,
//           name: p.signal_name || '',
//           status: null,
//           updateTime: null,
//         })) || []
//     }
//   } catch (err) {
//     console.error('加载设备点位数据失败:', err)
//   }
// })

// 暂时注释掉 WebSocket 订阅，待通道配置确认后启用
// useWebSocket(
//   {
//     source: 'comsrv',
//     channels: [6],
//     dataTypes: ['T', 'S'],
//     interval: 1000,
//   },
//   {
//     onBatchDataUpdate: (data: any) => {
//       const channel6TUpdate = data.updates?.find(
//         (item: any) => item.channel_id === 6 && item.data_type === 'T',
//       )
//       if (channel6TUpdate) {
//         const values = channel6TUpdate.values || {}
//         const timestamps = channel6TUpdate.ts || {}
//         leftTableData.value.forEach((item) => {
//           const pointValue = values[String(item.pointId)]
//           const pointTimestamp = timestamps[String(item.pointId)]
//           if (pointValue !== undefined && pointValue !== null) {
//             item.value = pointValue
//           }
//           if (pointTimestamp !== undefined && pointTimestamp !== null) {
//             item.updateTime = pointTimestamp
//           }
//         })
//       }
//       const channel6SUpdate = data.updates?.find(
//         (item: any) => item.channel_id === 6 && item.data_type === 'S',
//       )
//       if (channel6SUpdate) {
//         const values = channel6SUpdate.values || {}
//         const timestamps = channel6SUpdate.ts || {}
//         rightTableData.value.forEach((item) => {
//           const pointValue = values[String(item.pointId)]
//           const pointTimestamp = timestamps[String(item.pointId)]
//           if (pointValue !== undefined && pointValue !== null) {
//             item.status = pointValue
//           }
//           if (pointTimestamp !== undefined && pointTimestamp !== null) {
//             item.updateTime = pointTimestamp
//           }
//         })
//       }
//     },
//   },
// )
</script>

<style scoped lang="scss">
.aether-class.pv__content {
  width: 100%;
  height: calc(100% - 0.4rem);

  .devices-pv__tables {
    width: 100%;
    height: 100%;
    gap: 0.2rem;

    .devices-pv__tables-content {
      width: 100%;
      height: 100%;
    }
  }
}
</style>
