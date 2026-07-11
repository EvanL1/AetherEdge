<template>
  <div class="aether-class sidebar" :class="{ collapse: globalStore.isCollapse }">
    <div class="sidebar__header" :class="{ collapse: globalStore.isCollapse }">
      <div class="sidebar__header-img" :class="{ collapse: globalStore.isCollapse }"></div>
    </div>

    <nav class="sidebar__nav">
      <el-menu
        :collapse="globalStore.isCollapse"
        class="sidebar__menu"
        :default-active="activeMenuPath"
        router
        background-color="transparent"
        text-color="#fff"
        active-text-color="#fff"
        :unique-opened="true"
      >
        <template v-for="item in filterRoutesList" :key="item.path">
          <el-sub-menu
            v-if="item.meta.isSubMenu"
            :index="item.meta.activeNav"
            class="sidebar__subMenu"
          >
            <template #title>
              <img
                v-if="item.meta.icon"
                :src="item.meta.icon"
                class="sidebar__subMenu-img"
                :class="{ collapse: globalStore.isCollapse }"
                alt=""
              />
              <span class="sidebar__subMenu-title">{{ item.meta.title }}</span>
            </template>
            <el-menu-item
              v-for="child in item.children"
              :key="child.path"
              :index="child.meta.activeNav"
            >
              <span>{{ child.meta.title }}</span>
            </el-menu-item>
          </el-sub-menu>
          <el-menu-item v-else :index="item.meta.activeNav" class="sidebar__menu-item">
            <img
              v-if="item.meta.icon"
              :src="item.meta.icon"
              class="sidebar__subMenu-img"
              :class="{ collapse: globalStore.isCollapse }"
              alt=""
            />
            <span class="sidebar__menu-text">{{ item.meta.title }}</span>
          </el-menu-item>
        </template>
      </el-menu>
    </nav>

    <div class="sidebar__footer" :class="{ collapse: globalStore.isCollapse }">
      <div
        class="sidebar__footer-shrink"
        :class="{ collapse: globalStore.isCollapse }"
        @click="handleShrink"
      ></div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { useGlobalStore } from '@/stores/global'
import { useRoute } from 'vue-router'
import { getFilteredRoutesForSidebar } from '@/router/injector'
const globalStore = useGlobalStore()
const route = useRoute()
const filterRoutesList = computed(() => getFilteredRoutesForSidebar())
const activeMenuPath = ref<string>('/home')

// 监听路由变化，更新激活的菜单
watch(
  route,
  (newPath) => {
    // console.log(newPath)
    activeMenuPath.value = newPath.meta.activeNav as string
  },
  { immediate: true },
)

const handleShrink = () => {
  globalStore.isCollapse = !globalStore.isCollapse
}
</script>

<style lang="scss" scoped>
.aether-class.sidebar {
  position: relative;
  height: 100vh;
  padding: 0.2rem 0;
  background: rgba(84, 98, 140, 0.4);
  border-right: 0.01rem solid;
  border-image-source: linear-gradient(
    147.24deg,
    rgba(148, 166, 197, 0.72) 39.16%,
    rgba(148, 166, 197, 0.36) 66.27%,
    rgba(148, 166, 197, 0.72) 98.58%
  );
  backdrop-filter: blur(0.1rem);
  display: flex;
  flex-direction: column;

  transition: width 0.3s ease-in-out;
  width: 2.2rem;

  &.collapse {
    width: 0.85rem;
  }

  .sidebar__header {
    width: 100%;
    display: flex;
    justify-content: center;
    align-items: center;
    margin: 0.2rem auto;

    .sidebar__header-img {
      background-repeat: no-repeat;
      background-image: url('@/assets/images/aether-logo.svg');
      width: 1.56rem;
      height: 0.62rem;
      background-position: center;
      background-size: contain;

      &.collapse {
        background-image: url('@/assets/images/aether-logo-compact.svg');
        width: 0.5rem;
        height: 0.5rem;
        background-size: contain;
      }
    }
  }

  .sidebar__logo {
    display: flex;
    align-items: center;
    gap: 0.12rem;

    .sidebar__logo-text {
      font-size: 0.18rem;
      font-weight: 600;
      color: #ffffff;
      font-family: 'Montserrat', sans-serif;
    }
  }

  .sidebar__nav {
    flex: 1;
    padding: 0.2rem;

    .sidebar__menu-item {
    }
  }

  .sidebar__subMenu-img {
    width: 0.24rem;
    height: 0.24rem;
    margin-right: 0.1rem; // 默认展开

    &.collapse {
      margin-right: 0;
    }
  }

  .sidebar__menu-text {
    font-weight: 700;
    font-size: 0.14rem;
  }

  .sidebar__subMenu-title {
    font-family: Arimo;
    font-weight: 700;
    font-style: Bold;
    font-size: 0.14rem;
    letter-spacing: 0%;
    color: #fff;
  }

  .sidebar__footer {
    position: relative;
    right: 0;
    bottom: 0;
    padding: 0 0.2rem;
    display: flex;
    flex-direction: row-reverse;
    transition: all 0.3s ease;
    justify-content: flex-start;
    align-items: center;

    &.collapse {
      justify-content: center;
      align-items: center;
    }

    .sidebar__footer-shrink {
      background-image: url('@/assets/icons/sidebar-shrink.svg');
      background-size: 100% 100%;
      background-repeat: no-repeat;
      background-position: center;
      width: 0.24rem;
      height: 0.24rem;
      cursor: pointer;

      &.collapse {
        background-image: url('@/assets/icons/sidebar-grow.svg');
      }
    }
  }
}
</style>
