<template>
  <div class="aether-class pv__content">
    <div class="devices-pv__tables">
      <LoadingBg :loading="globalStore.loading">
        <DeviceMonitoringTable :leftTableData="leftTableData" :rightTableData="rightTableData" />
      </LoadingBg>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted } from 'vue'
import LoadingBg from '@/components/common/LoadingBg.vue'
import DeviceMonitoringTable from '@/components/device/DeviceMonitoringTable.vue'
import { useGlobalStore } from '@/stores/global'
import type { LeftTableItem, RightTableItem } from '@/types/deviceMonitoring'
import { getPointsTables } from '@/api/channelsManagement'
import type { PointInfoResponse } from '@/types/channelConfiguration'
import useWebSocket from '@/composables/useWebSocket'

const globalStore = useGlobalStore()

const leftTableData = ref<LeftTableItem[]>([])
const rightTableData = ref<RightTableItem[]>([])

// 订阅 WebSocket - ValueMonitoring 使用 comsrv 源
useWebSocket(
  {
    source: 'comsrv',
    channels: [3],
    dataTypes: ['T', 'S'],
    interval: 1000,
  },
  {
    onBatchDataUpdate: (data: any) => {
      // 处理通道3的T类型数据（左侧表格）
      const channel3TUpdate = data.updates?.find(
        (item: any) => item.channel_id === 3 && item.data_type === 'T',
      )
      if (channel3TUpdate) {
        const values = channel3TUpdate.values || {}
        const timestamps = channel3TUpdate.ts || {}
        leftTableData.value.forEach((item) => {
          const pointValue = values[String(item.pointId)]
          const pointTimestamp = timestamps[String(item.pointId)]
          if (pointValue !== undefined && pointValue !== null) {
            item.value = pointValue
          }
          if (pointTimestamp !== undefined && pointTimestamp !== null) {
            item.updateTime = pointTimestamp
          }
        })
      }

      // 处理通道3的S类型数据（右侧表格）
      const channel3SUpdate = data.updates?.find(
        (item: any) => item.channel_id === 3 && item.data_type === 'S',
      )
      if (channel3SUpdate) {
        const values = channel3SUpdate.values || {}
        const timestamps = channel3SUpdate.ts || {}
        rightTableData.value.forEach((item) => {
          const pointValue = values[String(item.pointId)]
          const pointTimestamp = timestamps[String(item.pointId)]
          if (pointValue !== undefined && pointValue !== null) {
            item.status = pointValue
          }
          if (pointTimestamp !== undefined && pointTimestamp !== null) {
            item.updateTime = pointTimestamp
          }
        })
      }
    },
  },
)

// 初始化数据：通过 API 获取点位数据
onMounted(async () => {
  try {
    // PV 暂时使用通道 3（与 DieselGenerator 相同）
    const res = await getPointsTables(3)
    if (res?.success && res.data) {
      const data = res.data as PointInfoResponse
      leftTableData.value =
        data.telemetry?.map((p) => ({
          pointId: p.point_id,
          name: p.signal_name || '',
          unit: p.unit || '',
          value: null,
          updateTime: null,
        })) || []
      rightTableData.value =
        data.signal?.map((p) => ({
          pointId: p.point_id,
          name: p.signal_name || '',
          status: null,
          updateTime: null,
        })) || []
    }
  } catch (err) {
    console.error('加载设备点位数据失败:', err)
  }
})
</script>

<style scoped lang="scss">
.aether-class.pv__content {
  width: 100%;
  height: calc(100% - 0.4rem);

  .devices-pv__tables {
    width: 100%;
    height: 100%;
    gap: 0.2rem;
  }
}
</style>
