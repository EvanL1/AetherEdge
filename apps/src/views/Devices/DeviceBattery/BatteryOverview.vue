<template>
  <div class="aether-class pv-overview">
    <div class="pv-overview__right">
      <BatteryCard
        class="battery-card"
        v-for="item in batteryCardData"
        :key="item.pointId"
        :title="item.title"
        :value="item.value"
        :unit="item.unit"
      ></BatteryCard>
    </div>
  </div>
</template>
<script setup lang="ts">
import { formatNumber } from '@/utils/common'
import { watch, ref } from 'vue'
import useWebSocket from '@/composables/useWebSocket'

interface BatteryCardItem {
  title: string
  value?: number | null | string
  unit?: string
  pointId: number
}

// WebSocket 数据
const wsData = ref<any>(null)

// 订阅 WebSocket - Overview 使用 inst 源
useWebSocket(
  {
    source: 'inst',
    channels: [1],
    dataTypes: ['A', 'M', 'P'] as any,
    interval: 1000,
  },
  {
    onBatchDataUpdate: (data: any) => {
      wsData.value = data
    },
  },
)

// 监听 WebSocket 数据更新
watch(
  wsData,
  (data) => {
    if (!data?.updates?.length) return
    // 从数据类型 M 中取值
    const mUpdate = data.updates.find(
      (item: any) => item.channel_id === 1 && item.data_type === 'M',
    )
    if (!mUpdate) return
    const values = mUpdate.values || {}
    batteryCardData.value.forEach((item) => {
      const pointValue = values[item.pointId]
      if (pointValue !== undefined && pointValue !== null) {
        if (item.pointId === 15) {
          item.value = pointValue === 1 ? 'Charge' : 'Discharge'
        } else {
          item.value = formatNumber(pointValue)
        }
      }
    })
  },
  { deep: true, immediate: true },
)
const batteryCardData = ref<BatteryCardItem[]>([
  //no
  {
    pointId: 15,
    title: 'Status',
    value: '-',
    unit: '',
  },
  {
    pointId: 3,
    title: 'SoC',
    value: '-',
    unit: '%',
  },
  {
    pointId: 4,
    title: 'SoH',
    value: '-',
    unit: '%',
  },

  {
    pointId: 1,
    title: 'Voltage',
    value: '-',
    unit: 'V',
  },

  {
    pointId: 2,
    title: 'Current',
    value: '-',
    unit: 'A',
  },

  //no
  {
    title: 'Power',
    value: '-',
    unit: 'KW',
    pointId: 3,
  },
  {
    title: 'Max Cell Voltage',
    value: '-',
    unit: 'V',
    pointId: 7,
  },
  {
    title: 'Min Cell Voltage',
    value: '-',
    unit: 'V',
    pointId: 8,
  },
  {
    title: 'Avg Cell Voltage',
    value: '-',
    unit: 'V',
    pointId: 9,
  },
  //no
  {
    title: 'Cell Voltage Difference',
    value: '-',
    unit: 'V',
    pointId: 10,
  },
  //no
  {
    title: 'Avg Cell Temperature',
    value: '-',
    unit: '℃',
    pointId: 11,
  },
])
</script>
<style scoped lang="scss">
.aether-class.pv-overview {
  height: 100%;
  width: 100%;
  display: flex;
  justify-content: flex-end;
  position: relative; // 确保z-index生效
  z-index: 1;

  &::after {
    content: '';
    position: absolute;
    // top: -0.72rem;
    left: -0.2rem;
    width: calc(100% + 0.4rem);
    // height: calc(100vh - 0.85rem); // 减去header高度
    height: calc(100% + 0.2rem);
    // margin-top: 0.72rem;
    background-image: url('@/assets/images/battery-bg.png');
    background-repeat: no-repeat;
    background-size: 60% 100%;
    background-position: left center;
    z-index: 0;
    pointer-events: none; // 防止背景阻挡用户交互
  }

  .pv-overview__right {
    width: 48.2%;
    height: 100%;
    overflow: auto;
    display: flex;
    flex-wrap: wrap;
    gap: 0.16rem;
    position: relative;
    z-index: 2; // 确保内容在背景之上

    .battery-card {
      height: calc((100% - 0.48rem) / 4);
      width: calc((100% - 0.32rem) / 3);
      // margin-bottom: 0.01rem;
    }
  }
}
</style>
