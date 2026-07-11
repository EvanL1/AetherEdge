<template>
  <div class="aether-class main-layout">
    <div class="main-layout__left">
      <!-- 侧边导航栏 -->
      <Sidebar />
    </div>
    <div class="main-layout__right" :class="{ collapse: globalStore.isCollapse }">
      <!-- 头部 -->
      <Header />
      <!-- 主内容区域 -->
      <main class="main-layout__content" :class="{ collapse: globalStore.isCollapse }">
        <router-view />
      </main>
    </div>
  </div>
</template>

<script setup lang="ts">
import Sidebar from './Sidebar.vue'
import Header from './Header.vue'
import { useGlobalStore } from '@/stores/global'
// 定义响应式变量，动态切换背景色
const route = useRoute()
const isHomePage: Ref<boolean> = computed(() => {
  return route.name === 'home' ? true : false
})

const globalStore = useGlobalStore()
</script>

<style lang="scss" scoped>
.aether-class.main-layout {
  height: 100%;
  width: 100%;
  display: flex;
  background-size: 100% 100%;
  background-position: center;
  background-repeat: no-repeat;
  // background: transparent;
  background-image: url('../assets/images/simple-bg.png');

  // z-index: -99;
  .main-layout__left {
    z-index: 99;
  }

  .main-layout__right {
    transition: width 0.3s ease-in-out;
    width: calc(100% - 2.2rem);

    // z-index: 99;
    &.collapse {
      width: calc(100% - 0.85rem);
    }

    .main-layout__content {
      padding: 0.2rem;
      height: calc(100% - 0.85rem);
      width: 100%;
    }
  }
}
</style>
