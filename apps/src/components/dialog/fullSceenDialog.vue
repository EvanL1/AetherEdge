<template>
  <el-dialog
    v-model="dialogVisible"
    :title="null"
    :modal="true"
    :close-on-click-modal="false"
    :append-to-body="true"
    :modal-append-to-body="true"
    fullscreen
  >
    <!-- dialog-head插槽，默认显示标题 -->
    <template #header>
      <slot name="dialog-head">
        <div class="dialog-head-container">
          <img class="dialog-head-icon" :src="cardIcon" />
          <span>{{ props.title }}</span>
        </div>
      </slot>
    </template>

    <!-- dialog-body插槽，默认显示表单内容 -->
    <template #default>
      <slot name="dialog-body"> </slot>
    </template>

    <!-- dialog-footer插槽，默认显示底部按钮 -->
    <template #footer>
      <slot name="dialog-footer"> </slot>
    </template>
  </el-dialog>
</template>

<script setup lang="ts">
import cardIcon from '@/assets/icons/card-icon.svg'
const props = defineProps<{
  title: string
}>()
const dialogVisible = ref(false)
// 暴露方法和属性
defineExpose({
  dialogVisible,
})
</script>

<style scoped lang="scss">
.dialog-head-container {
  display: flex;
  align-items: center;
  // justify-content: center;

  .dialog-head-icon {
    width: 0.2rem;
    height: 0.2rem;
    margin-right: 0.05rem;
  }

  .dialog-head-title {
    font-weight: 700;
    font-size: 18px;
    line-height: 100%;
    letter-spacing: 0%;
    color: #fff;
  }
}
</style>
