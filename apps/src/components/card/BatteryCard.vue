<template>
  <div class="battery-card" :class="{ isOnly: propNumber }">
    <div class="battery-card__title" :class="{ isOnly: propNumber }">{{ prop.title }}</div>
    <div v-if="propNumber !== undefined" class="battery-card__value">
      {{ prop.value }}
      <span v-if="prop.unit !== undefined" class="battery-card__value-unit">{{ prop.unit }}</span>
    </div>
  </div>
</template>
<script setup lang="ts">
const prop = defineProps<{
  title: String
  value?: String | number
  unit?: String
}>()

const propNumber = computed(() => {
  if (prop.value === undefined && prop.unit === undefined) {
    return true
  }
  return false
})
</script>
<style scoped lang="scss">
.battery-card {
  width: 100%;
  height: 100%;
  padding: 0.3rem;
  background: rgba(84, 98, 140, 0.5);
  border: 0.01rem solid;
  display: flex;
  flex-direction: column;
  justify-content: space-between;
  border-image: linear-gradient(
      117.64deg,
      #94a6c5 2.73%,
      rgba(148, 166, 197, 0) 31.73%,
      rgba(148, 166, 197, 0.344221) 71.62%,
      #94a6c5 97.67%
    )
    1;
  &.isOnly {
    justify-content: center;
    align-items: center;
  }
  .battery-card__title {
    font-family: Arimo;
    font-style: regular;
    font-size: 0.2rem;
    color: rgba(255, 255, 255, 0.6);
    &.isOnly {
      font-family: Arimo;
      font-weight: 700;
      font-style: bold;
      font-size: 0.32rem;
      color: #fff;
    }
  }
  .battery-card__value {
    font-family: Arimo;
    font-weight: 700;
    font-style: bold;
    font-size: 0.4rem;
    line-height: 100%;
    letter-spacing: 0%;
    text-transform: capitalize;
    color: #fff;
    .battery-card__value-unit {
      font-family: Arimo;
      font-weight: 700;
      font-style: bold;
      font-size: 0.16rem;
      color: rgba(255, 255, 255, 0.5);
    }
  }
}
</style>
