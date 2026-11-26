import { createApp } from 'vue'
import App from './App.vue'
import { useAuth } from './auth'
import { createAppRouter } from './router'
import { initTheme } from './theme'
import './styles/tokens.css'
import './styles/base.css'
import './styles/tags.css'

async function boot() {
  initTheme()
  const auth = useAuth()
  // 先确定登录状态，深链接到 /admin 时守卫才能正确放行。
  await auth.refresh().catch(() => undefined)
  const router = createAppRouter(
    () => auth.user.value !== null,
    () => auth.loginRequired.value,
  )
  createApp(App).use(router).mount('#app')
}
void boot()
