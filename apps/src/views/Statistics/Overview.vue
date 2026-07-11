<template>
  <div class="aether-class curves">
    <!-- 表格区域 -->
    <div class="curves__content">
      <!-- 表格工具栏 -->
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

      <!-- 图表区域 -->
      <div class="curves__charts">
        <!-- 概览卡片 -->
        <div class="curves__chart-item">
          <div class="chart__review">
            <div class="chart__review-header">
              <div class="chart__review-header-title">Energy consumption</div>
              <div class="chart__review-header-value">
                1000 &nbsp;<span class="chart__review-header-unit">kWh</span>
              </div>
            </div>
            <div class="chart__review-content">
              <div class="chart__review-content-list">
                <div
                  v-for="item in stationInfoList"
                  :key="item.title"
                  class="chart__review-content-item"
                >
                  <EnergyCard
                    :title="item.title"
                    :icon="item.icon"
                    :value="item.value"
                    :unit="item.unit"
                  />
                </div>
              </div>
            </div>
          </div>
        </div>

        <!-- 饼图卡片 -->
        <div class="curves__chart-item">
          <ModuleCard title="Energy Distribution">
            <DoughnutChart :series="energyDistributionData" />
          </ModuleCard>
        </div>

        <!-- 折线图卡片 -->
        <div class="curves__chart-item">
          <ModuleCard title="Power Trend">
            <lineChart
              :xAxiosOption="powerTrendXAxis"
              :yAxiosOption="powerTrendYAxis"
              :series="powerTrendSeries"
            />
          </ModuleCard>
        </div>

        <!-- 堆叠柱状图卡片 -->
        <div class="curves__chart-item">
          <ModuleCard title="Energy Chart">
            <StackedBarChart
              :xAxiosOption="xAxiosOption"
              :yAxiosOption="yAxiosOption"
              :series="exampleSeries"
            />
          </ModuleCard>
        </div>

        <!-- 其余图表卡片 -->
        <div class="curves__chart-item" v-for="(item, idx) in 2" :key="idx">
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
import PVEnergy from '@/assets/icons/icon-pv-energy.svg'
import ESS from '@/assets/icons/icon-ess-energy.svg'
import DG from '@/assets/icons/DGEnergy.svg'
import { queryPowerTrend } from '@/api/Statistic/overview'
import dayjs from 'dayjs'
import { getRecentHoursRange, getRecentWeekRange, getRecentDaysRange } from '@/utils/date.ts'
import type { QueryPowerTrendParams } from '@/types/Statistics/OverView'

interface TrendPoint {
  timestamp: string
  value: number
}

interface LineSeries {
  name: string
  data: number[]
  color: string
}

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

const stationInfoList = reactive([
  {
    title: 'PV',
    icon: PVEnergy,
    value: '150',
    unit: 'kWh',
  },
  {
    title: 'ESS',
    icon: ESS,
    value: '150',
    unit: 'kWh',
  },
  {
    title: 'DG',
    icon: DG,
    value: '145',
    unit: 'kWh', // 修正单位大小写
  },
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
      fetchPowerTrendData()
    }
  }
}

// 处理日期范围变化
const handleDateRangeChange = () => {
  if (selectedTimeBtn.value === 'custom' && rangeArray.value.length === 2) {
    fetchPowerTrendData()
  }
}

// 能源分布数据 - 用于饼图
const energyDistributionData = [
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

// 功率趋势数据 - 用于折线图
const powerTrendYAxis = {
  yUnit: 'kW',
}
const powerTrendXAxis = reactive({
  xAxiosData: [] as string[],
})
const powerTrendSeries = ref<LineSeries[]>([
  {
    name: 'Point 1',
    data: [],
    color: 'rgba(105, 203, 255, 1)',
  },
  {
    name: 'Point 2',
    data: [],
    color: 'rgba(29, 134, 255, 1)',
  },
])

const formatTimestampLabel = (timestamp: string) => dayjs(timestamp).format('YYYY-MM-DD\nHH:mm:ss')
const formatValue = (value: number | string | undefined | null) =>
  Number(Number(value ?? 0).toFixed(3))

const fetchPowerTrendData = async () => {
  try {
    const requestPayload: QueryPowerTrendParams = {
      redis_key: 'inst:1:M',
      point_id: '',
    }

    if (selectedTimeBtn.value === 'custom') {
      // 自定义时间范围
      if (rangeArray.value.length === 2) {
        requestPayload.start_time = rangeArray.value[0]
        requestPayload.end_time = rangeArray.value[1]
        // 根据时间间隔设置 interval（这里需要根据实际需求调整）
        // 暂时使用固定值，后续可以根据时间范围和间隔动态计算
        requestPayload.interval = 720
      } else {
        return // 如果没有选择完整的时间范围，不执行请求
      }
    } else {
      // 预设时间范围
      switch (selectedTimeBtn.value) {
        case '6h': {
          const range = getRecentHoursRange(6)
          requestPayload.interval = 720
          requestPayload.start_time = range.start
          requestPayload.end_time = range.end
          break
        }
        case '1d': {
          requestPayload.interval = 2880
          break
        }
        case '1w': {
          const range = getRecentWeekRange()
          requestPayload.interval = 21600
          requestPayload.start_time = range.start
          requestPayload.end_time = range.end
          break
        }
        case '1m': {
          const range = getRecentDaysRange(7) // 按需求：最近一周
          requestPayload.interval = 86400
          requestPayload.start_time = range.start
          requestPayload.end_time = range.end
          break
        }
        default:
          break
      }
    }
    const [point1Res, point2Res] = await Promise.all([
      queryPowerTrend({ ...requestPayload, point_id: '1' }),
      queryPowerTrend({ ...requestPayload, point_id: '2' }),
    ])

    const point1Data: TrendPoint[] = [...(point1Res.data || [])].reverse()
    const point2Data: TrendPoint[] = [...(point2Res.data || [])].reverse()

    const orderedTimestamps = Array.from(
      new Set(
        [...point1Data, ...point2Data]
          .map((item) => item.timestamp)
          .filter((item): item is string => Boolean(item)),
      ),
    ).sort((a, b) => new Date(a).getTime() - new Date(b).getTime())
    powerTrendXAxis.xAxiosData = orderedTimestamps.map((timestamp) =>
      formatTimestampLabel(timestamp),
    )

    const point1Map = new Map(point1Data.map((item) => [item.timestamp, item.value]))
    const point2Map = new Map(point2Data.map((item) => [item.timestamp, item.value]))

    powerTrendSeries.value = [
      {
        name: 'Point 1',
        data: orderedTimestamps.map((timestamp) => formatValue(point1Map.get(timestamp))),
        color: 'rgba(105, 203, 255, 1)',
      },
      {
        name: 'Point 2',
        data: orderedTimestamps.map((timestamp) => formatValue(point2Map.get(timestamp))),
        color: 'rgba(29, 134, 255, 1)',
      },
    ]
  } catch (error) {
    console.error('Failed to load power trend data:', error)
  }
}

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

onMounted(() => {
  fetchPowerTrendData()
})
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
    height: calc(100% - 0.52rem);
    display: flex;
    flex-wrap: wrap;
    gap: 0.2rem;

    .curves__chart-item {
      width: calc((100% - 0.4rem) / 3);
      height: calc((100% - 0.2rem) / 2);

      .chart__review {
        width: 100%;
        height: 100%;
        padding: 0.2rem;
        display: flex;
        flex-direction: column;
        background-color: rgba(84, 98, 140, 0.2);
        border: 0.01rem solid;

        border-image: linear-gradient(
            117.01deg,
            rgba(148, 166, 197, 0.3) 3.11%,
            rgba(148, 166, 197, 0) 31.6%,
            rgba(148, 166, 197, 0.103266) 70.79%,
            rgba(148, 166, 197, 0.3) 96.39%
          )
          1;
        backdrop-filter: blur(0.1rem);

        .chart__review-header {
          height: 0.83rem;
          padding: 0 0.2rem;
          background-color: rgba(84, 98, 140, 0.2);
          border: 0.01rem solid;

          border-image: linear-gradient(
              117.01deg,
              rgba(148, 166, 197, 0.3) 3.11%,
              rgba(148, 166, 197, 0) 31.6%,
              rgba(148, 166, 197, 0.103266) 70.79%,
              rgba(148, 166, 197, 0.3) 96.39%
            )
            1;
          backdrop-filter: blur(0.1rem);
          display: flex;
          justify-content: space-between;
          align-items: center;

          .chart__review-header-title {
            font-weight: 700;
            font-size: 0.26rem;
            line-height: 100%;
            letter-spacing: 0%;
          }

          .chart__review-header-value {
            font-weight: 700;
            font-size: 0.22rem;
            line-height: 0.3rem;
            letter-spacing: 0%;

            .chart__review-header-unit {
              font-size: 0.14rem;
              line-height: 0.3rem;
              letter-spacing: 0%;
              color: rgba(255, 255, 255, 0.6);
            }
          }
        }

        .chart__review-content {
          flex: 1;
          padding-top: 0.2rem;

          .chart__review-content-list {
            height: 100%;
            overflow-y: hidden;

            .chart__review-content-item {
              margin-bottom: 0.12rem;
              padding-bottom: 0.13rem;
              border-bottom: 0.01rem dashed rgba(255, 255, 255, 0.2);

              &:last-child {
                border-bottom: none;
                padding-bottom: 0;
                margin-bottom: 0;
              }
            }
          }
        }
      }
    }
  }

  // :deep(.el-select__popper.el-popper) {
  //   top: 1.49rem !important;
  // }
}
</style>
