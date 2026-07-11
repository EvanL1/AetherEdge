<template>
  <el-button :type="type" class="icon-button" :class="customClass" @click="handleClick">
    <img :src="icon" class="icon-button__icon" :class="iconClass" />
    {{ text }}
  </el-button>
</template>

<script setup lang="ts">
interface Props {
  icon: string
  text: string
  type?: 'primary' | 'success' | 'warning' | 'danger' | 'info'
  customClass?: string
  iconClass?: string
}

interface Emits {
  (e: 'click'): void
}

const props = withDefaults(defineProps<Props>(), {
  type: 'primary',
  customClass: '',
  iconClass: 'icon-button__icon',
})

const emit = defineEmits<Emits>()

const handleClick = () => {
  emit('click')
}
</script>

<style scoped lang="scss">
.aether-class {
  display: flex;
  align-items: center;
  gap: 0.08rem;

  .icon-button__icon {
    width: 0.14rem;
    height: 0.14rem;
    margin-right: 0.08rem;
  }
}
</style>
