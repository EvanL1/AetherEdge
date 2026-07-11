<template>
  <div class="card">
    <LoadingBg :loading="props.loading">
      <div class="card__header">
        <div class="card__header-icon"></div>
        <div class="card__header-title">{{ props.title }}</div>
      </div>
      <div class="card__content">
        <slot></slot>
      </div>
      <div class="card__content-footer">
        <slot name="footer"></slot>
      </div>
    </LoadingBg>
  </div>
</template>

<script lang="ts" setup>
const props = defineProps({
  title: {
    type: String,
    required: true,
  },
  loading: {
    type: Boolean,
    default: false,
  },
})
</script>

<style lang="scss" scoped>
.card {
  width: 100%;
  height: 100%;
  min-height: 0;
  background: rgba(84, 98, 140, 0.2);
  padding: 0.2rem;
  border: 0.01rem solid;
  border-image: linear-gradient(
      117.31deg,
      rgba(148, 166, 197, 0.3) 2.77%,
      rgba(148, 166, 197, 0) 32.18%,
      rgba(148, 166, 197, 0.103266) 72.63%,
      rgba(148, 166, 197, 0.3) 99.05%
    )
    1;
  backdrop-filter: blur(0.1rem);
  display: flex;
  flex-direction: column;

  .card__header {
    display: flex;
    align-items: center;

    .card__header-icon {
      width: 0.2rem;
      height: 0.2rem;
      background-image: url('@/assets/images/card-icon.png');
      background-size: cover;
      background-position: center;
      background-repeat: no-repeat;
    }

    .card__header-title {
      margin-left: 0.03rem;
      font-family: Arimo;
      font-weight: 700;
      font-style: Bold;
      font-size: 0.18rem;
      line-height: 1em;
      color: #ffffff;
    }
  }

  .card__content {
    flex: 1 1 auto;
    min-height: 0;
    width: 100%;
    display: flex;
    flex-direction: column;
    justify-content: space-between;

    // &.no-footer {
    //   height: calc(100% - 0.2rem);
    // }
  }

  .card__content-footer {
    flex: none;
    width: 100%;
    display: flex;
    flex-direction: row-reverse;
    // border-top: 0.01rem solid rgba(255, 255, 255, 0.1);

    .card__content-footer-button {
      margin: 0.3rem 0 0.1rem 0;

      .card__content-footer-button-icon {
        width: 0.16rem;
        height: 0.16rem;
        margin-right: 0.08rem;
        object-position: center;
        object-fit: contain;
      }
    }
  }
}
</style>
