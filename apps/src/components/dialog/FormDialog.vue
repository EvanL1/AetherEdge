<template>
  <el-dialog
    v-model="dialogVisible"
    :title="null"
    :width="width"
    :modal="true"
    :close-on-click-modal="false"
    :append-to-body="appendToBody"
    :before-close="beforeClose"
    @close="handleClose"
  >
    <!-- dialog-head插槽，默认显示标题 -->
    <template #header>
      <slot name="dialog-head">
        <div class="dialog-head">
          <img class="dialog-head-icon" src="../../assets/icons/card-icon.svg" />
          <span class="dialog-head-title">{{ props.title }}</span>
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
const props = defineProps<{
  title: string
  width: number | string
  appendToBody?: boolean
  beforeClose?: (done: () => void) => void
}>()

const emit = defineEmits<{
  (e: 'close'): void
}>()

const dialogVisible = ref(false)

// 处理关闭事件
const handleClose = () => {
  emit('close')
}

// 暴露方法和属性
defineExpose({
  dialogVisible,
})
</script>

<style lang="scss" scoped>
.dialog-head {
  display: flex;
  align-items: center;

  .dialog-head-icon {
    width: 0.2rem;
    height: 0.2rem;
    margin-right: 0.03rem;
  }

  .dialog-head-title {
    font-weight: 700;
    font-size: 0.18rem;
    line-height: 0.2rem;
    letter-spacing: 0%;
    color: rgba(245, 247, 255, 1);
  }
}
</style>
