import { describe, it, expect } from 'vitest'
import { mount } from '@vue/test-utils'
import { createPinia } from 'pinia'
import EnergyCard from '../EnergyCard.vue'

describe('EnergyCard', () => {
  it('should render correctly', () => {
    const wrapper = mount(EnergyCard, {
      props: {
        title: 'Energy Usage',
        icon: '/test-icon.svg',
        value: '1200',
      },
      global: {
        plugins: [createPinia()],
      },
    })
    expect(wrapper.exists()).toBe(true)
  })

  it('should display energy information', () => {
    const props = {
      title: 'Energy Usage',
      icon: '/test-icon.svg',
      value: '1200',
      unit: 'kWh',
    }
    const wrapper = mount(EnergyCard, {
      props,
      global: {
        plugins: [createPinia()],
      },
    })
    expect(wrapper.text()).toContain('Energy Usage')
    expect(wrapper.text()).toContain('1200')
    expect(wrapper.text()).toContain('kWh')
  })

  it('should display icon', () => {
    const props = {
      title: 'Energy Usage',
      icon: '/test-icon.svg',
      value: '1200',
    }
    const wrapper = mount(EnergyCard, {
      props,
      global: {
        plugins: [createPinia()],
      },
    })
    const iconImg = wrapper.find('.card__left-iconImg')
    expect(iconImg.exists()).toBe(true)
    expect(iconImg.attributes('src')).toBe('/test-icon.svg')
  })

  it('should not display unit when not provided', () => {
    const props = {
      title: 'Energy Usage',
      icon: '/test-icon.svg',
      value: '1200',
    }
    const wrapper = mount(EnergyCard, {
      props,
      global: {
        plugins: [createPinia()],
      },
    })
    expect(wrapper.find('.card__right-unit').exists()).toBe(false)
  })
})
