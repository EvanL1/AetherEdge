<template>
  <div class="aether-class curves">
    <!-- 表格区域 -->
    <div class="curves__content">
      <!-- 表格工具栏-->
      <div class="curves__toolbar">
        <div class="curves__toolbar-right">
          <div class="curves__toolbar-time-btns" @click="handleTimeBtnClick">
            <div
              v-show="selectedTimeBtn === 'custom'"
              class="curves__toolbar-time-interval"
              ref="toolbarRightRef"
            >
            </div>
            <el-date-picker
              v-if="selectedTimeBtn === 'custom'"
              v-model="rangeArray"
              type="datetimerange"
              value-format="YYYY-MM-DD HH:mm:ss"
              format="YYYY-MM-DD HH:mm:ss"
              range-separator="To"
              :default-time="defaultTime"
              start-placeholder="Select Start Date"
              end-placeholder="Select End Date"
              :teleported="false"
              @change="handleDateRangeChange"
            />
            <div
              v-for="btn in timeBtnList"
              :key="btn.value"
              class="curves__toolbar-time-btn"
              :class="{ 'is-active': selectedTimeBtn === btn.value }"
              :data-value="btn.value"
            >
              {{ btn.label }}
            </div>
          </div>
        </div>
      </div>

      <!-- 表格 -->
      <div class="curves__charts">
        <!-- 堆叠柱状图-->
        <div class="curves__chart-item">
          <ModuleCard title="Energy Chart">
            <StackedBarChart
              :xAxiosOption="xAxiosOption"
              :yAxiosOption="yAxiosOption"
              :series="exampleSeries"
            />
          </ModuleCard>
        </div>

        <!-- 饼图 -->
        <div class="curves__chart-item">
          <ModuleCard title="Energy Distribution">
            <DoughnutChart :series="exampleDoughntSeries" />
          </ModuleCard>
        </div>

        <!-- 折线图-->
        <div class="curves__chart-item">
          <ModuleCard title="Power Trend">
            <lineChart
              :xAxiosOption="powerTrendXAxis"
              :yAxiosOption="powerTrendYAxis"
              :series="powerTrendSeries"
            />
          </ModuleCard>
        </div>

        <!-- 其余图表 -->
        <div class="curves__chart-item" v-for="(item, idx) in 6" :key="idx">
          <ModuleCard title="Energy Chart">
            <StackedBarChart
              :xAxiosOption="xAxiosOption"
              :yAxiosOption="yAxiosOption"
              :series="exampleSeries"
            />
          </ModuleCard>
        </div>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
const activeTab = ref<'current' | 'history'>('current')
const toolbarRightRef = ref<HTMLElement | null>(null)

// 时间按钮列表
const timeBtnList: { label: string; value: '6h' | '1d' | '1w' | '1m' | 'custom' }[] = [
  { label: 'Custom', value: 'custom' },
  { label: '6 Hour', value: '6h' },
  { label: '1 Day', value: '1d' },
  { label: '1 Week', value: '1w' },
  { label: '1 Month', value: '1m' },
]

const defaultTime: [Date, Date] = [new Date(2000, 0, 1, 0, 0, 0), new Date(2000, 0, 1, 23, 59, 59)]
const timeInterval = ref('15m')
const rangeArray = ref<string[]>([])
const intervalList = ref([
  { label: '30 Seconds', value: '30s' },
  { label: '1 Minute', value: '1m' },
  { label: '15 Minutes', value: '15m' },
  { label: '1 Hour', value: '1h' },
  { label: '6 Hours', value: '6h' },
  { label: '1 Day', value: '1d' },
  { label: '1 Week', value: '1w' },
  { label: '1 Month', value: '1M' },
  { label: '1 Year', value: '1y' },
])

// 当前选中的时间按钮
const selectedTimeBtn = ref<'6h' | '1d' | '1w' | '1m' | 'custom'>('6h')

// 事件代理处理时间按钮点击
const handleTimeBtnClick = (event: MouseEvent) => {
  const target = event.target as HTMLElement
  // 查找最近的按钮元素
  const btn = target.closest('.curves__toolbar-time-btn') as HTMLElement | null
  if (btn && btn.dataset.value) {
    selectedTimeBtn.value = btn.dataset.value as '6h' | '1d' | '1w' | '1m' | 'custom'
    rangeArray.value = []
    if (selectedTimeBtn.value !== 'custom') {
      // 处理非自定义时间选择
      handleTimeRangeChange()
    }
  }
}

// 处理日期范围变化
const handleDateRangeChange = () => {
  if (selectedTimeBtn.value === 'custom' && rangeArray.value.length === 2) {
    handleTimeRangeChange()
  }
}

// 处理时间范围变化（统一处理数据刷新）
const handleTimeRangeChange = () => {
  console.log('Time range changed:', selectedTimeBtn.value, rangeArray.value, timeInterval.value)
  // 这里可以添加数据刷新逻辑
}

// 功率趋势数据 - 用于折线图
const powerTrendXAxis = {
  xAxiosData: ['00:00', '04:00', '08:00', '12:00', '16:00', '20:00', '24:00'],
}
const powerTrendYAxis = {
  yUnit: 'kW',
}
const powerTrendSeries = [
  {
    name: 'total power',
    data: [120, 135, 140, 160, 180, 200, 210],
    color: 'rgba(105, 203, 255, 1)',
  },
  {
    name: 'load power',
    data: [100, 110, 115, 130, 150, 170, 180],
    color: 'rgba(29, 134, 255, 1)',
  },
]

const exampleDoughntSeries = [
  {
    name: 'pv',
    value: 45,
    color: '#4FADF7',
  },
  {
    name: 'diesel generator',
    value: 30,
    color: '#F6C85F',
  },
  {
    name: 'ess',
    value: 25,
    color: '#6DD400',
  },
]
const exampleXAxisData = [
  '0:00',
  '2:00',
  '4:00',
  '6:00',
  '8:00',
  '10:00',
  '12:00',
  '14:00',
  '16:00',
  '18:00',
  '20:00',
  '22:00',
]
const xAxiosOption = {
  xAxiosData: exampleXAxisData,
}
const yAxiosOption = {
  yUnit: 'kWh',
}
const exampleSeries = [
  {
    name: 'Diesel',
    data: [120, 135, 140, 160, 180, 200, 210, 190, 170, 160, 150, 140],
    color: 'rgb(3, 93, 239)',
  },
  {
    name: 'ESS',
    data: [80, 90, 100, 110, 120, 130, 140, 135, 130, 125, 120, 115],
    color: 'rgb(29, 134, 255)',
  },
  {
    name: 'PV',
    data: [0, 10, 30, 60, 100, 130, 150, 140, 120, 80, 30, 5],
    color: 'rgb(105, 203, 255)',
  },
]
</script>

<style scoped lang="scss">
.aether-class.curves {
  height: 100%;
  width: 100%;

  .curves__content {
    display: flex;
    flex-direction: column;
    width: 100%;
    height: 100%;
    display: flex;
    flex-direction: column;
  }

  .curves__toolbar {
    padding-bottom: 0.2rem;
    display: flex;
    align-items: center;
    justify-content: flex-end;

    .curves__toolbar-right {
      display: flex;
      align-items: center;
      gap: 0.2rem;

      .curves__toolbar-time-btns {
        position: relative;
        height: 0.32rem;
        display: flex;
        align-items: center;

        .curves__toolbar-time-interval {
          position: relative;
          margin-right: 0.2rem;
        }

        .curves__toolbar-time-btn {
          height: 0.32rem;
          line-height: 0.32rem;
          padding: 0 0.1rem;
          font-size: 0.14rem;
          background: transparent;
          border-right: 0.01rem solid rgba(255, 255, 255, 0.2);
          cursor: pointer;

          &:last-child {
            border-right: none;
          }

          &.is-active {
            background: rgba(255, 255, 255, 0.2);
          }
        }
      }
    }
  }

  .curves__charts {
    flex: 1;
    display: flex;
    flex-wrap: wrap;
    gap: 0.2rem;

    .curves__chart-item {
      width: calc((100% - 0.4rem) / 3);
      height: calc((100% - 0.4rem) / 3);
    }
  }
}
</style>
