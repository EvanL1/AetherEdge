<template>
  <div class="device__content">
    <div class="device__tables">
      <!-- 左侧表格 -->
      <div class="device__table device__table--left">
        <div class="vtable">
          <div class="vtable__header">
            <div class="vtable__cell vtable__cell--name">Name</div>
            <div class="vtable__cell vtable__cell--value">Value</div>
            <div class="vtable__cell vtable__cell--unit">Unit</div>
            <div class="vtable__cell vtable__cell--time">Update Time</div>
          </div>
          <DynamicScroller
            class="vtable__body"
            :items="leftTableData"
            :min-item-size="rowHeight"
            key-field="pointId"
          >
            <template #default="{ item, index }">
              <DynamicScrollerItem :item="item" :active="true" :index="index">
                <div class="vtable__row">
                  <div class="vtable__cell vtable__cell--name">{{ item.name }}</div>
                  <div class="vtable__cell vtable__cell--value">{{ formatNumber(item.value) }}</div>
                  <div class="vtable__cell vtable__cell--unit">{{ item.unit }}</div>
                  <div class="vtable__cell vtable__cell--time">
                    {{ formatTimestamp(item.updateTime) }}
                  </div>
                </div>
              </DynamicScrollerItem>
            </template>
          </DynamicScroller>
        </div>
      </div>
      <!-- 右侧表格 -->
      <div class="device__table device__table--right">
        <div class="vtable">
          <div class="vtable__header">
            <div class="vtable__cell vtable__cell--name">Name</div>
            <div class="vtable__cell vtable__cell--status">Status</div>
            <div class="vtable__cell vtable__cell--time">Update Time</div>
          </div>
          <DynamicScroller
            class="vtable__body"
            :items="rightTableData"
            :min-item-size="rowHeight"
            key-field="pointId"
          >
            <template #default="{ item, index }">
              <DynamicScrollerItem :item="item" :active="true" :index="index">
                <div class="vtable__row">
                  <div class="vtable__cell vtable__cell--name">{{ item.name }}</div>
                  <div class="vtable__cell vtable__cell--status">{{ item.status ?? '-' }}</div>
                  <div class="vtable__cell vtable__cell--time">
                    {{ formatTimestamp(item.updateTime) }}
                  </div>
                </div>
              </DynamicScrollerItem>
            </template>
          </DynamicScroller>
        </div>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted, onUnmounted } from 'vue'
import type { LeftTableItem, RightTableItem } from '@/types/deviceMonitoring'
import { pxToResponsive } from '@/utils/responsive'
import { formatNumber, formatTimestamp } from '@/utils/common'

defineProps<{
  leftTableData: LeftTableItem[]
  rightTableData: RightTableItem[]
}>()

// 行高（与普通表格视觉一致，0.4rem ≈ 40px）
const rowHeight = ref(pxToResponsive(40)) // 使用 px 传递给 RecycleScroller，项目内 100px = 1rem
onMounted(() => {
  // rowHeight.value = pxToResponsive(40)
  window.addEventListener('resize', () => {
    rowHeight.value = pxToResponsive(40)
  })
})
onUnmounted(() => {
  window.removeEventListener('resize', () => {
    rowHeight.value = pxToResponsive(40)
  })
})
</script>

<style scoped lang="scss">
.device__content {
  width: 100%;
  height: 100%;

  .device__tables {
    display: flex;
    flex-direction: row;
    justify-content: space-between;
    width: 100%;
    height: 100%;
    gap: 0.2rem;

    .device__table {
      width: calc((100% - 0.2rem) / 2);
      height: 100%;
    }
  }
}
</style>
