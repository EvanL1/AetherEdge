<template>
  <div class="aether-class pv-overview">
    <div class="pv-overview__header">
      <div class="card-item" v-for="item in energyCardData" :key="item.title">
        <PVCard :title="item.title" :icon="item.icon" :value="item.value" :unit="item.unit" />
      </div>
    </div>
    <div class="pv-overview__content"></div>
  </div>
</template>
<script setup lang="ts">
import powerIcon from '@/assets/icons/Power.svg'
import oilIcon from '@/assets/icons/Oil.svg'
import voltageIcon from '@/assets/icons/Voltage.svg'
import coolantTempIcon from '@/assets/icons/CoolantTemp.svg'
import { formatNumber } from '@/utils/common'
import { watch, ref } from 'vue'
import useWebSocket from '@/composables/useWebSocket'

// WebSocket 数据
const wsData = ref<any>(null)

// 订阅 WebSocket - Overview 使用 inst 源
useWebSocket(
  {
    source: 'inst',
    channels: [2],
    dataTypes: ['A', 'M', 'P'] as any,
    interval: 1000,
  },
  {
    onBatchDataUpdate: (data: any) => {
      wsData.value = data
    },
  },
)

const energyCardData = reactive([
  //no
  {
    pointId: 1,
    title: 'Power',
    icon: powerIcon,
    value: '-',
    unit: 'kW',
  },
  //no
  {
    pointId: 13,
    title: 'oil',
    icon: oilIcon,
    value: '-',
    unit: '%',
  },
  //no
  {
    pointId: 3,
    title: 'Voltage',
    icon: voltageIcon,
    value: '-',
    unit: 'V',
  },
  //no
  {
    pointId: 14,
    title: 'Temperature',
    icon: coolantTempIcon,
    value: '-',
    unit: '℃',
  },
])

// 监听 WebSocket 数据更新
watch(
  wsData,
  (data) => {
    if (!data?.updates?.length) return
    // 从数据类型 M 中取值
    const mUpdate = data.updates.find(
      (item: any) => item.channel_id === 2 && item.data_type === 'M',
    )
    if (!mUpdate) return
    const values = mUpdate.values || {}
    energyCardData.forEach((item: any) => {
      if (item.pointId) {
        const pointValue = values[item.pointId]
        if (pointValue !== undefined && pointValue !== null) {
          item.value = formatNumber(pointValue)
        }
      }
    })
  },
  { deep: true, immediate: true },
)
</script>
<style scoped lang="scss">
.aether-class.pv-overview {
  height: 100%;
  width: 100%;
  display: flex;
  flex-direction: column;
  position: relative;
  z-index: 1;

  .pv-overview__header {
    width: 100%;
    padding: 0.2rem 0;
    display: flex;
    justify-content: space-between;
    align-items: center;

    &::after {
      content: '';
      position: absolute;
      top: -0.72rem;
      left: -0.2rem;
      width: calc(100% + 0.4rem);
      height: calc(100vh - 0.85rem);
      background-image: url('@/assets/images/DieselGenerator-bg.png');
      background-repeat: no-repeat;
      background-size: 100% 100%;
      background-position: center;
      z-index: 0;
      pointer-events: none;
    }

    .card-item {
      height: 1rem;
      width: calc((100% - 1.2rem) / 4);
      position: relative;
      z-index: 1;
    }
  }

  .pv-overview__content {
    width: 100%;
    flex: 1;

    // background-image: url('@/assets/images/DieselGenerator-bg.png');
    // background-repeat: no-repeat;
    // background-size: 100% 100%;
  }
}
</style>
