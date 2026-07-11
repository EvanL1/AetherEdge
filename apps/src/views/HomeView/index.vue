<template>
  <div class="aether-class home">
    <!-- <EnergyBgCopy></EnergyBgCopy> -->
    <div class="home-left">
      <div class="home-left-top">
        <EnergyCard
          class="home-left-top-item"
          v-for="item in energyDashboardList"
          :key="item.id"
          :title="item.title"
          :icon="item.icon"
          :value="item.value"
          :unit="item.unit"
        />
      </div>
      <div class="home-left-middle">
        <!-- <img :src="tuopuSvg" alt="">
          -->
        <HomeBg :data="tuopuData"></HomeBg>
      </div>
      <div class="home-left-bottom">
        <div class="home-left-LineChart">
          <ModuleCard title="Power Curve">
            <LineChart
              :xAxiosOption="xAxiosOption"
              :yAxiosOption="lineChartYAxiosOption"
              :series="lineChartSeries"
            />
          </ModuleCard>
        </div>
        <div class="home-left-EnergyChart">
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
    <div class="home-right">
      <div class="home-station">
        <ModuleCard title="Station infomation">
          <div class="home-stationList">
            <div v-for="item in stationInfoList" :key="item.id" class="home-stationItem">
              <EnergyCard
                :title="item.title"
                :icon="item.icon"
                :value="item.value"
                :unit="item.unit"
              />
            </div>
          </div>
        </ModuleCard>
      </div>
      <div class="home-device">
        <ModuleCard title="Device infomation">
          <!-- <div class="home-deviceValue">
              <div class="home-deviceValue-item" v-for="item in deviceInfoList" :key="item.title">
                <span class="deviceValue-item-title">{{ item.title }}:</span>
                <span class="deviceValue-item-value">{{ item.value }}</span>
                &nbsp;
                <span class="deviceValue-item-unit">{{ item.unit }}</span>
              </div>
            </div> -->
          <div class="home-decice-Carousel">
            <el-carousel
              ref="carouselRef"
              :autoplay="false"
              arrow="never"
              indicator-position="none"
              class="home-device-carousel"
            >
              <el-carousel-item
                v-for="(item, index) in deviceInfoList"
                :key="index"
                class="home-device-carousel__item"
              >
                <div class="home-decice-Carousel-item">
                  <div class="home-deviceValue">
                    <div
                      class="home-deviceValue-item"
                      v-for="dataItem in item.data"
                      :key="dataItem.id"
                    >
                      <span class="deviceValue-item-title">{{ dataItem.title }}:</span>
                      <span class="deviceValue-item-value">{{ dataItem.values }}</span>
                      &nbsp;
                      <span class="deviceValue-item-unit">{{ dataItem.unit }}</span>
                    </div>
                  </div>
                  <img :src="item.icon" />
                  <div class="item-name">{{ item.name }}</div>
                </div>
              </el-carousel-item>
            </el-carousel>

            <!-- 自定义左右切换按钮 -->
            <div class="custom-carousel-controls">
              <div class="custom-arrow custom-arrow-left" @click="handlePrev">
                <img :src="arrowLeftImg" alt="Previous" />
              </div>
              <div class="custom-arrow custom-arrow-right" @click="handleNext">
                <img :src="arrowRightImg" alt="Next" />
              </div>
            </div>
          </div>
        </ModuleCard>
      </div>
      <div class="home-alters">
        <ModuleCard title="Alters infomation">
          <div class="home-altersList">
            <div class="home-altersItem" v-for="item in alterInfoList" :key="item.id">
              <div class="alters__item-name">{{ item.deviceName }}</div>
              <img
                v-if="item.alterLevel == 'Critical Alarm'"
                :src="alterL1"
                class="alters__item-icon"
              />
              <img
                v-else-if="item.alterLevel == 'Warning Alarm'"
                :src="alterL2"
                class="alters__item-icon"
              />
              <img
                v-else-if="item.alterLevel == 'Info Alarm'"
                :src="alterL3"
                class="alters__item-icon"
              />
              <div class="alters__item-msg">{{ item.alterMsg }}</div>
            </div>
          </div>
        </ModuleCard>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import type { EnergyCard } from '@/types/home'
import { HOMEPAGE_POINT_IDS } from '@/types/home'
import useWebSocket from '@/composables/useWebSocket'
import { formatNumber } from '@/utils/common'

import ModuleCard from '@/components/card/ModuleCard.vue'
import StackedBarChart from '@/components/charts/StackedBarChart.vue'
import LineChart from '@/components/charts/lineChart.vue'

import alterL1 from '@/assets/icons/home-alter-L1.svg'
import alterL2 from '@/assets/icons/home-alter-L2.svg'
import alterL3 from '@/assets/icons/home-alter-L3.svg'

import arrowLeftImg from '@/assets/icons/arrow-left.svg'
import arrowRightImg from '@/assets/icons/arrow-right.svg'

import devicePV from '@/assets/icons/device-pv.svg'
import deviceDiesel from '@/assets/icons/device-diesel.svg'
import deviceBattery from '@/assets/images/device-battery.png'

import PVEnergy from '@/assets/icons/icon-pv-energy.svg'
import DieselEnergy from '@/assets/icons/icon-diesel-energy.svg'
import EnergyUsed from '@/assets/icons/icon-energy-used.svg'
import SavingBilling from '@/assets/icons/icon-saving-billing.svg'
import ESSEnergyIcon from '@/assets/icons/icon-ess-energy.svg'

import HomeBg from './HomeBg.vue'

/** imgurl 到图标路径（Energy Card / Station 使用，拓扑图与 Device 无视 imgurl） */
const ICON_BY_IMGURL: Record<string, string> = {
  'icon-pv-energy': PVEnergy,
  'icon-diesel-energy': DieselEnergy,
  'icon-energy-used': EnergyUsed,
  'icon-saving-billing': SavingBilling,
  'icon-ess-energy': ESSEnergyIcon,
}
const iconGlob = import.meta.glob<{ default: string }>('@/assets/icons/*.svg', { eager: true })
for (const [path, mod] of Object.entries(iconGlob)) {
  const m = path.match(/([^/\\]+)\.svg$/)
  const url = (mod?.default ?? (mod as any)) as string
  if (m && typeof url === 'string') ICON_BY_IMGURL[m[1]] = url
}
function getIconByImgurl(imgurl?: string): string {
  if (!imgurl) return PVEnergy
  return ICON_BY_IMGURL[imgurl] ?? PVEnergy
}

/** 点位存储：id -> { id, name, value, unit, imgurl }，由 homepage_batch 推送更新 */
interface PointInfo {
  id: number
  name: string
  values: string | number
  unit: string
  imgurl?: string
}
const pointStore = reactive<Record<number, PointInfo>>({})

/** 无数据时展示 '-'，有数据时限制最多三位小数 */
function displayValue(v: string | number | null | undefined): string {
  if (v === null || v === undefined) return '-'
  return formatNumber(v)
}
/** Energy Card / Station 点位默认 name/unit（无 WebSocket 数据时的占位） */
const HOMEPAGE_POINT_DEFAULTS: Record<number, { name: string; unit: string; imgurl?: string }> = {
  1: { name: 'PV Energy', unit: 'kWh', imgurl: 'icon-pv-energy' },
  2: { name: 'Diesel Energy', unit: 'kWh', imgurl: 'icon-diesel-energy' },
  3: { name: 'Energy Used', unit: 'kWh', imgurl: 'icon-energy-used' },
  4: { name: 'Saving Billing', unit: '$', imgurl: 'icon-saving-billing' },
  5: { name: 'PV Power', unit: 'kW', imgurl: 'icon-pv-energy' },
  6: { name: 'Diesel Power', unit: 'kW', imgurl: 'icon-diesel-energy' },
  7: { name: 'ESS Power', unit: 'kW', imgurl: 'icon-ess-energy' },
  8: { name: 'P', unit: 'kW' },
  9: { name: 'U', unit: 'V' },
  10: { name: 'P', unit: 'kW' },
  11: { name: 'U', unit: 'V' },
  12: { name: 'P', unit: 'kW' },
  13: { name: 'U', unit: 'V' },
  14: { name: 'P', unit: 'kW' },
  15: { name: 'P', unit: 'kW' },
  16: { name: 'P', unit: 'kW' },
  17: { name: 'Oil', unit: '%' },
  18: { name: 'P', unit: 'kW' },
  19: { name: 'SOC', unit: '%' },
}

/** Energy Card：id 1-4 */
const energyDashboardList = computed<EnergyCard[]>(() =>
  HOMEPAGE_POINT_IDS.energyCard.map((id) => {
    const p = pointStore[id]
    const def = HOMEPAGE_POINT_DEFAULTS[id]
    return {
      id,
      title: p?.name ?? def?.name,
      icon: getIconByImgurl(p?.imgurl ?? def?.imgurl),
      value: displayValue(p?.values),
      unit: p?.unit ?? def?.unit,
    }
  }),
)

/** Station information：id 5-7 */
const stationInfoList = computed<EnergyCard[]>(() =>
  HOMEPAGE_POINT_IDS.stationInfo.map((id) => {
    const p = pointStore[id]
    const def = HOMEPAGE_POINT_DEFAULTS[id]
    return {
      id,
      title: p?.name ?? def?.name,
      icon: getIconByImgurl(p?.imgurl ?? def?.imgurl),
      value: displayValue(p?.values),
      unit: p?.unit ?? def?.unit,
    }
  }),
)

// 拓朴图数据
const tuopuData = computed(() => {
  const { topology } = HOMEPAGE_POINT_IDS
  const fmt = (p: PointInfo | undefined, defaultName: string, defaultUnit: string) =>
    p
      ? {
          ...p,
          name: p.name ?? defaultName,
          value: displayValue(p.values),
          unit: p.unit ?? defaultUnit,
        }
      : { id: undefined, name: defaultName, value: '-' as const, unit: defaultUnit }
  return {
    pv: {
      P: fmt(
        pointStore[topology.pv.P],
        HOMEPAGE_POINT_DEFAULTS[14].name ?? '',
        HOMEPAGE_POINT_DEFAULTS[14].unit ?? '',
      ),
    },
    load: {
      P: fmt(
        pointStore[topology.load.P],
        HOMEPAGE_POINT_DEFAULTS[15].name ?? '',
        HOMEPAGE_POINT_DEFAULTS[15].unit ?? 'kW',
      ),
    },
    diesel: {
      p: fmt(
        pointStore[topology.diesel.p],
        HOMEPAGE_POINT_DEFAULTS[16].name ?? '',
        HOMEPAGE_POINT_DEFAULTS[16].unit ?? '',
      ),
      oil: fmt(
        pointStore[topology.diesel.oil],
        HOMEPAGE_POINT_DEFAULTS[17].name ?? '',
        HOMEPAGE_POINT_DEFAULTS[17].unit ?? '%',
      ),
    },
    ess: {
      p: fmt(
        pointStore[topology.ess.p],
        HOMEPAGE_POINT_DEFAULTS[18].name ?? '',
        HOMEPAGE_POINT_DEFAULTS[18].unit ?? 'kW',
      ),
      soc: fmt(
        pointStore[topology.ess.soc],
        HOMEPAGE_POINT_DEFAULTS[19].name ?? '',
        HOMEPAGE_POINT_DEFAULTS[19].unit ?? '%',
      ),
    },
  }
})

/** Device information：PV/Diesel/ESS 轮播（拓扑图与 Device 无视 imgurl，使用固定图标） */
interface DevicePointItem {
  id: number
  title: string
  values: string | number
  unit: string
}
interface DeviceSlide {
  data: DevicePointItem[]
  icon: string
  name: string
}
const deviceIcons = [devicePV, deviceDiesel, deviceBattery]
const deviceNames = ['PV', 'Diesel Generator', 'ESS']
const deviceInfoList = computed<DeviceSlide[]>(() =>
  HOMEPAGE_POINT_IDS.deviceInfo.map((ids, idx) => ({
    data: ids.map((pointId) => {
      const p = pointStore[pointId]
      const def = HOMEPAGE_POINT_DEFAULTS[pointId]
      return {
        id: pointId,
        title: p?.name ?? def?.name,
        values: displayValue(p?.values),
        unit: p?.unit ?? def?.unit,
      }
    }),
    icon: deviceIcons[idx],
    name: deviceNames[idx],
  })),
)

/** 应用 homepage_batch 推送数据*/
function applyHomepageBatch(data: {
  updates?: Array<{ id: number; name: string; values?: number; unit: string; imgurl?: string }>
}) {
  const updates = data?.updates ?? []

  for (const u of updates) {
    pointStore[u.id] = {
      id: u.id,
      name: u.name,
      values: u.values !== null && u.values !== undefined ? formatNumber(u.values) : '-',
      unit: u.unit,
      imgurl: u.imgurl,
    }
  }
}

/** 首页 WebSocket 订阅：进入订阅、离开取消 */
useWebSocket(
  {
    source: 'homepage',
    interval: 1000,
  },
  {
    onBatchDataUpdate: (data: any) => {
      if (data?.updates?.length) {
        applyHomepageBatch(data)
      }
    },
  },
)

const alterInfoList = reactive([
  {
    id: 1,
    deviceName: 'ESS',
    alterLevel: 'Critical Alarm',
    alterMsg: 'Battery Overvoltage Alarm',
  },
  {
    id: 2,
    deviceName: 'PV',
    alterLevel: 'Warning Alarm',
    alterMsg: 'Battery Overvoltage Alarm',
  },
  {
    id: 3,
    deviceName: 'Load',
    alterLevel: 'Info Alarm',
    alterMsg: 'Battery Overvoltage Alarm',
  },
])

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

// 新增lineChartSeries变量，专门用于LineChart
const lineChartSeries = [
  {
    name: 'PV',
    data: [10, 35, 20, 80, 60, 180, 120, 300, 180, 250, 90, 40],
    color: 'rgba(105, 203, 255, 1)',
  },
  {
    name: 'ESS',
    data: [500, 420, 480, 350, 370, 320, 400, 220, 300, 120, 200, 60],
    color: 'rgba(29, 134, 255, 1)',
  },
]

// exampleSeries 仍然保留给 StackedBarChart 使用
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
const xAxiosOption = {
  xAxiosData: exampleXAxisData,
}
const yAxiosOption = {
  yUnit: 'kWh',
}
const lineChartYAxiosOption = {
  yUnit: 'kW',
}

// Carousel引用
const carouselRef = ref()

// 切换到上一张
const handlePrev = () => {
  carouselRef.value?.prev()
}

// 切换到下一张
const handleNext = () => {
  console.log('next')

  carouselRef.value?.next()
}
</script>

<style scoped lang="scss">
.home {
  position: relative;
  width: 100%;
  height: 100%;
  display: flex;
  justify-content: space-between;
  z-index: 2;

  &::before {
    content: '';
    position: absolute;
    top: -0.2rem;
    left: -0.2rem;
    width: calc(100% + 0.4rem);
    height: calc(100% + 0.4rem);
    background: url('@/assets/images/home-bg.png') no-repeat center center;
    background-size: 100% 100%;
    z-index: 1;
  }

  .home-left {
    position: relative;
    z-index: 2;
    width: calc(100% - 3.9rem);
    height: 100%;
    margin-right: 0.2rem;
    display: flex;
    flex-direction: column;
    justify-content: space-between;

    .home-left-top {
      width: 100%;
      height: 0.8rem;
      padding-top: 0.1rem;
      display: flex;
      justify-content: space-between;
      z-index: 1;

      .home-left-top-item {
        height: 0.7rem;
      }
    }

    .home-left-middle {
      width: 100%;
      height: calc(69% - 1.2rem);
      flex: 1;
      // background-image: url('@/assets/images/tuopu.png');
      // background-size: 100% 100%;
      // background-repeat: no-repeat;
      // background-position: center;

      img {
        width: 100%;
        height: 100%;
        object-fit: contain;
      }
    }

    .home-left-bottom {
      width: 100%;
      height: 30.89%;
      display: flex;
      justify-content: space-between;

      .home-left-EnergyChart {
        width: calc((100% - 0.2rem) / 2);
        height: 100%;
      }

      .home-left-LineChart {
        width: calc((100% - 0.2rem) / 2);
        height: 100%;
      }
    }
  }

  .home-right {
    position: relative;
    z-index: 2;

    width: 3.7rem;
    height: 100%;
    display: flex;
    flex-direction: column;
    justify-content: space-between;
    gap: 0.2rem;

    .home-station {
      width: 100%;
      height: 36.75%;

      .home-stationList {
        height: 100%;
        padding-top: 0.2rem;

        .home-stationItem {
          height: 33.33%;
          padding-top: 0.12rem;
          padding-bottom: 0.13rem;
          border-bottom: 0.01rem dashed rgba(255, 255, 255, 0.2);

          &:last-child {
            border-bottom: none;
            // padding-bottom: 0;
            margin-bottom: 0;
          }
        }
      }
    }

    .home-device {
      width: 100%;
      height: 28.27%;

      .home-decice-Carousel {
        height: 100%;
        width: 100%;

        .home-decice-Carousel-item {
          height: 100%;
          width: 100%;
          display: flex;
          flex-direction: column;
          align-items: center;
          justify-content: center;

          .home-deviceValue {
            width: 100%;
            padding: 0.15rem 0;
            margin-bottom: 0.2rem;
            border-bottom: 0.01rem dashed rgba(255, 255, 255, 0.2);
            display: flex;
            justify-content: space-between;

            .home-deviceValue-item {
              width: 50%;
              display: flex;
              align-items: flex-end;
              justify-content: center;
              font-size: 0.16rem;
              font-weight: 400;
              color: rgba(255, 255, 255, 0.6);
              height: 0.16rem;

              .deviceValue-item-title {
                font-size: 0.16rem;
                font-weight: 600;
                margin-right: 0.09rem;
              }

              .deviceValue-item-value {
                font-size: 0.22rem;
                font-weight: 700;
                color: #fff;
                line-height: 0.26rem;
              }

              .deviceValue-item-unit {
                font-size: 0.14rem;
                font-weight: 400;
              }
            }
          }

          img {
            width: 1.2rem;
            height: 0.73rem;
            object-fit: contain;
            margin-bottom: 0.05rem;
          }

          .item-name {
            font-size: 0.18rem;
            font-weight: 500;
            line-height: 100%;
            letter-spacing: 0%;
          }
        }
      }
    }

    .home-alters {
      height: 30.89%;
      width: 100%;

      .home-altersList {
        height: 100%;
        overflow-y: scroll;
        // 默认隐藏滚动条
        scrollbar-width: none;
        /* Firefox */
        -ms-overflow-style: none;
        /* IE and Edge */

        // Webkit浏览器隐藏滚动条
        &::-webkit-scrollbar {
          width: 0;
          height: 0;
        }

        // 鼠标悬停时显示滚动条
        &:hover {
          scrollbar-width: auto;
          /* Firefox */
          -ms-overflow-style: auto;
          /* IE and Edge */

          &::-webkit-scrollbar {
            width: 0.04rem;
            height: 0.04rem;
          }

          &::-webkit-scrollbar-thumb {
            border-radius: 0.02rem;
          }
        }

        .home-altersItem {
          min-height: 0.9rem;
          border-bottom: 0.01rem dashed rgba(255, 255, 255, 0.2);
          display: flex;
          align-items: center;

          .alters__item-name {
            width: 0.4rem;
            font-size: 0.16rem;
            font-weight: 700;
            line-height: 0.16rem;
            margin-right: 0.17rem;
          }

          .alters__item-icon {
            width: 0.46rem;
            height: 0.2rem;
            object-fit: contain;
            margin-right: 0.1rem;
          }

          .alters__item-msg {
            font-size: 0.14rem;
            line-height: 0.16rem;
            font-weight: 400;

            &:last-child {
              border-bottom: none;
            }
          }

          &:last-child {
            border-bottom: none;
          }
        }
      }
    }
  }
}

:deep(.home-device-carousel.el-carousel),
:deep(.home-device-carousel .el-carousel__container),
:deep(.home-device-carousel__item.el-carousel__item) {
  height: 100%;
}

:deep(.home-device-carousel__item.el-carousel__item) {
  width: 100%;
  display: flex;
  align-items: center;
  justify-content: center;
}

// 自定义carousel控制按钮样式
.custom-carousel-controls {
  width: 100%;
  height: 0.32rem;
  position: absolute;
  top: 50%;
  left: 0;
  right: 0;
  transform: translateY(-50%);
  z-index: 999;
}

.custom-arrow {
  position: absolute;
  width: 0.32rem;
  height: 0.32rem;
  cursor: pointer;

  img {
    width: 0.32rem;
    height: 0.32rem;
    object-fit: contain;
  }

  // &:hover {
  //   background-color: rgba(84, 98, 140, 1);
  // }
}

.custom-arrow-left {
  left: 0.1rem;
}

.custom-arrow-right {
  right: 0.1rem;
}
</style>
