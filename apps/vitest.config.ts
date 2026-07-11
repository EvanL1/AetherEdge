import { fileURLToPath } from 'node:url'
import { mergeConfig, defineConfig, configDefaults } from 'vitest/config'
import viteConfig from './vite.config'

export default defineConfig(async (env) => {
  const resolvedViteConfig = typeof viteConfig === 'function' ? await viteConfig(env) : viteConfig

  return mergeConfig(
    resolvedViteConfig,
    defineConfig({
      test: {
        environment: 'jsdom',
        exclude: [...configDefaults.exclude, 'e2e/**'],
        root: fileURLToPath(new URL('./', import.meta.url)),
        coverage: {
          provider: 'v8',
          reporter: ['text', 'json', 'json-summary', 'html', 'lcov'],
          reportsDirectory: './coverage',
          // 只统计业务逻辑文件（utils / api / stores / composables / router）
          // 排除纯 UI 模板（.vue）、类型定义（types/）、不可单测的基础设施文件
          include: [
            'src/utils/*.ts',
            'src/api/**/*.ts',
            'src/stores/*.ts',
            'src/composables/*.ts',
            'src/router/*.ts',
          ],
          exclude: [
            'src/**/*.d.ts',
            'src/**/*.test.{js,ts}',
            'src/**/*.spec.{js,ts}',
            'src/main.ts',
            // HTTP 客户端封装 — 通过 API 测试隐式覆盖，单独测试意义不大
            'src/utils/request.ts',
            // 拖拽 / 布局 UI 工具 — 依赖真实 DOM 交互，不适合单测
            'src/utils/useDnd.ts',
            'src/utils/useLayout.ts',
            // 纯 Symbol 声明，无可测逻辑
            'src/utils/key.ts',
            // 路由注入器 — 依赖完整 Vue Router + UserStore 上下文，不适合单测
            'src/router/injector.ts',
            // Vue 指令 — DOM 操作实现，JSDOM 环境下无法有效覆盖
            'src/utils/directives.ts',
          ],
        },
        css: {
          modules: {
            classNameStrategy: 'non-scoped',
          },
        },
      },
    }),
  )
})
