/// <reference types="vite/client" />
// 支持导入 .vue 文件
declare module '*.vue' {
  import type { DefineComponent } from 'vue'
  const component: DefineComponent<{}, {}, any>
  export default component
}

// 声明 postcss-pxtorem 模块
declare module 'postcss-pxtorem' {
  interface PxtoremOptions {
    rootValue?: number | ((file: string) => number)
    unitPrecision?: number
    propList?: string[]
    selectorBlackList?: (string | RegExp)[]
    replace?: boolean
    mediaQuery?: boolean
    minPixelValue?: number
    exclude?: RegExp | ((file: string) => boolean)
  }

  function pxtorem(options?: PxtoremOptions): any
  export = pxtorem
}
declare module 'v-fit-columns' {
  import type { Plugin } from 'vue'
  const vFitColumns: Plugin
  export default vFitColumns
}
