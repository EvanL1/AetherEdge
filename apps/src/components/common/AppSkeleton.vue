<template>
  <div class="app-skeleton aether-class">
    <div class="app-skeleton__left">
      <div class="app-skeleton__sidebar">
        <div class="app-skeleton__logo" aria-hidden="true">
          <div class="app-skeleton__logo-img" />
        </div>
        <div class="app-skeleton__nav">
          <div v-for="i in 5" :key="i" class="app-skeleton__nav-item skeleton-block" />
        </div>
        <div class="app-skeleton__footer skeleton-block" />
      </div>
    </div>
    <div class="app-skeleton__right">
      <div class="app-skeleton__header">
        <div class="app-skeleton__header-title">Norton Creek Solar Energy Center</div>
        <div class="app-skeleton__header-right">
          <div class="app-skeleton__avatar skeleton-block skeleton-circle" />
          <div class="app-skeleton__notice skeleton-block" />
        </div>
      </div>
      <div class="app-skeleton__content">
        <LoadingBg :loading="true">
          <div class="app-skeleton__content-placeholder" />
        </LoadingBg>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import LoadingBg from './LoadingBg.vue'
</script>

<style lang="scss" scoped>
.app-skeleton {
  height: 100vh;
  width: 100%;
  display: flex;
  background-image: url('@/assets/images/simple-bg.png');
  background-size: 100% 100%;
  background-position: center;
  background-repeat: no-repeat;
}

.app-skeleton__left {
  width: 2.2rem;
  flex-shrink: 0;
}

.app-skeleton__sidebar {
  height: 100%;
  padding: 0.2rem 0;
  background: rgba(84, 98, 140, 0.4);
  border-right: 0.01rem solid rgba(148, 166, 197, 0.36);
  display: flex;
  flex-direction: column;
  gap: 0.2rem;
}

.app-skeleton__logo {
  width: 100%;
  display: flex;
  justify-content: center;
  align-items: center;
  margin: 0.2rem auto;
}

.app-skeleton__logo-img {
  width: 1.56rem;
  height: 0.62rem;
  background-image: url('@/assets/images/aether-logo.svg');
  background-repeat: no-repeat;
  background-position: center;
  background-size: contain;
}

.app-skeleton__nav {
  flex: 1;
  padding: 0 0.2rem;
  display: flex;
  flex-direction: column;
  gap: 0.12rem;
}

.app-skeleton__nav-item {
  height: 0.4rem;
  border-radius: 0.04rem;
}

.app-skeleton__footer {
  width: 0.24rem;
  height: 0.24rem;
  margin: 0 0.2rem 0.2rem auto;
}

.app-skeleton__right {
  flex: 1;
  display: flex;
  flex-direction: column;
  min-width: 0;
}

.app-skeleton__header {
  height: 0.85rem;
  padding: 0 0.2rem;
  display: flex;
  align-items: center;
  justify-content: space-between;
  background: rgba(84, 98, 140, 0.2);
  border-bottom: 0.01rem solid rgba(148, 166, 197, 0.2);
}

.app-skeleton__header-title {
  font-family: Montserrat, sans-serif;
  font-weight: 600;
  font-size: 0.3rem;
  line-height: 150%;
  color: var(--vt-text-primary);
  margin-right: 0.1rem;
}

.app-skeleton__header-right {
  display: flex;
  align-items: center;
  gap: 0.2rem;
}

.app-skeleton__avatar {
  width: 0.32rem;
  height: 0.32rem;
}

.app-skeleton__notice {
  width: 0.32rem;
  height: 0.32rem;
}

.app-skeleton__content {
  flex: 1;
  padding: 0.2rem;
  position: relative;
  min-height: 0;

  .app-skeleton__content-placeholder {
    width: 100%;
    height: 100%;
    min-height: 2rem;
  }
}

/* 骨架屏动画 */
.skeleton-block {
  background: linear-gradient(
    90deg,
    rgba(148, 166, 197, 0.15) 25%,
    rgba(148, 166, 197, 0.35) 50%,
    rgba(148, 166, 197, 0.15) 75%
  );
  background-size: 200% 100%;
  animation: skeleton-shimmer 1.5s ease-in-out infinite;
}

.skeleton-circle {
  border-radius: 50%;
}

@keyframes skeleton-shimmer {
  0% {
    background-position: 200% 0;
  }
  100% {
    background-position: -200% 0;
  }
}
</style>
