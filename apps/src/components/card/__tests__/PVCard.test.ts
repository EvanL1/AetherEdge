import { describe, it, expect } from 'vitest'
import { mount } from '@vue/test-utils'
import { createPinia } from 'pinia'
import PVCard from '../PVCard.vue'

describe('PVCard', () => {
  it('should render correctly', () => {
    const wrapper = mount(PVCard, {
      props: {
        title: 'PV Generation',
        icon: '/test-icon.svg',
        value: '500',
      },
      global: {
        plugins: [createPinia()],
      },
    })
    expect(wrapper.exists()).toBe(true)
  })

  it('should display PV information', () => {
    const props = {
      title: 'PV Generation',
      icon: '/test-icon.svg',
      value: '500',
      unit: 'W',
    }
    const wrapper = mount(PVCard, {
      props,
      global: {
        plugins: [createPinia()],
      },
    })
    expect(wrapper.text()).toContain('PV Generation')
    expect(wrapper.text()).toContain('500')
    expect(wrapper.text()).toContain('W')
  })

  it('should display icon', () => {
    const props = {
      title: 'PV Generation',
      icon: '/test-icon.svg',
      value: '500',
    }
    const wrapper = mount(PVCard, {
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
      title: 'PV Generation',
      icon: '/test-icon.svg',
      value: '500',
    }
    const wrapper = mount(PVCard, {
      props,
      global: {
        plugins: [createPinia()],
      },
    })
    expect(wrapper.find('.card__right-unit').exists()).toBe(false)
  })
})
