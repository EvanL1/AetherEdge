<template>
  <div class="card">
    <div class="card__container">
      <div class="card__left">
        <div class="card__left-icon">
          <img :src="icon" class="card__left-iconImg" />
        </div>
      </div>
      <div class="card__right">
        <div class="card__right-top">
          <div class="card__right-title">{{ title }}</div>
        </div>
        <div class="card__right-bottom">
          <span class="card__right-value">
            {{ props.value }}
            <span v-if="props.unit" class="card__right-unit">&nbsp;{{ props.unit }}</span>
          </span>
        </div>
      </div>
    </div>
  </div>
</template>

<script lang="ts" setup>
import { computed } from 'vue'

const props = defineProps<{
  title?: string
  icon: string
  value?: unknown
  unit?: string
}>()
</script>

<style lang="scss" scoped>
.card {
  width: 100%;
  height: 100%;
  display: flex;
  align-items: center;

  .card__container {
    display: flex;

    .card__left {
      display: flex;
      align-items: center;
      margin-right: 0.23rem;

      .card__left-icon {
        width: 0.536rem;
        height: 0.586rem;
        display: flex;
        align-items: center;
        justify-content: center;

        .card__left-iconImg {
          width: 100%;
          height: 100%;
          object-fit: contain;
        }
      }
    }

    .card__right {
      height: 100%;
      flex: 1;
      display: flex;
      flex-direction: column;
      justify-content: space-between;
      color: rgba(255, 255, 255, 0.6);

      .card__right-top {
        .card__right-title {
          font-weight: 400;
          font-size: 0.16rem;
          line-height: 0.22rem;
        }
      }

      .card__right-bottom {
        margin-top: 0.05rem;
        height: 0.32rem;

        .card__right-value {
          font-weight: 700;
          font-size: 0.3rem;
          line-height: 0.32rem;
          color: #ffffff;

          .card__right-unit {
            font-weight: 700;
            font-size: 0.14rem;
            color: rgba(255, 255, 255, 0.6);
          }
        }
      }
    }
  }
}
</style>
