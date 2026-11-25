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

/** 登录后的跳转目标：只接受同源路径（以单个 / 开头），其余一律回首页。 */
export function safeRedirect(q: unknown): string {
  const v = Array.isArray(q) ? q[0] : q
  if (typeof v !== 'string' || !v.startsWith('/') || v.startsWith('//') || v.startsWith('/\\')) return '/'
  if (v === '/login' || v.startsWith('/login?')) return '/'
  return v
}

export function loginLocation(fullPath: string): { path: string; query?: { redirect: string } } {
  const target = safeRedirect(fullPath)
  return target === '/' ? { path: '/login' } : { path: '/login', query: { redirect: target } }
}

export function createAppRouter(isAuthed: () => boolean, isLoginRequired: () => boolean): Router {
  const router = createRouter({
    history: createWebHistory(),
    routes: [
      { path: '/', component: WallView },
      { path: '/node/:id', component: () => import('./views/NodeView.vue') },
      { path: '/login', component: LoginView },
      {
        path: '/admin',
        component: () => import('./views/admin/AdminLayout.vue'),
        children: [
          { path: '', component: () => import('./views/admin/NodesView.vue') },
          { path: 'probes', component: () => import('./views/admin/ProbesView.vue') },
          { path: 'settings', component: () => import('./views/admin/SettingsView.vue') },
        ],
      },
      { path: '/:rest(.*)*', redirect: '/' },
    ],
  })
  router.beforeEach((to) => {
    const dest = guard(to.path, isAuthed(), isLoginRequired())
    if (dest === '/login') return loginLocation(to.fullPath)
    return dest ?? true
  })
  return router
}
