<template>
  <div class="aether-class pv-overview">
    <div class="pv-overview__header">
      <div class="cards-item" v-for="item in energyCardData" :key="item.title">
        <PVCard :title="item.title" :icon="item.icon" :value="item.value" :unit="item.unit" />
      </div>
    </div>
    <div class="pv-overview__content">
      <div class="trap-wrap">
        <div
          v-for="(item, idx) in rowData"
          :key="item.id"
          class="row"
          @mouseleave="hoveredRow = null"
          @mouseenter="handleHoveredRow(idx)"
        >
          <div v-show="hoveredRow === idx - 1" class="row-highlight" aria-hidden="true" />
        </div>
      </div>

      <!-- 单独的行卡片，根据悬停状态显示 -->
      <div
        v-for="(item, idx) in rowData"
        :key="`card-${item.id}`"
        v-show="hoveredRow === idx - 1"
        class="row-cards"
        :style="getRowCardStyle(idx)"
      >
        <div class="card-content-item">
          <div class="card-name">P:</div>
          <div class="card-value">{{ formatNumber(item.PValue) }}</div>
          <div class="card-unit">kw</div>
        </div>
        <div class="card-content-item">
          <div class="card-name">V:</div>
          <div class="card-value">{{ formatNumber(item.VValue) }}</div>
          <div class="card-unit">V</div>
        </div>
        <div class="card-content-item">
          <div class="card-name">I:</div>
          <div class="card-value">{{ formatNumber(item.IValue) }}</div>
          <div class="card-unit">A</div>
        </div>
      </div>
    </div>
  </div>
</template>
<script setup lang="ts">
import powerIcon from '@/assets/icons/Power.svg'
import voltageIcon from '@/assets/icons/Voltage.svg'
import currentIcon from '@/assets/icons/Current.svg'
import coolantTempIcon from '@/assets/icons/CoolantTemp.svg'
import { formatNumber } from '@/utils/common'
import useWebSocket from '@/composables/useWebSocket'

import { ref, watch, reactive } from 'vue'

const hoveredRow = ref<number | null>(null)
const rowData = reactive([
  {
    id: 1,
    PValue: 32,
    VValue: 220,
    IValue: 10,
  },
  {
    id: 2,
    PValue: 32,
    VValue: 220,
    IValue: 10,
  },
  {
    id: 3,
    PValue: 32,
    VValue: 220,
    IValue: 10,
  },
  {
    id: 4,
    PValue: 32,
    VValue: 220,
    IValue: 10,
  },
  {
    id: 5,
    PValue: 32,
    VValue: 220,
    IValue: 10,
  },
  {
    id: 6,
    PValue: 32,
    VValue: 220,
    IValue: 10,
  },
])

// 必须在 watch 之前声明 energyCardData，因为 watch 设置了 immediate: true
const energyCardData = reactive([
  {
    title: 'PV Power',
    icon: powerIcon,
    value: '-',
    unit: 'kW',
    pointId: 5,
  },
  {
    pointId: 8,
    title: 'PV Voltage',
    icon: voltageIcon,
    value: '-',
    unit: 'V',
  },
  {
    pointId: 9,
    title: 'PV Current',
    icon: currentIcon,
    value: '-',
    unit: 'A',
  },
  {
    pointId: 5,
    title: "Today's Energy",
    icon: coolantTempIcon,
    value: '-',
    unit: 'kWh',
  },
])

/**
 * 让第 i 行（0~rows-1）在 topScale 与 bottomScale 之间做线性插值，
 * 使得上窄下宽（形成梯形）。
 */
const handleHoveredRow = (r: number) => {
  hoveredRow.value = r - 1
}

// 计算每行卡片的位置
const getRowCardStyle = (idx: number) => {
  // 计算每行在容器中的位置
  const rowHeights = [8.3, 9.1, 11, 10.6, 12, 14] // 每行的高度百分比
  const rowGaps = [0, 4, 0, 4.9, 0, 0] // 每行后的间距

  let topPosition = 9.5
  let topvhPosition = 0
  for (let i = 0; i < idx; i++) {
    topPosition += rowHeights[i]
    topvhPosition += rowGaps[i]
  }

  // 当前行的中心位置
  const currentRowCenter = topPosition + rowHeights[idx] / 2

  return {
    top: `calc(${currentRowCenter}% + ${topvhPosition}vh)`,
    left: 'calc(50% + 28.5% + 1rem)', // 50% + 梯形容器宽度的一半(57%/2) + 额外间距
    transform: 'translate(-50%, -50%)',
  }
}
// WebSocket 数据
const wsData = ref<any>(null)

// 订阅 WebSocket - Overview 使用 inst 源
useWebSocket(
  {
    source: 'inst',
    channels: [4],
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
      (item: any) => item.channel_id === 4 && item.data_type === 'M',
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
    // 更新行数据
    rowData.forEach((row: any) => {
      // 假设 rowData 中的 id 对应 pointId
      // 这里需要根据实际的数据结构来映射
      // 暂时保持原有逻辑，后续可以根据实际需求调整
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

  .pv-overview__header {
    width: 100%;
    padding: 0.2rem 0;
    display: flex;
    justify-content: space-between;
    align-items: center;

    .cards-item {
      height: 1rem;
      width: calc((100% - 1.2rem) / 4);
    }
  }

  .pv-overview__content {
    position: relative;
    width: 100%;
    height: calc(100% - 1.4rem);
    background-image: url('@/assets/images/PV-bg.png');
    // background-repeat: no-repeat;
    background-size: 90% 130%;
    background-position: 27% 60%;
    display: flex;
    justify-content: center;
    align-items: center;
    overflow: visible;

    .update-time {
      position: absolute;
      top: 0.1rem;
      right: 0;
      color: #fff;
      z-index: 10;
    }

    /* 行卡片样式 */
    .row-cards {
      position: absolute;
      width: 1.5rem;
      // height: 4rem;
      height: 0.92rem;
      display: flex;
      flex-direction: column;
      justify-content: center;
      gap: 0.06rem;
      padding: 0.15rem;
      background-color: rgba(84, 98, 140, 0.8);
      border: 1px solid rgba(148, 166, 197, 0.3);
      border-radius: 0.1rem;
      z-index: 1000;

      .card-content-item {
        height: 0.16rem;
        display: flex;
        align-items: flex-end;
      }

      .card-name {
        font-weight: 400;
        font-size: 0.16rem;
        color: #ffffff;
        margin-right: 0.09rem;
      }

      .card-value {
        font-weight: 700;
        font-size: 0.16rem;
        text-transform: capitalize;
        color: #ffffff;
        vertical-align: bottom;
        margin-right: 0.04rem;
      }

      .card-unit {
        font-weight: 400;
        font-size: 0.12rem;
        text-transform: capitalize;
        color: rgba(255, 255, 255, 0.5);
        vertical-align: bottom;
      }
    }
  }

  .trap-wrap {
    // position: relative;
    width: 57%;
    height: 81%;
    display: flex;
    flex-direction: column;
    gap: 0.5%;
    overflow: auto;
  }

  /* 每一行是 16 列网格，宽度由 --row-scale 控制形成梯形 */
  .row {
    position: relative;
    width: 100%;
    margin-inline: auto;
    transition: width 0.12s ease;

    &:nth-child(1) {
      height: 10.3%;
      clip-path: polygon(10% 0, 90% 0, 91% 100%, 9% 100%);
    }

    &:nth-child(2) {
      height: 11.2%;
      clip-path: polygon(9% 0, 91% 0, 92% 100%, 8% 100%);
    }

    &:nth-child(3) {
      height: 12.14%;
      clip-path: polygon(7.5% 0, 93% 0, 94% 100%, 6.8% 100%);
    }

    &:nth-child(4) {
      height: 13.1%;
      clip-path: polygon(6.3% 0, 94.2% 0, 95.4% 100%, 5.4% 100%);
    }

    &:nth-child(5) {
      height: 14%;
      clip-path: polygon(4.8% 0, 96.6% 0, 97.8% 100%, 3.6% 100%);
    }

    &:nth-child(6) {
      height: 15%;
      clip-path: polygon(3.2% 0, 98.2% 0, 99.4% 100%, 2.3% 100%);
    }
  }

  /* 每两行与第三行之间有较大空隙 */
  .row:nth-child(2) {
    margin-bottom: 4vh;
    /* 第3、6行后有更大间距 */
  }

  .row:nth-child(4) {
    margin-bottom: 4.9vh;
    /* 第3、6行后有更大间距 */
  }

  /* 行高亮遮罩（覆盖整行内区，不挡事件） */
  .row-highlight {
    position: absolute;
    inset: 0;
    background: rgba(255, 105, 0, 0.2);
    pointer-events: none;
  }
}
</style>
