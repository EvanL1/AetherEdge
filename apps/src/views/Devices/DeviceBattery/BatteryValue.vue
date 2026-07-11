<template>
  <div class="aether-class devices-pv__content">
    <div class="devices-pv__tables">
      <LoadingBg :loading="globalStore.loading">
        <el-tabs v-model="activeTab" type="card" class="devices-pv__tabs">
          <el-tab-pane label="Battery" name="battery">
            <DeviceMonitoringTable
              :leftTableData="BatteryleftTableData"
              :rightTableData="BatteryrightTableData"
            />
          </el-tab-pane>
          <el-tab-pane label="PCS" name="pcs">
            <DeviceMonitoringTable
              :leftTableData="PCSleftTableData"
              :rightTableData="PCSrightTableData"
            />
          </el-tab-pane>
        </el-tabs>
      </LoadingBg>
    </div>
  </div>
</template>

<script setup lang="ts">
import LoadingBg from '@/components/common/LoadingBg.vue'
import { useGlobalStore } from '@/stores/global'
import type { LeftTableItem, RightTableItem } from '@/types/deviceMonitoring'
import { getPointsTables } from '@/api/channelsManagement'
import type { PointInfoResponse } from '@/types/channelConfiguration'
import { ref } from 'vue'
import useWebSocket from '@/composables/useWebSocket'

const globalStore = useGlobalStore()

const BatteryleftTableData = ref<LeftTableItem[]>([])
const BatteryrightTableData = ref<RightTableItem[]>([])
const PCSleftTableData = ref<LeftTableItem[]>([])
const PCSrightTableData = ref<RightTableItem[]>([])

// 订阅 WebSocket - ValueMonitoring 使用 comsrv 源
useWebSocket(
  {
    source: 'comsrv',
    channels: [2, 1],
    dataTypes: ['T', 'S'],
    interval: 1000,
  },
  {
    onBatchDataUpdate: (data: any) => {
      // 处理通道2的T类型数据（Battery左侧表格）
      const channel2TUpdate = data.updates?.find(
        (item: any) => item.channel_id === 2 && item.data_type === 'T',
      )
      if (channel2TUpdate) {
        const values = channel2TUpdate.values || {}
        const timestamps = channel2TUpdate.ts || {}
        BatteryleftTableData.value.forEach((item) => {
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

      // 处理通道2的S类型数据（Battery右侧表格）
      const channel2SUpdate = data.updates?.find(
        (item: any) => item.channel_id === 2 && item.data_type === 'S',
      )
      if (channel2SUpdate) {
        const values = channel2SUpdate.values || {}
        const timestamps = channel2SUpdate.ts || {}
        BatteryrightTableData.value.forEach((item) => {
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

      // 处理通道1的S类型数据（PCS右侧表格）
      const channel1SUpdate = data.updates?.find(
        (item: any) => item.channel_id === 1 && item.data_type === 'S',
      )
      if (channel1SUpdate) {
        const values = channel1SUpdate.values || {}
        const timestamps = channel1SUpdate.ts || {}
        PCSrightTableData.value.forEach((item) => {
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

      // 处理通道1的T类型数据（PCS左侧表格）
      const channel1TUpdate = data.updates?.find(
        (item: any) => item.channel_id === 1 && item.data_type === 'T',
      )
      if (channel1TUpdate) {
        const values = channel1TUpdate.values || {}
        const timestamps = channel1TUpdate.ts || {}
        PCSleftTableData.value.forEach((item) => {
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
    },
  },
)

// 初始化数据：通过 API 获取点位数据
onMounted(async () => {
  try {
    // Battery 通道 id 为 2
    const batteryRes = await getPointsTables(2)
    if (batteryRes?.success && batteryRes.data) {
      const batteryData = batteryRes.data as PointInfoResponse
      BatteryleftTableData.value =
        batteryData.telemetry?.map((p) => ({
          pointId: p.point_id,
          name: p.signal_name || '',
          unit: p.unit || '',
          value: null,
          updateTime: null,
        })) || []
      BatteryrightTableData.value =
        batteryData.signal?.map((p) => ({
          pointId: p.point_id,
          name: p.signal_name || '',
          status: null,
          updateTime: null,
        })) || []
    }

    // PCS 通道 id 为 1
    const pcsRes = await getPointsTables(1)
    if (pcsRes?.success && pcsRes.data) {
      const pcsData = pcsRes.data as PointInfoResponse
      PCSleftTableData.value =
        pcsData.telemetry?.map((p) => ({
          pointId: p.point_id,
          name: p.signal_name || '',
          unit: p.unit || '',
          value: null,
          updateTime: null,
        })) || []
      PCSrightTableData.value =
        pcsData.signal?.map((p) => ({
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
const activeTab = ref<'battery' | 'pcs'>('battery')
</script>

<style scoped lang="scss">
.devices-pv__content {
  width: 100%;
  height: 100%;

  .devices-pv__tables {
    position: relative;
    height: 100%;
    width: 100%;
  }
}

:deep(.devices-pv__tabs.el-tabs) {
  height: 100%;
}

:deep(.devices-pv__tabs .el-tab-pane) {
  height: 100%;
}
</style>
