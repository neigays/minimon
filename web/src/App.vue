<script setup lang="ts">
import { computed, watch } from 'vue'
import { RouterLink, RouterView, useRoute, useRouter } from 'vue-router'
import { useAuth } from './auth'
import { useLive } from './live'
import { guard, loginLocation } from './router'
import { theme, toggleTheme } from './theme'

const route = useRoute()
const router = useRouter()
const auth = useAuth()
const { user, loginRequired } = auth
const { connected } = useLive()

const inAdmin = computed(() => route.path.startsWith('/admin'))

// 会话失效或站点要求登录时，当前页面立即跳转登录页。
watch([user, loginRequired], () => {
  const to = guard(route.path, user.value !== null, loginRequired.value)
  if (to) void router.replace(to === '/login' ? loginLocation(route.fullPath) : to)
})

async function logout() {
  await auth.logout().catch(() => undefined)
  await router.replace('/login') // 身份变化的重连由 live.ts 监听 user 完成
}
</script>

<template>
  <header class="top">
    <div class="wrap bar">
      <RouterLink class="brand" to="/">
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><polyline points="3 12 7 12 10 5 14 19 17 12 21 12" /></svg>
        minimon
      </RouterLink>
      <nav class="main" aria-label="主导航">
        <RouterLink to="/" :aria-current="!inAdmin && route.path !== '/login' ? 'page' : undefined">节点</RouterLink>
        <RouterLink to="/admin" :aria-current="inAdmin ? 'page' : undefined">后台</RouterLink>
      </nav>
      <span class="spacer"></span>
      <span v-if="!connected" class="live down" role="status">
        <span class="dot" aria-hidden="true"></span>
        <span class="txt">连接中断，重连中…</span>
      </span>
      <button class="icon-btn" type="button" aria-label="切换浅色/深色" @click="toggleTheme">
        <svg v-if="theme === 'dark'" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><circle cx="12" cy="12" r="4" /><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" /></svg>
        <svg v-else width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><path d="M21 12.8A9 9 0 1111.2 3a7 7 0 009.8 9.8z" /></svg>
      </button>
      <button v-if="user" class="icon-btn" type="button" @click="logout">退出</button>
      <RouterLink v-else class="icon-btn" to="/login">登录</RouterLink>
    </div>
  </header>
  <main class="wrap">
    <RouterView />
  </main>
</template>

<style scoped>
.top { position: sticky; top: env(safe-area-inset-top, 0px); z-index: 5; background: var(--surface); border-bottom: 1px solid var(--line); }
.bar { display: flex; align-items: center; gap: 20px; height: 52px; }
.brand { display: flex; align-items: center; gap: 8px; font-weight: 650; letter-spacing: 0.02em; text-decoration: none; }
.brand svg { color: var(--ok); }
nav.main { display: flex; gap: 4px; }
nav.main a { text-decoration: none; padding: 6px 10px; border-radius: 6px; color: var(--muted); }
nav.main a[aria-current="page"] { color: var(--fg); background: var(--surface-2); }
nav.main a:hover { color: var(--fg); }
.spacer { flex: 1; }
.live { display: inline-flex; align-items: center; gap: 6px; color: var(--bad); font-size: 12px; white-space: nowrap; }
.live .dot { width: 8px; height: 8px; border-radius: 50%; background: var(--bad); }
@media (max-width: 720px) {
  .bar { gap: 10px; }
  .live .txt { display: none; }
  nav.main a { padding: 6px 8px; }
}
</style>
