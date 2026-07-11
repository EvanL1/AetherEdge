<template>
  <div class="line-chart">
    <div class="line-chart-container" ref="chartRef"></div>
    <div v-if="showToolbox" class="line-chart-toolbox">
      <div v-if="showFullScreen" class="line-chart-toolbox-item" @click="handleFullScreen">
        <el-icon>
          <ZoomIn />
        </el-icon>
      </div>
      <div v-if="showDownload" class="line-chart-toolbox-item" @click="handleExport">
        <el-icon>
          <Download />
        </el-icon>
      </div>
    </div>
    <FullSceenDialog
      ref="fullScreenDialogRef"
      :title="props.title || 'Line Chart Full Screen'"
      fullscreen
      :append-to-body="true"
      :modal-append-to-body="true"
      :close-on-click-modal="false"
    >
      <template #dialog-body>
        <div class="line-chart-full-screen">
          <div class="line-chart-full-screen__container" ref="fullScreenChartRef"></div>
        </div>
      </template>
    </FullSceenDialog>
  </div>
</template>

<script setup lang="ts">
import * as echarts from 'echarts/core'
import { LineChart, BarChart } from 'echarts/charts'
import {
  TooltipComponent,
  GridComponent,
  LegendComponent,
  DataZoomComponent,
  ToolboxComponent,
} from 'echarts/components'
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

echarts.use([
  LineChart,
  BarChart,
  TooltipComponent,
  GridComponent,
  LegendComponent,
  CanvasRenderer,
  DataZoomComponent,
  ToolboxComponent,
])

// 定义数据类型
export interface SeriesData {
  name: string
  data: number[]
  color: string
}

export interface XAxisOption {
  xAxiosData: string[]
  xUnit?: string
}

export interface YAxisOption {
  yUnit?: string
}

// Grid配置接口
export interface GridConfig {
  left?: number
  right?: number
  top?: number
  bottom?: number
}

const props = withDefaults(
  defineProps<{
    xAxiosOption: XAxisOption
    yAxiosOption: YAxisOption
    series: SeriesData[]
    // Grid配置参数
    gridConfig?: GridConfig
    // 全屏模式Grid配置参数
    fullScreenGridConfig?: GridConfig
    // 按钮显示控制
    showToolbox?: boolean
    showFullScreen?: boolean
    showDownload?: boolean
    title?: string
    // 是否展示折线下方的区域
    showArea?: boolean
  }>(),
  {
    // 默认值
    gridConfig: () => ({
      left: 0,
      right: 0,
      top: 45,
      bottom: 10,
    }),
    fullScreenGridConfig: () => ({
      left: 50,
      right: 50,
      top: 90,
      bottom: 50,
    }),
    showToolbox: true,
    showFullScreen: true,
    showDownload: true,
    showArea: false,
  },
)

const chartRef = ref<HTMLDivElement | null>(null)
let chartInstance: echarts.ECharts | null = null

// 通用tooltip formatter，支持自定义大小
function customTooltipFormatter(
  params: any,
  sizeConfig: {
    width: number
    fontSize: number
    itemFontSize: number
    itemLineHeight: number
    dotSize: number
    gap: number
  },
) {
  const { width, fontSize, itemFontSize, itemLineHeight, dotSize, gap } = sizeConfig
  const name = params[0]?.axisValueLabel || params[0]?.name || ''
  let html = `
    <div style="
      max-width:${width}px;
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
  `
  params.forEach((item: any) => {
    html += `
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
            background:${item.color};
            margin-right:${dotSize / 2}px;
          "></span>
          <span>${item.seriesName}</span>
        </div>
        <div style="font-weight:600;">${item.value}${props.yAxiosOption.yUnit ? ' ' + props.yAxiosOption.yUnit : ''}</div>
      </div>
    `
  })
  html += '</div>'
  return html
}

// Grid配置转换函数
function getGridConfig(isFullScreen: boolean) {
  return isFullScreen
    ? {
        left: pxToResponsive(props.fullScreenGridConfig.left || 50),
        right: pxToResponsive(props.fullScreenGridConfig.right || 50),
        top: pxToResponsive(props.fullScreenGridConfig.top || 90),
        bottom: pxToResponsive(props.fullScreenGridConfig.bottom || 50),
      }
    : {
        left: pxToResponsive(props.gridConfig.left || 0),
        right: pxToResponsive(props.gridConfig.right || 0),
        top: pxToResponsive(props.gridConfig.top || 45),
        bottom: pxToResponsive(props.gridConfig.bottom || 15),
      }
}

// 统一生成option的方法
function getChartOption({ isFullScreen = false }: { isFullScreen?: boolean }) {
  // 配置参数
  const xUnit = props.xAxiosOption.xUnit || ''
  const yUnit = props.yAxiosOption.yUnit || ''

  // 背景数据 - 取每个索引位置上的最大值
  const totalData = props.xAxiosOption.xAxiosData.map((_, index: number) => {
    // 获取所有系列在当前位置的值，取最大值
    const valuesAtIndex = props.series.map((s) => s.data[index] || 0)
    return Math.max(...valuesAtIndex)
  })

  // Tooltip样式参数
  const tooltipSize = isFullScreen
    ? {
        width: pxToResponsive(300),
        fontSize: pxToResponsive(32),
        itemFontSize: pxToResponsive(24),
        itemLineHeight: pxToResponsive(32),
        dotSize: pxToResponsive(20),
        gap: pxToResponsive(16),
      }
    : {
        width: pxToResponsive(220),
        fontSize: pxToResponsive(14),
        itemFontSize: pxToResponsive(12),
        itemLineHeight: pxToResponsive(18),
        dotSize: pxToResponsive(8),
        gap: pxToResponsive(8),
      }

  // legend/grid/axis样式参数
  const legend = isFullScreen
    ? {
        icon: 'circle',
        show: true,
        type: 'plain',
        orient: 'horizontal',
        right: pxToResponsive(50),
        top: pxToResponsive(40),
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
        icon: 'circle',
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

  const xAxis = isFullScreen
    ? {
        type: 'category',
        name: xUnit,
        nameTextStyle: {
          color: 'rgba(255, 255, 255, 0.6)',
          fontFamily: 'Arimo',
          fontWeight: 400,
          fontSize: pxToResponsive(16),
          padding: [pxToResponsive(15), 0, 0, 0],
        },
        data: props.xAxiosOption.xAxiosData,
        axisTick: {
          alignWithLabel: true,
          lineStyle: { color: '#fff' },
        },
        axisLine: { show: false },
        axisLabel: {
          color: 'rgba(255, 255, 255, 0.6)',
          fontFamily: 'Arimo',
          fontWeight: 400,
          fontSize: pxToResponsive(16),
        },
        splitLine: { show: false },
        boundaryGap: true,
      }
    : {
        type: 'category',
        name: xUnit,
        nameTextStyle: {
          color: 'rgba(255, 255, 255, 0.6)',
          fontFamily: 'Arimo',
          fontWeight: 400,
          fontSize: pxToResponsive(12),
          padding: [pxToResponsive(10), 0, 0, 0],
        },
        data: props.xAxiosOption.xAxiosData,
        axisTick: {
          alignWithLabel: true,
          lineStyle: { color: '#fff' },
        },
        axisLine: { show: false },
        axisLabel: {
          color: 'rgba(255, 255, 255, 0.6)',
          fontFamily: 'Arimo',
          fontWeight: 400,
          fontSize: pxToResponsive(12),
        },
        splitLine: { show: false },
        boundaryGap: true,
      }

  const yAxis = isFullScreen
    ? {
        type: 'value',
        name: yUnit,
        nameTextStyle: {
          color: 'rgba(255, 255, 255, 0.6)',
          fontFamily: 'Arimo',
          fontWeight: 400,
          fontSize: pxToResponsive(16),
          align: 'right',
          padding: [0, pxToResponsive(12), 0, 0],
        },
        axisLine: { show: false },
        axisTick: { show: false },
        axisLabel: {
          color: 'rgba(255, 255, 255, 0.6)',
          fontFamily: 'Arimo',
          fontWeight: 400,
          fontSize: pxToResponsive(16),
        },
        splitLine: {
          show: true,
          lineStyle: {
            color: '#fff',
            type: 'dashed',
            opacity: 0.2,
          },
        },
      }
    : {
        type: 'value',
        name: yUnit,
        nameTextStyle: {
          color: 'rgba(255, 255, 255, 0.6)',
          fontFamily: 'Arimo',
          fontWeight: 400,
          fontSize: pxToResponsive(12),
          align: 'right',
          padding: [0, pxToResponsive(8), 0, 0],
        },
        axisLine: { show: false },
        axisTick: { show: false },
        axisLabel: {
          color: 'rgba(255, 255, 255, 0.6)',
          fontFamily: 'Arimo',
          fontWeight: 400,
          fontSize: pxToResponsive(12),
        },
        splitLine: {
          show: true,
          lineStyle: {
            color: '#fff',
            type: 'dashed',
            opacity: 0.2,
            width: pxToResponsive(1),
          },
        },
      }

  // series
  const seriesData = [
    {
      name: 'background',
      type: 'bar',
      barWidth: '70%',
      barGap: '-100%',
      itemStyle: {
        color: 'rgba(255,255,255,0)',
      },
      data: totalData,
      showBackground: true,
      backgroundStyle: {
        color: 'rgba(252, 252, 253, 0.04)',
      },
      silent: true,
      emphasis: { disabled: true },
      tooltip: { show: false },
      label: { show: false },
      z: 0,
    },
    ...props.series.map((s: SeriesData) => ({
      name: s.name,
      type: 'line',
      data: s.data,
      smooth: true,
      symbol: 'circle',
      symbolSize: isFullScreen ? pxToResponsive(8) : pxToResponsive(0),
      areaStyle: props.showArea ? {} : undefined,
      lineStyle: {
        color: s.color,
        width: isFullScreen ? pxToResponsive(6) : pxToResponsive(4),
      },
      itemStyle: {
        color: s.color,
        borderColor: s.color,
        borderWidth: isFullScreen ? 3 : 2,
      },
      emphasis: {
        focus: 'series',
        scale: false,
      },
      z: 1,
    })),
  ]

  // tooltip
  const tooltip = isFullScreen
    ? {
        trigger: 'axis',
        confine: true,
        backgroundColor: '#3f4f75',
        borderColor: 'rgba(255,255,255,0.12)',
        borderWidth: pxToResponsive(2),
        padding: [pxToResponsive(30), pxToResponsive(40), pxToResponsive(30), pxToResponsive(40)],
        extraCssText: `
          border-radius: ${pxToResponsive(24)}px;
          box-shadow: 0 ${pxToResponsive(16)}px ${pxToResponsive(32)}px 0 rgba(0,0,0,0.15);
          max-width: ${pxToResponsive(500)}px;
        `,
        textStyle: {
          fontFamily: 'Arimo',
          fontWeight: 400,
          fontSize: pxToResponsive(24),
          color: 'rgba(255,255,255,0.85)',
          lineHeight: pxToResponsive(32),
        },
        axisPointer: {
          type: 'shadow',
          shadowStyle: {
            color: 'rgba(79, 173, 247, 0.08)',
          },
          lineStyle: {
            width: pxToResponsive(2),
          },
        },
        formatter: (params: any) => customTooltipFormatter(params, tooltipSize),
      }
    : {
        trigger: 'axis',
        confine: true,
        backgroundColor: '#3f4f75',
        borderColor: 'rgba(255,255,255,0.12)',
        borderWidth: pxToResponsive(1),
        padding: [pxToResponsive(10), pxToResponsive(16), pxToResponsive(10), pxToResponsive(16)],
        extraCssText: `
          border-radius: ${pxToResponsive(8)}px;
          box-shadow: 0 ${pxToResponsive(4)}px ${pxToResponsive(16)}px 0 rgba(0,0,0,0.12);
          max-width: ${pxToResponsive(220)}px;
        `,
        textStyle: {
          fontFamily: 'Arimo',
          fontWeight: 400,
          fontSize: pxToResponsive(12),
          color: 'rgba(255,255,255,0.85)',
          lineHeight: pxToResponsive(18),
        },
        axisPointer: {
          type: 'shadow',
          shadowStyle: {
            color: 'rgba(79, 173, 247, 0.08)',
          },
          lineStyle: {
            width: pxToResponsive(1),
          },
        },
        formatter: (params: any) => customTooltipFormatter(params, tooltipSize),
      }
  const dataZoom = [
    {
      type: 'inside',
      show: true,
    },
  ]
  const toolbox = isFullScreen
    ? {
        itemSize: pxToResponsive(20),
        itemGap: pxToResponsive(26),
        top: pxToResponsive(-10),
        right: pxToResponsive(40),
        iconStyle: {
          // color: '#fff',
          borderColor: '#fff',
          borderWidth: pxToResponsive(1),
        },
        emphasis: {
          iconStyle: {
            // color: '#fff',
            borderColor: '#fff',
            borderWidth: pxToResponsive(1),
          },
        },
        textStyle: {
          fontFamily: 'Arimo',
          fontWeight: 400,
          fontSize: pxToResponsive(12),
          color: 'rgba(255,255,255,1)',
        },
        feature: {
          myDownload: {
            show: true,
            title: '',
            icon: 'path:// M160 832h704a32 32 0 1 1 0 64H160a32 32 0 1 1 0-64m384-253.696 236.288-236.352 45.248 45.248L508.8 704 192 387.2l45.248-45.248L480 584.704V128h64z',
            onclick: handleExport,
            iconStyle: {
              color: '#fff',
            },
          },
        },
      }
    : {}
  return {
    legend,
    grid,
    tooltip,
    dataZoom,
    xAxis,
    yAxis,
    toolbox,
    series: seriesData,
  }
}

// 初始化echarts
const initChart = () => {
  if (!chartRef.value) return
  if (chartInstance) {
    chartInstance.dispose()
  }
  chartInstance = echarts.init(chartRef.value, {
    renderer: 'canvas',
    devicePixelRatio: window.devicePixelRatio,
  })
  chartInstance.setOption(getChartOption({ isFullScreen: false }))
}

// 初始化全屏图表
const initFullScreenChart = () => {
  if (!fullScreenChartRef.value) return
  if (fullScreenChartInstance) {
    fullScreenChartInstance.dispose()
  }
  fullScreenChartInstance = echarts.init(fullScreenChartRef.value)
  fullScreenChartInstance.setOption(getChartOption({ isFullScreen: true }))
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
  const headers: (string | number)[] = [
    'time',
    ...props.series.map(
      (s: SeriesData) =>
        `${s.name}${props.yAxiosOption.yUnit ? ' (' + props.yAxiosOption.yUnit + ')' : ''}`,
    ),
  ]
  exportData.push(headers)

  // 添加数据
  props.xAxiosOption.xAxiosData.forEach((time: string, index: number) => {
    const row: (string | number)[] = [time]
    props.series.forEach((series: SeriesData) => {
      row.push(series.data[index] || 0)
    })
    exportData.push(row)
  })

  const fileName = `line_chart_data_${new Date().toISOString().slice(0, 10)}.csv`
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
  () => [props.xAxiosOption.xAxiosData, props.series],
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
.line-chart {
  width: 100%;
  height: 100%;
  position: relative;

  .line-chart-container {
    width: 100%;
    height: 100%;
  }

  .line-chart-toolbox {
    position: absolute;
    top: -0.2rem;
    right: 0;
    display: flex;
    align-items: center;
    gap: 0.2rem;

    .line-chart-toolbox-item {
      width: 0.14rem;
      height: 0.14rem;
      cursor: pointer;
    }
  }
}

// 全屏图表样式
.line-chart-full-screen {
  width: 100%;
  height: 100%;
  display: flex;
  align-items: center;
  justify-content: center;
  background: #212c49;
  overflow: hidden;

  .line-chart-full-screen__container {
    width: 100%;
    height: calc(100vh - 1.1rem);
    overflow: hidden;
  }
}
</style>
