import { createRouter, createWebHistory } from 'vue-router'
import { staticRoutes } from './static-routes'

export const router = createRouter({
  history: createWebHistory(import.meta.env.BASE_URL),
  routes: staticRoutes,
})

export default router
