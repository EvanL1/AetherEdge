<template>
  <div class="card">
    <div class="card__container">
      <div class="card__left">
        <div class="card__left-icon">
          <img :src="icon" class="card__left-iconImg" />
        </div>
      </div>
      <div class="card__right">
        <div class="card__right-contain">
          <div class="card__right-top">
            <div class="card__right-title">{{ props.title }}</div>
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
  </div>
</template>

<script lang="ts" setup>
import { computed } from 'vue'

const props = defineProps({
  title: {
    type: String,
    required: true,
  },
  icon: {
    type: String,
    required: true,
  },
  value: {
    type: [String, Number],
    required: true,
  },
  unit: {
    type: String,
    default: '',
  },
})
</script>

<style lang="scss" scoped>
.card {
  width: 100%;
  height: 100%;
  background-color: rgba(84, 98, 140, 0.2);
  padding: 0.2rem;
  display: flex;
  align-items: center;
  border: 0.01rem solid;

  border-image: linear-gradient(
      116.72deg,
      rgba(148, 166, 197, 0.3) 2.77%,
      rgba(148, 166, 197, 0) 32.16%,
      rgba(148, 166, 197, 0.103266) 72.59%,
      rgba(148, 166, 197, 0.3) 98.99%
    )
    1;
  backdrop-filter: blur(0.1rem);

  .card__container {
    display: flex;
    height: 0.5856rem;

    .card__left {
      display: flex;
      align-items: center;
      margin-right: 0.23rem;

      .card__left-icon {
        width: 0.536rem;
        height: 100%;
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
      align-items: center;

      color: #fff;

      .card__right-contain {
        display: flex;
        flex-direction: column;
        justify-content: space-between;
      }

      .card__right-top {
        .card__right-title {
          font-weight: 700;
          font-size: 0.18rem;
          line-height: 0.18rem;
          margin-bottom: 0.1rem;
        }
      }

      .card__right-bottom {
        margin-top: 0.02rem;
        display: flex;
        align-items: flex-end;

        .card__right-value {
          font-weight: 700;
          font-size: 0.2rem;
          line-height: 0.16rem;
          color: #ffffff;

          .card__right-unit {
            font-size: 0.12rem;
            line-height: 0.16rem;
            color: rgba(255, 255, 255, 0.6);
          }
        }
      }
    }
  }
}
</style>
