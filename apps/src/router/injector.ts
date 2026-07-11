import { markRaw } from 'vue'
import type { RouteItem } from '@/types/menu'
import { router } from './index'
import { dynamicRoutes } from './dynamic-routes'
import { useUserStore } from '@/stores/user'

/**
 * 递归过滤路由，根据用户角色筛选可访问的路由
 */
function filterByRoles(routes: RouteItem[], roles: string[]): RouteItem[] {
  const res: RouteItem[] = []
  for (const r of routes) {
    const allow = (r.meta?.roles as string[] | undefined)
      ? roles.some((role) => (r.meta!.roles as string[]).includes(role))
      : true // 没写 roles 默认放行
    if (!allow) continue

    const route: RouteItem = { ...r }
    if (route.children?.length) {
      route.children = filterByRoles(route.children, roles)
    }

    // 避免被响应式代理（如果你会把 route 存到响应式容器里）
    res.push(markRaw(route))
  }
  return res
}

/**
 * 动态注入路由：通过 router.addRoute 动态添加
 */
export async function ensureRoutesInjected() {
  const user = useUserStore()
  if (user.routesInjected) return

  // 如果没有用户信息，使用默认的 Admin 角色
  const userRoles = user.roles.length > 0 ? user.roles : ['Admin']
  const allowed = filterByRoles(dynamicRoutes, userRoles)
  // 递归添加路由及其子路由
  function addRouteWithChildren(route: RouteItem, parentName?: string) {
    // 创建路由对象，移除 children 避免重复添加
    const { children, ...routeWithoutChildren } = route

    if (parentName) {
      // 作为子路由添加
      router.addRoute(parentName, routeWithoutChildren as any)
    } else {
      // 作为根路由的子路由添加
      router.addRoute('main', routeWithoutChildren as any)
    }

    // 递归添加子路由
    if (children && children.length > 0) {
      children.forEach((child) => {
        addRouteWithChildren(child, route.name)
      })
    }
  }

  // 添加所有过滤后的路由
  allowed.forEach((route) => {
    addRouteWithChildren(route)
  })

  // 设置根路由重定向到第一个动态路由
  if (allowed.length > 0) {
    const firstRoute = allowed[0]
    const rootRoute = router.getRoutes().find((route) => route.path === '/')
    if (rootRoute) {
      if (firstRoute.redirect) {
        // 确保重定向路径格式正确
        const redirectPath = firstRoute.redirect.startsWith('/')
          ? firstRoute.redirect
          : `/${firstRoute.redirect}`
        rootRoute.redirect = redirectPath
      } else {
        // 确保路径格式正确，避免双斜杠
        const path = firstRoute.path.startsWith('/') ? firstRoute.path : `/${firstRoute.path}`
        rootRoute.redirect = path
      }
    }
  }

  user.routesInjected = true
}

/**
 * 重置动态路由
 */
export function resetDynamicRoutes() {
  // 获取所有动态路由的名称
  const dynamicRouteNames = getAllRouteNames(dynamicRoutes)

  // 移除所有动态路由
  dynamicRouteNames.forEach((name) => {
    if (router.hasRoute(name)) {
      router.removeRoute(name)
    }
  })

  // 重置用户状态
  const user = useUserStore()
  user.routesInjected = false
}

/**
 * 递归获取所有路由名称
 */
function getAllRouteNames(routes: RouteItem[]): string[] {
  const names: string[] = []

  routes.forEach((route) => {
    if (route.name) {
      names.push(route.name)
    }
    if (route.children && route.children.length > 0) {
      names.push(...getAllRouteNames(route.children))
    }
  })

  return names
}

/**
 * 获取过滤后的路由用于侧边栏
 */
export function getFilteredRoutesForSidebar() {
  const user = useUserStore()
  // 如果没有用户信息，使用默认的 Admin 角色
  const userRoles = user.roles.length > 0 ? user.roles : ['Admin']
  return filterByRoles(dynamicRoutes, userRoles)
}
