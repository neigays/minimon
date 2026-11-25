import { createRouter, createWebHistory, type Router } from 'vue-router'
import WallView from './views/WallView.vue'
import LoginView from './views/LoginView.vue'

/** 路由守卫的纯函数：返回重定向路径，不需要重定向时返回 null。 */
export function guard(path: string, authed: boolean, loginRequired: boolean): string | null {
  if (path === '/login') return authed ? '/' : null
  if (authed) return null
  if (loginRequired || path === '/admin' || path.startsWith('/admin/')) return '/login'
  return null
}

export function createAppRouter(isAuthed: () => boolean, isLoginRequired: () => boolean): Router {
  const router = createRouter({
    history: createWebHistory(),
    routes: [
      { path: '/', component: WallView },
      { path: '/node/:id', component: () => import('./views/NodeView.vue') },
      { path: '/login', component: LoginView },
      { path: '/admin', component: () => import('./views/AdminView.vue') },
      { path: '/admin/settings', component: () => import('./views/AdminSettingsView.vue') },
      { path: '/:rest(.*)*', redirect: '/' },
    ],
  })
  router.beforeEach((to) => guard(to.path, isAuthed(), isLoginRequired()) ?? true)
  return router
}
