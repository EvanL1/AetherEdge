<template>
  <div class="doughnut-chart">
    <div class="doughnut-chart-container" ref="chartRef"></div>
    <div v-if="showToolbox" class="doughnut-chart-toolbox">
      <div v-if="showFullScreen" class="doughnut-chart-toolbox-item" @click="handleFullScreen">
        <el-icon>
          <ZoomIn />
        </el-icon>
      </div>
      <div v-if="showDownload" class="doughnut-chart-toolbox-item" @click="handleExport">
        <el-icon>
          <Download />
        </el-icon>
      </div>
    </div>
    <FullSceenDialog
      ref="fullScreenDialogRef"
      title="Doughnut Chart Full Screen"
      fullscreen
      :append-to-body="true"
      :modal-append-to-body="true"
      :close-on-click-modal="false"
    >
      <template #dialog-body>
        <div class="doughnut-chart-full-screen">
          <div class="doughnut-chart-full-screen__container" ref="fullScreenChartRef"></div>
        </div>
      </template>
    </FullSceenDialog>
  </div>
</template>

<script setup lang="ts">
import * as echarts from 'echarts/core'
import { PieChart } from 'echarts/charts'
import { TooltipComponent, LegendComponent, TitleComponent } from 'echarts/components'
import { CanvasRenderer } from 'echarts/renderers'
import { useGlobalStore } from '@/stores/global'
import { pxToResponsive } from '@/utils/responsive'
import FullSceenDialog from '@/components/dialog/fullSceenDialog.vue'
import { ZoomIn, Download } from '@element-plus/icons-vue'
import { downloadCsv } from '@/utils/csv'

const fullScreenDialogRef = ref()
const fullScreenChartRef = ref<HTMLDivElement | null>(null)
const globalStore = useGlobalStore()
let fullScreenChartInstance: echarts.ECharts | null = null

// 监听侧边栏折叠状态变化
watch(
  () => globalStore.isCollapse,
  () => {
    // 延迟重新绘制，确保DOM更新完成
    nextTick(() => {
      setTimeout(() => {
        chartInstance?.dispose()
        initChart()
      }, 300)
    })
  },
)

echarts.use([PieChart, TooltipComponent, LegendComponent, TitleComponent, CanvasRenderer])

// 定义数据类型
interface SeriesData {
  name: string
  value: number
  color: string
}

// Grid配置接口
interface GridConfig {
  left?: number
  right?: number
  top?: number
  bottom?: number
}

const props = withDefaults(
  defineProps<{
    series: SeriesData[]
    title?: string
    radius?: [string, string] // 内半径和外半径，如 ['40%', '70%']
    center?: [string, string] // 圆心位置，如 ['50%', '50%']
    // Grid配置参数
    gridConfig?: GridConfig
    // 全屏模式Grid配置参数
    fullScreenGridConfig?: GridConfig
    // 按钮显示控制
    showToolbox?: boolean
    showFullScreen?: boolean
    showDownload?: boolean
  }>(),
  {
    // 默认值
    gridConfig: () => ({
      left: 0,
      right: 0,
      top: 55,
      bottom: 10,
    }),
    fullScreenGridConfig: () => ({
      left: 50,
      right: 50,
      top: 80,
      bottom: 50,
    }),
    showToolbox: true,
    showFullScreen: true,
    showDownload: true,
  },
)

const chartRef = ref<HTMLDivElement | null>(null)
let chartInstance: echarts.ECharts | null = null

// 通用tooltip formatter，支持自定义大小
function customTooltipFormatter(
  params: any,
  sizeConfig: {
    width: number
    minHeight: number
    fontSize: number
    itemFontSize: number
    itemLineHeight: number
    dotSize: number
    gap: number
  },
) {
  const { width, minHeight, fontSize, itemFontSize, itemLineHeight, dotSize, gap } = sizeConfig
  const name = params.name || ''
  const value = params.value || 0
  const percent = params.percent || 0

  const html = `
    <div style="
      max-width:${width}px;
      min-height:${minHeight}px;
      display:flex;
      flex-direction:column;
      gap:${gap}px;
    ">
      <div style="
        color:rgba(255,255,255,0.85);
        font-size:${fontSize}px;
        font-family:Arimo;
        font-weight:600;
        width:100%;
        margin-bottom:${gap / 2}px;
      ">${name}</div>
      <div style="
        display:flex;
        align-items:center;
        justify-content:space-between;
        font-size:${itemFontSize}px;
        font-family:Arimo;
        color:rgba(255,255,255,0.85);
        line-height:${itemLineHeight}px;
        margin-bottom:${gap / 4}px;
        gap:${gap * 2}px;
      ">
        <div style="display:flex;align-items:center;gap:${gap / 2}px;">
          <span style="
            display:inline-block;
            width:${dotSize}px;
            height:${dotSize}px;
            border-radius:50%;
            background:${params.color};
            margin-right:${dotSize / 2}px;
          "></span>
          <span>数值</span>
        </div>
        <div style="font-weight:600;">${value}</div>
      </div>
      <div style="
        display:flex;
        align-items:center;
        justify-content:space-between;
        font-size:${itemFontSize}px;
        font-family:Arimo;
        color:rgba(255,255,255,0.85);
        line-height:${itemLineHeight}px;
        gap:${gap * 2}px;
      ">
        <span>占比</span>
        <div style="font-weight:600;">${percent}%</div>
      </div>
    </div>
  `
  return html
}

function getGridConfig(isFullScreen: boolean) {
  return isFullScreen
    ? {
        left: pxToResponsive(props.gridConfig.left || 0),
        right: pxToResponsive(props.gridConfig.right || 0),
        top: pxToResponsive(props.gridConfig.top || 45),
        bottom: pxToResponsive(props.gridConfig.bottom || 15),
      }
    : {
        left: pxToResponsive(props.fullScreenGridConfig.left || 0),
        right: pxToResponsive(props.fullScreenGridConfig.right || 0),
        top: pxToResponsive(props.fullScreenGridConfig.top || 45),
        bottom: pxToResponsive(props.fullScreenGridConfig.bottom || 15),
      }
}

// 统一生成option的方法
function getChartOption({
  isFullScreen = false,
  chartWidth = 600,
}: {
  isFullScreen?: boolean
  chartWidth?: number
}) {
  // 获取配置参数，使用默认值
  const title = props.title || ''
  const radius = props.radius || ['40%', '70%']
  const center = props.center || ['50%', '50%']

  // Tooltip样式参数
  const tooltipSize = isFullScreen
    ? {
        width: pxToResponsive(300),
        minHeight: pxToResponsive(120),
        fontSize: pxToResponsive(32),
        itemFontSize: pxToResponsive(24),
        itemLineHeight: pxToResponsive(32),
        dotSize: pxToResponsive(20),
        gap: pxToResponsive(16),
      }
    : {
        width: pxToResponsive(220),
        minHeight: pxToResponsive(100),
        fontSize: pxToResponsive(14),
        itemFontSize: pxToResponsive(12),
        itemLineHeight: pxToResponsive(18),
        dotSize: pxToResponsive(8),
        gap: pxToResponsive(8),
      }

  // legend/grid/axis样式参数
  const legend = isFullScreen
    ? {
        show: true,
        type: 'plain',
        orient: 'horizontal',
        right: pxToResponsive(50),
        top: pxToResponsive(30),
        itemWidth: pxToResponsive(20),
        itemHeight: pxToResponsive(20),
        itemGap: pxToResponsive(40),
        textStyle: {
          color: 'rgba(255, 255, 255, 0.6)',
          fontSize: pxToResponsive(18),
          fontFamily: 'Arimo',
          fontWeight: 400,
        },
        data: props.series.map((s: SeriesData) => s.name),
      }
    : {
        show: true,
        type: 'plain',
        orient: 'horizontal',
        right: 0,
        top: pxToResponsive(10),
        itemWidth: pxToResponsive(12),
        itemHeight: pxToResponsive(12),
        itemGap: pxToResponsive(25),
        textStyle: {
          color: 'rgba(255, 255, 255, 0.6)',
          fontSize: pxToResponsive(12),
          fontFamily: 'Arimo',
          fontWeight: 400,
        },
        data: props.series.map((s: SeriesData) => s.name),
      }

  const grid = getGridConfig(isFullScreen)

  // tooltip
  const tooltip = isFullScreen
    ? {
        trigger: 'item',
        confine: true,
        backgroundColor: '#3f4f75',
        borderColor: 'rgba(255,255,255,0.12)',
        borderWidth: pxToResponsive(2),
        padding: [pxToResponsive(30), pxToResponsive(40), pxToResponsive(30), pxToResponsive(40)],
        extraCssText: `
          border-radius: ${pxToResponsive(24)}px;
          box-shadow: 0 ${pxToResponsive(16)}px ${pxToResponsive(32)}px 0 rgba(0,0,0,0.15);
          max-width: ${pxToResponsive(500)}px;
          min-height: ${pxToResponsive(120)}px;
        `,
        textStyle: {
          fontFamily: 'Arimo',
          fontWeight: 400,
          fontSize: pxToResponsive(24),
          color: 'rgba(255,255,255,0.85)',
          lineHeight: pxToResponsive(32),
        },
        formatter: (params: any) => customTooltipFormatter(params, tooltipSize),
      }
    : {
        trigger: 'item',
        confine: true,
        backgroundColor: '#3f4f75',
        borderColor: 'rgba(255,255,255,0.12)',
        borderWidth: pxToResponsive(1),
        padding: [pxToResponsive(10), pxToResponsive(16), pxToResponsive(10), pxToResponsive(16)],
        extraCssText: `
          border-radius: ${pxToResponsive(8)}px;
          box-shadow: 0 ${pxToResponsive(4)}px ${pxToResponsive(16)}px 0 rgba(0,0,0,0.12);
          max-width: ${pxToResponsive(220)}px;
          min-height: ${pxToResponsive(100)}px;
        `,
        textStyle: {
          fontFamily: 'Arimo',
          fontWeight: 400,
          fontSize: pxToResponsive(12),
          color: 'rgba(255,255,255,0.85)',
          lineHeight: pxToResponsive(18),
        },
        formatter: (params: any) => customTooltipFormatter(params, tooltipSize),
      }

  const titleConfig = isFullScreen
    ? {
        text: title,
        left: 'center',
        top: pxToResponsive(20),
        textStyle: {
          color: 'rgba(255, 255, 255, 0.8)',
          fontFamily: 'Arimo',
          fontWeight: 500,
          fontSize: pxToResponsive(24),
        },
      }
    : {
        text: title,
        left: 'center',
        top: pxToResponsive(10),
        textStyle: {
          color: 'rgba(255, 255, 255, 0.8)',
          fontFamily: 'Arimo',
          fontWeight: 500,
          fontSize: pxToResponsive(14),
        },
      }

  const series = [
    {
      name: title || '数据分布',
      type: 'pie',
      radius: radius,
      center: center,
      data: props.series.map((s: SeriesData) => ({
        name: s.name,
        value: s.value,
        itemStyle: {
          color: s.color,
        },
      })),
      label: {
        show: true,
        position: 'outside',
        formatter: '{b}\n{d}%',
        color: 'rgba(255, 255, 255, 0.8)',
        fontSize: isFullScreen ? pxToResponsive(18) : pxToResponsive(12),
        fontFamily: 'Arimo',
        fontWeight: 400,
      },
      labelLine: {
        show: true,
        lineStyle: {
          color: 'rgba(255, 255, 255, 0.3)',
          width: isFullScreen ? 2 : 1,
        },
      },
      emphasis: {
        scale: true,
        scaleSize: isFullScreen ? pxToResponsive(8) : pxToResponsive(5),
        label: {
          fontSize: isFullScreen ? pxToResponsive(20) : pxToResponsive(14),
          fontWeight: 600,
        },
      },
      animationType: 'scale',
      animationEasing: 'elasticOut',
      animationDelay: (idx: number) => Math.random() * 200,
    },
  ]

  return {
    title: titleConfig,
    grid,
    legend,
    tooltip,
    series,
  }
}

// 初始化echarts
const initChart = () => {
  if (!chartRef.value) return
  if (chartInstance) {
    chartInstance.dispose()
  }
  const chartWidth = chartRef.value.clientWidth || 600
  chartInstance = echarts.init(chartRef.value)
  chartInstance.setOption(getChartOption({ isFullScreen: false, chartWidth }))
}

// 初始化全屏图表
const initFullScreenChart = () => {
  if (!fullScreenChartRef.value) return
  if (fullScreenChartInstance) {
    fullScreenChartInstance.dispose()
  }
  const chartWidth = fullScreenChartRef.value.clientWidth || 1200
  fullScreenChartInstance = echarts.init(fullScreenChartRef.value)
  fullScreenChartInstance.setOption(getChartOption({ isFullScreen: true, chartWidth }))
}

const handleFullScreen = () => {
  fullScreenDialogRef.value.dialogVisible = true
  nextTick(() => {
    setTimeout(() => {
      initFullScreenChart()
    }, 100)
  })
}

const handleExport = () => {
  // 准备导出数据
  const exportData: (string | number)[][] = []

  // 添加表头
  const headers: (string | number)[] = ['name', 'value', 'percentage']
  exportData.push(headers)

  // 计算总数用于计算百分比
  const total = props.series.reduce((sum: number, item: SeriesData) => sum + item.value, 0)

  // 添加数据
  props.series.forEach((item: SeriesData) => {
    const percentage = total > 0 ? ((item.value / total) * 100).toFixed(2) : '0.00'
    exportData.push([item.name, item.value, `${percentage}%`])
  })

  const fileName = `doughnut_chart_data_${new Date().toISOString().slice(0, 10)}.csv`
  downloadCsv(exportData, fileName)
}

// 监听侧边栏折叠状态变化
watch(
  () => globalStore.isCollapse,
  () => {
    nextTick(() => {
      setTimeout(() => {
        chartInstance?.dispose()
        initChart()
      }, 300)
    })
  },
)

// 监听窗口大小变化，重新调整全屏图表
const resizeFullScreenChart = () => {
  if (fullScreenChartInstance && fullScreenDialogRef.value.dialogVisible) {
    setTimeout(() => {
      fullScreenChartInstance?.resize()
    }, 300)
  }
}

const resizeChart = () => {
  setTimeout(() => {
    chartInstance?.resize()
  }, 300)
}

watch(
  () => [props.series, props.title, props.radius, props.center],
  () => {
    initChart()
  },
  { deep: true },
)

onMounted(() => {
  initChart()
  window.addEventListener('resize', resizeChart)
  window.addEventListener('resize', resizeFullScreenChart)
})

onBeforeUnmount(() => {
  window.removeEventListener('resize', resizeChart)
  window.removeEventListener('resize', resizeFullScreenChart)
  chartInstance?.dispose()
  fullScreenChartInstance?.dispose()
})
</script>

<style scoped lang="scss">
.doughnut-chart {
  width: 100%;
  height: 100%;
  position: relative;

  .doughnut-chart-container {
    width: 100%;
    height: 100%;
  }

  .doughnut-chart-toolbox {
    position: absolute;
    top: -0.2rem;
    right: 0;
    display: flex;
    align-items: center;
    gap: 0.1rem;

    .doughnut-chart-toolbox-item {
      width: 0.3rem;
      height: 0.3rem;
      cursor: pointer;
    }
  }
}

// 全屏图表样式
.doughnut-chart-full-screen {
  width: 100%;
  height: 100%;
  display: flex;
  align-items: center;
  justify-content: center;
  background: #212c49;
  overflow: hidden;

  .doughnut-chart-full-screen__container {
    width: 100%;
    height: calc(100vh - 1.1rem);
    overflow: hidden;
  }
}
</style>
