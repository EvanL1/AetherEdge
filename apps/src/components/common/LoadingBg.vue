<template>
  <div class="loading-bg" ref="rootRef">
    <slot />
  </div>
</template>
<script setup lang="ts">
import type { Ref } from 'vue'
import { ElLoading } from 'element-plus'

const props = defineProps<{
  loading: boolean
}>()

const loadingInstance: Ref<ReturnType<typeof ElLoading.service> | null> = ref(null)
const rootRef = ref<HTMLElement | null>(null)
const svg = `
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024">
  <path fill="#ff6900" d="M512 64a32 32 0 0 1 32 32v192a32 32 0 0 1-64 0V96a32 32 0 0 1 32-32m0 640a32 32 0 0 1 32 32v192a32 32 0 1 1-64 0V736a32 32 0 0 1 32-32m448-192a32 32 0 0 1-32 32H736a32 32 0 1 1 0-64h192a32 32 0 0 1 32 32m-640 0a32 32 0 0 1-32 32H96a32 32 0 0 1 0-64h192a32 32 0 0 1 32 32M195.2 195.2a32 32 0 0 1 45.248 0L376.32 331.008a32 32 0 0 1-45.248 45.248L195.2 240.448a32 32 0 0 1 0-45.248zm452.544 452.544a32 32 0 0 1 45.248 0L828.8 783.552a32 32 0 0 1-45.248 45.248L647.744 692.992a32 32 0 0 1 0-45.248zM828.8 195.264a32 32 0 0 1 0 45.184L692.992 376.32a32 32 0 0 1-45.248-45.248l135.808-135.808a32 32 0 0 1 45.248 0m-452.544 452.48a32 32 0 0 1 0 45.248L240.448 828.8a32 32 0 0 1-45.248-45.248l135.808-135.808a32 32 0 0 1 45.248 0z"></path>
</svg>
`
const openLoading = () => {
  if (rootRef.value && !loadingInstance.value) {
    loadingInstance.value = ElLoading.service({
      target: rootRef.value,
      customClass: 'loading-bg__mask',
      lock: true,
      background: 'rgba(0,0,0,0.3)',
      text: '',
      spinner: svg,
    })
  }
}

const closeLoading = () => {
  if (loadingInstance.value) {
    loadingInstance.value.close()
    loadingInstance.value = null
  }
}

watch(
  () => props.loading,
  (val) => {
    if (val) {
      nextTick(() => {
        openLoading()
      })
    } else {
      closeLoading()
    }
  },
  { immediate: true },
)

onBeforeUnmount(() => {
  closeLoading()
})
</script>
<style scoped lang="scss">
.loading-bg {
  position: relative;
  inset: 0;
  width: 100%;
  height: 100%;
  min-height: 0.4rem;
  display: flex;
  flex-direction: column;
  // z-index: 999;
}

:deep(.loading-bg__mask) {
  width: 100%;
  height: 100%;
  position: absolute;
  top: 0;
  left: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 9;
}

:deep(.loading-bg__mask .el-loading-spinner) {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
}

:deep(.loading-bg__mask .circular) {
  height: 0.3rem;
  width: 0.3rem;
  animation: loading-rotate 1.2s linear infinite;
}

@keyframes loading-rotate {
  100% {
    transform: rotate(360deg);
  }
}

:deep(.loading-bg__mask .el-loading-text) {
  color: #ff6900;
  font-size: 0.12rem;
  margin: 0.03rem 0;
}
</style>
