import { ref } from 'vue'
import { api, ApiError, onUnauthorized } from './api'

const user = ref<string | null>(null)
/** 访客在公开接口上收到 401，说明站点开启了 require_login。 */
const loginRequired = ref(false)

// 登录/改密码接口自身的 401（密码错误）不代表会话失效，也不代表整站需要登录。
onUnauthorized((path) => {
  if (path.startsWith('/api/auth/')) return
  user.value = null
  if (path === '/api/nodes' || path.startsWith('/api/nodes/') || path === '/api/live') loginRequired.value = true
})

export function useAuth() {
  async function refresh() {
    try {
      user.value = (await api<{ username: string }>('GET', '/api/auth/me')).username
    } catch (e) {
      if (!(e instanceof ApiError)) throw e
      user.value = null
    }
  }
  async function login(username: string, password: string) {
    await api('POST', '/api/auth/login', { username, password })
    await refresh()
  }
  async function logout() {
    try {
      await api('POST', '/api/auth/logout', {})
    } finally {
      user.value = null
    }
  }
  async function changePassword(oldPassword: string, newPassword: string) {
    await api('PUT', '/api/auth/password', { old_password: oldPassword, new_password: newPassword })
  }
  return { user, loginRequired, refresh, login, logout, changePassword }
}
