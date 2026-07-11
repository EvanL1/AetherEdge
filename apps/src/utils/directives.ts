import { type DirectiveBinding, unref } from 'vue'
import { useUserStore } from '@/stores/user'
/**
 * 自定义指令 v-permission
 * 用法：<button v-permission="['Admin', 'editor']">仅管理员和编辑可见</button>
 * 只有当前用户角色在传入的角色数组中时，元素才会显示，否则会被移除
 */
const permissionDirective = {
  mounted(el: HTMLElement, binding: DirectiveBinding<string[]>) {
    const allowedRoles = binding.value
    const userStore = useUserStore()
    const userRole = userStore.roles
    if (!userRole || !allowedRoles.includes(userRole[0])) {
      // 如果没有权限，移除元素
      el.parentNode && el.parentNode.removeChild(el)
    }
  },
}

/**
 * v-fit-columns 指令
 * 目标：根据每列单元格内容的 scrollWidth 自动为 <col> 设置 width，使表格横向出现滚动而不换行。
 * 适用：Element Plus 表格（需存在 <colgroup><col name="xxx">），单元格需要有 .cell 容器（ElTable 默认）。
 * 使用：
 *   <el-table v-fit-columns> ... </el-table>
 *   <el-table v-fit-columns="24"> ... </el-table>    // 自定义 padding（额外补白）
 *   <el-table v-fit-columns="{ padding: 24 }"> ... </el-table>
 * 约定：
 *   若某列首个单元格（或表头 th）包含类名 .leave-alone，则跳过该列的自适应（常用于操作列）。
 */
type ColumnDef = {
  prop?: string
  label?: string
  width?: number | string // 支持数字(px)或字符串(px/rem)
  fixed?: 'left' | 'right' | boolean
  type?: string
}

type FitColumnsBinding =
  | number
  | { padding?: number; observe?: 'data' | 'layout'; columns?: ColumnDef[] }

function getPadding(binding: DirectiveBinding<FitColumnsBinding>): number {
  // 内部一律以 px 计算，默认 padding 按 0.18rem => 18px
  const DEFAULT_REM = 0.18
  const DEFAULT_PX = Math.round(DEFAULT_REM * 100)
  const val = binding?.value as any
  if (typeof val === 'number') return val // 数字按 px
  if (val && typeof val.padding === 'number') return val.padding // 数字按 px
  return DEFAULT_PX
}

function getObserveMode(binding: DirectiveBinding<FitColumnsBinding>): 'data' | 'layout' {
  const val = binding?.value as any
  return val && (val.observe === 'layout' || val.observe === 'data') ? val.observe : 'data'
}

function remToPx(value: string): number | null {
  if (!value) return null
  const trimmed = value.trim()
  if (trimmed.endsWith('rem')) {
    const n = parseFloat(trimmed.replace('rem', ''))
    if (Number.isFinite(n)) {
      const rootFont = parseFloat(getComputedStyle(document.documentElement).fontSize || '16')
      return Math.round(n * rootFont)
    }
  }
  if (trimmed.endsWith('px')) {
    const n = parseFloat(trimmed.replace('px', ''))
    return Number.isFinite(n) ? Math.round(n) : null
  }
  const n = parseFloat(trimmed)
  return Number.isFinite(n) ? Math.round(n) : null
}

function pxToRemString(px: number): string {
  const rem = px / 100
  // 保留最多 3 位小数，去掉末尾 0
  const s = rem
    .toFixed(3)
    .replace(/\.0+$/, '')
    .replace(/(\.\d*?)0+$/, '$1')
  return `${s}rem`
}

function anyToRemString(value: number | string): string | null {
  if (value == null) return null
  if (typeof value === 'number' && Number.isFinite(value)) {
    return pxToRemString(value)
  }
  const str = String(value).trim()
  if (!str) return null
  if (str.endsWith('rem')) {
    const n = parseFloat(str)
    if (!Number.isFinite(n)) return null
    const s = n
      .toFixed(3)
      .replace(/\.0+$/, '')
      .replace(/(\.\d*?)0+$/, '$1')
    return `${s}rem`
  }
  if (str.endsWith('px')) {
    const px = parseFloat(str)
    return Number.isFinite(px) ? pxToRemString(px) : null
  }
  const n = parseFloat(str)
  return Number.isFinite(n) ? pxToRemString(n) : null
}

function getHeaderColNames(table: HTMLElement): string[] {
  // 只取非固定表头的 th，避免重复
  const header = table.querySelector('.el-table__header-wrapper thead') as HTMLElement | null
  const ths = header ? Array.from(header.querySelectorAll('th')) : []
  const names: string[] = []
  ths.forEach((th) => {
    const cls = Array.from(th.classList).find((c) => /el-table_\d+_column_\d+/.test(c))
    if (cls) names.push(cls)
  })
  return names
}

function markLeaveAlone(table: HTMLElement, colName: string) {
  table
    .querySelectorAll(`th.${colName}, td.${colName}`)
    .forEach((el) => el.classList.add('leave-alone'))
}

function applyColumnsConfig(table: HTMLElement, columns?: ColumnDef[] | any) {
  const resolved = unref(columns) as ColumnDef[] | undefined
  if (!Array.isArray(resolved) || resolved.length === 0) return
  const colNames = getHeaderColNames(table)
  if (!colNames.length) return

  resolved.forEach((colDef, idx) => {
    const name = colNames[idx]
    if (!colDef || !name) return
    if (colDef.width != null) {
      const remStr = anyToRemString(colDef.width)
      if (!remStr) return
      // 指定宽度：使用 CSS width（rem），标记跳过自适应
      table.querySelectorAll(`col[name=${name}]`).forEach((col) => {
        col.removeAttribute('width')
        ;(col as HTMLElement).style.width = remStr
      })
      markLeaveAlone(table, name)
      return
    }
    // 未指定宽度但为固定列：锁定当前宽度并跳过自适应
    if (colDef.fixed === 'left' || colDef.fixed === 'right' || colDef.fixed === true) {
      // 读取当前 th 的可见宽度
      const th = table.querySelector(`th.${name}`) as HTMLElement | null
      let px = th?.offsetWidth || 0
      if (!px) {
        const col = table.querySelector(`col[name=${name}]`) as HTMLElement | null
        if (col) {
          const w = parseFloat(getComputedStyle(col).width || '0')
          if (Number.isFinite(w)) px = Math.round(w)
        }
      }
      if (px > 0) {
        const remStr = pxToRemString(px)
        table.querySelectorAll(`col[name=${name}]`).forEach((col) => {
          col.removeAttribute('width')
          ;(col as HTMLElement).style.width = remStr
        })
      }
      markLeaveAlone(table, name)
    }
  })
}

function adjustColumnWidth(table: HTMLElement, padding = 18) {
  // 为表格添加标识类，配合样式启用 nowrap 与横向滚动
  table.classList.add('r-table')

  // 先收集每个列 name 对应的最大内容宽度
  const widthMap = new Map<string, number>()

  // Element Plus 在主表、固定列会生成多个 table/colgroup，这里统一从根节点收集所有单元格参与计算
  const allCells = [
    ...Array.from(table.querySelectorAll('td')),
    ...Array.from(table.querySelectorAll('th')),
  ] as HTMLElement[]

  allCells.forEach((cellEl) => {
    // 每个 th/td 上都会带有类似 el-table_x_column_y 的类名，且与 col[name] 对应
    const cls = Array.from(cellEl.classList).find((c) => /el-table_\d+_column_\d+/.test(c))
    if (!cls) return
    // 跳过声明为 leave-alone 的列
    if (cellEl.classList.contains('leave-alone')) return
    const inner = cellEl.querySelector('.cell') as HTMLElement | null

    const w = inner?.scrollWidth || 0
    const prev = widthMap.get(cls) || 0
    if (w > prev) widthMap.set(cls, w)
  })

  // 将计算结果回填到所有 col[name]（覆盖主表与固定列的 colgroup）
  widthMap.forEach((w, name) => {
    const px = w + padding
    const remStr = pxToRemString(px)
    table.querySelectorAll(`col[name=${name}]`).forEach((col) => {
      col.removeAttribute('width')
      ;(col as HTMLElement).style.width = remStr
    })
  })
}

// 监听尺寸/内容变化，自动重算
function setupObservers(el: HTMLElement, padding: number, mode: 'data' | 'layout') {
  let rafId = 0
  const schedule = () => {
    if (rafId) cancelAnimationFrame(rafId)
    rafId = requestAnimationFrame(() => adjustColumnWidth(el, padding))
  }

  const cleanups: Array<() => void> = []

  if (mode === 'layout') {
    const ro = new ResizeObserver(() => schedule())
    ro.observe(el)
    const bodyWrapper = el.querySelector('.el-table__body-wrapper') as HTMLElement | null
    if (bodyWrapper) ro.observe(bodyWrapper)
    cleanups.push(() => ro.disconnect())
  } else {
    // data 模式：仅当行数变化时重算，避免频繁抖动
    const bodyTbody = (el.querySelector('.el-table__body-wrapper tbody') ||
      el.querySelector('tbody')) as HTMLElement | null
    if (bodyTbody) {
      let lastRows = bodyTbody.querySelectorAll('tr').length
      const mo = new MutationObserver(() => {
        const currentRows = bodyTbody.querySelectorAll('tr').length
        if (currentRows !== lastRows) {
          lastRows = currentRows
          schedule()
        }
      })
      mo.observe(bodyTbody, { childList: true })
      cleanups.push(() => mo.disconnect())
    }
  }

  ;(el as any).__fitColumnsCleanup = () => {
    cleanups.forEach((fn) => {
      try {
        fn()
      } catch {}
    })
  }
}

const fitColumnsDirective = {
  mounted(el: HTMLElement, binding: DirectiveBinding<FitColumnsBinding>) {
    const padding = getPadding(binding)
    const mode = getObserveMode(binding)
    const columns = (binding?.value as any)?.columns as ColumnDef[] | undefined
    // 初次延迟，等待表格渲染完成
    setTimeout(() => {
      applyColumnsConfig(el, columns)
      adjustColumnWidth(el, padding)
    }, 300)
    setupObservers(el, padding, mode)
  },
  updated(el: HTMLElement, binding: DirectiveBinding<FitColumnsBinding>) {
    const padding = getPadding(binding)
    const columns = (binding?.value as any)?.columns as ColumnDef[] | undefined
    // 仅在绑定值（如 padding/observe）变化时强制一次
    setTimeout(() => {
      applyColumnsConfig(el, columns)
      adjustColumnWidth(el, padding)
    }, 100)
  },
  unmounted(el: HTMLElement) {
    const cleanup = (el as any).__fitColumnsCleanup
    if (typeof cleanup === 'function') cleanup()
  },
}

export const createThrottle = <T extends (...args: any[]) => void>(fn: T, delay = 300) => {
  let last = 0
  return (...args: Parameters<T>) => {
    const now = Date.now()
    if (now - last > delay) {
      last = now
      fn(...args)
    }
  }
}

const throttleDirective = {
  mounted(el: HTMLElement, binding: DirectiveBinding<any>) {
    const { value: fn, modifiers, arg } = binding

    // 默认事件类型：click
    const event = arg || 'click'

    // 默认延迟
    let delay = 300

    // 如果包含数字修饰符（如 .500）
    for (const key in modifiers) {
      if (!isNaN(parseFloat(key))) {
        delay = parseFloat(key)
      }
    }

    const handler = createThrottle(fn, delay)
    ;(el as any).__actionHandler__ = handler
    el.addEventListener(event, handler)
  },

  beforeUnmount(el: HTMLElement, binding: DirectiveBinding<any>) {
    el.removeEventListener(binding.arg || 'click', (el as any).__actionHandler__)
  },
}

const debounceDirective = {
  mounted(el: HTMLElement, binding: DirectiveBinding<any>) {
    const { value: fn, modifiers, arg } = binding

    // 默认事件类型：click
    const event = arg || 'click'

    // 默认延迟
    let delay = 300
    // 如果包含数字修饰符（如 .500）
    for (const key in modifiers) {
      if (!isNaN(parseFloat(key))) {
        delay = parseFloat(key)
      }
    }

    let handler = fn
    if (modifiers.debounce) {
      let timer: number | null = null
      handler = (...args: any[]) => {
        clearTimeout(timer as number)
        timer = setTimeout(() => fn(...args), delay)
      }
    }
  },
}
export { throttleDirective, debounceDirective, fitColumnsDirective, permissionDirective }
