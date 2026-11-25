<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { api, ApiError } from '../../api'
import { useAuth } from '../../auth'

const auth = useAuth()
const requireLogin = ref(false)
const loaded = ref(false)
const settingErr = ref('')
const savingSetting = ref(false)

onMounted(async () => {
  try {
    requireLogin.value = (await api<{ require_login: boolean }>('GET', '/api/admin/settings')).require_login
    loaded.value = true
  } catch (e) {
    settingErr.value = e instanceof ApiError ? e.message : '加载失败'
  }
})

async function toggle() {
  if (savingSetting.value || !loaded.value) return
  const next = !requireLogin.value
  savingSetting.value = true
  settingErr.value = ''
  try {
    await api('PUT', '/api/admin/settings', { require_login: next })
    requireLogin.value = next
  } catch (e) {
    settingErr.value = e instanceof ApiError ? e.message : '保存失败'
  } finally {
    savingSetting.value = false
  }
}

const oldPw = ref('')
const newPw = ref('')
const confirmPw = ref('')
const pwErr = ref('')
const pwOk = ref(false)
const pwBusy = ref(false)

async function submitPw() {
  if (pwBusy.value) return
  pwOk.value = false
  if (!oldPw.value) return void (pwErr.value = '请输入旧密码')
  if (newPw.value.length < 8) return void (pwErr.value = '新密码至少 8 位')
  if (newPw.value !== confirmPw.value) return void (pwErr.value = '两次输入的新密码不一致')
  pwErr.value = ''
  pwBusy.value = true
  try {
    await auth.changePassword(oldPw.value, newPw.value)
    oldPw.value = newPw.value = confirmPw.value = ''
    pwOk.value = true
  } catch (e) {
    pwErr.value = e instanceof ApiError ? e.message : '修改失败，请重试'
  } finally {
    pwBusy.value = false
  }
}
</script>

<template>
  <section aria-label="系统设置">
    <div class="panel-head"><h2>系统设置</h2></div>

    <div class="card">
      <div class="row">
        <div class="grow">
          <div class="title" id="rl-label">整站需要登录</div>
          <div class="muted small">开启后，未登录的访客无法查看卡片墙和节点详情，已打开的页面会被立即踢到登录页。</div>
        </div>
        <button type="button" class="switch" role="switch" :aria-checked="requireLogin" aria-labelledby="rl-label" :disabled="!loaded || savingSetting" @click="toggle"></button>
      </div>
      <div v-if="settingErr" class="err" role="alert">{{ settingErr }}</div>
    </div>

    <form class="card pw" novalidate @submit.prevent="submitPw">
      <div class="title">修改密码</div>
      <div class="field"><label for="op">旧密码</label><input id="op" v-model="oldPw" type="password" autocomplete="current-password" /></div>
      <div class="field"><label for="np">新密码（至少 8 位）</label><input id="np" v-model="newPw" type="password" autocomplete="new-password" /></div>
      <div class="field"><label for="cp">确认新密码</label><input id="cp" v-model="confirmPw" type="password" autocomplete="new-password" /></div>
      <div v-if="pwErr" class="err" role="alert">{{ pwErr }}</div>
      <div v-if="pwOk" class="ok" role="status">密码已修改</div>
      <div><button class="btn" type="submit" :disabled="pwBusy">{{ pwBusy ? '提交中…' : '修改密码' }}</button></div>
    </form>
  </section>
</template>

<style scoped>
.panel-head { margin-bottom: 12px; }
.panel-head h2 { font-size: 16px; margin: 0; }
.card { background: var(--surface); border: 1px solid var(--line); border-radius: var(--r); padding: 16px; margin-bottom: 16px; display: grid; gap: 12px; }
.pw { max-width: 420px; }
.row { display: flex; gap: 16px; align-items: center; }
.grow { flex: 1; min-width: 0; }
.title { font-weight: 600; }
.small { font-size: 12px; }
.ok { color: var(--ok); font-size: 13px; }
.switch { flex: none; width: 32px; height: 18px; border-radius: 999px; background: var(--line); border: 0; position: relative; cursor: pointer; }
.switch::after { content: ""; position: absolute; top: 2px; left: 2px; width: 14px; height: 14px; border-radius: 50%; background: var(--fg); transition: left 0.15s; }
.switch[aria-checked="true"] { background: var(--ok); }
.switch[aria-checked="true"]::after { left: 16px; background: #fff; }
.switch:disabled { opacity: 0.6; cursor: default; }
</style>
