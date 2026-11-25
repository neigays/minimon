<script setup lang="ts">
import { ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { ApiError } from '../api'
import { useAuth } from '../auth'
import { safeRedirect } from '../router'

const auth = useAuth()
const router = useRouter()
const route = useRoute()
const username = ref('admin')
const password = ref('')
const error = ref('')
const busy = ref(false)

async function submit() {
  if (busy.value) return
  if (!username.value.trim()) return void (error.value = '请输入用户名')
  if (!password.value) return void (error.value = '请输入密码')
  error.value = ''
  busy.value = true
  try {
    await auth.login(username.value.trim(), password.value)
    await router.replace(safeRedirect(route.query.redirect))
  } catch (e) {
    error.value = e instanceof ApiError ? e.message : '登录失败，请重试'
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <section aria-label="登录">
    <form class="login" novalidate @submit.prevent="submit">
      <h1>管理员登录</h1>
      <p>登录后可查看 IP、主机名、流量额度等管理字段，并进入后台。</p>
      <div class="field"><label for="u">用户名</label><input id="u" v-model="username" autocomplete="username" /></div>
      <div class="field"><label for="p">密码</label><input id="p" v-model="password" type="password" autocomplete="current-password" placeholder="首次启动时打印在 server 日志中" /></div>
      <div v-if="error" class="err" role="alert">
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><circle cx="12" cy="12" r="10" /><path d="M12 8v4M12 16h.01" /></svg>
        <span>{{ error }}</span>
      </div>
      <button class="btn" type="submit" :disabled="busy">{{ busy ? '登录中…' : '登录' }}</button>
    </form>
  </section>
</template>

<style scoped>
.login { max-width: 360px; margin: 40px auto; background: var(--surface); border: 1px solid var(--line); border-radius: var(--r); padding: 28px 24px; display: grid; gap: 14px; }
h1 { margin: 0; font-size: 20px; }
p { margin: 0; color: var(--muted); font-size: 13px; }
</style>
