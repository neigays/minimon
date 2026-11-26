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
    const st = await api<{ require_login: boolean; timezone: string }>('GET', '/api/admin/settings')
    requireLogin.value = st.require_login
    tz.value = st.timezone
    savedTz.value = st.timezone
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

const MORE_ZONES = ['Asia/Seoul', 'Asia/Taipei', 'Asia/Bangkok', 'Asia/Kolkata', 'Asia/Dubai', 'Asia/Kuala_Lumpur', 'Asia/Manila', 'Asia/Jakarta', 'Australia/Sydney', 'Europe/Paris', 'Europe/Berlin', 'Europe/Amsterdam', 'Europe/Moscow', 'America/Chicago', 'America/Denver', 'America/Toronto', 'America/Sao_Paulo', 'Pacific/Auckland', 'Etc/UTC']
const ZONES = ['Asia/Shanghai', 'Asia/Hong_Kong', 'Asia/Tokyo', 'Asia/Singapore', 'UTC', 'Europe/London', 'America/Los_Angeles', 'America/New_York']
const tz = ref('')
const savedTz = ref('')
const tzErr = ref('')
const tzOk = ref(false)
const tzBusy = ref(false)
async function saveTz() {
  const v = tz.value.trim()
  tzOk.value = false
  if (tzBusy.value || !loaded.value) return
  if (!v) return void (tzErr.value = '请输入时区名称，如 Asia/Shanghai')
  tzErr.value = ''
  tzBusy.value = true
  try {
    await api('PUT', '/api/admin/settings', { timezone: v })
    savedTz.value = v
    tz.value = v
    tzOk.value = true
  } catch (e) {
    tzErr.value = e instanceof ApiError ? e.message : '保存失败，请重试'
  } finally {
    tzBusy.value = false
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

    <div class="cols">
    <form class="card pw" novalidate @submit.prevent="saveTz">
      <div class="title">时区</div>
      <div class="muted small">用于月流量账期与到期日计算，不读取服务器所在主机的时区。修改后立即按新规则重算。</div>
      <div class="field">
        <label for="tz-sel">常用时区</label>
        <select id="tz-sel" :value="ZONES.includes(tz) ? tz : ''" @change="tz = ($event.target as HTMLSelectElement).value || tz">
          <option v-if="!ZONES.includes(tz)" value="">自定义</option>
          <option v-for="z in ZONES" :key="z" :value="z">{{ z }}</option>
        </select>
      </div>
      <div class="field"><label for="tz-in">或输入任意 IANA 时区名称</label><input id="tz-in" v-model="tz" list="tz-list" autocomplete="off" spellcheck="false" placeholder="如 Europe/Berlin" /><datalist id="tz-list"><option v-for="z in [...ZONES, ...MORE_ZONES]" :key="z" :value="z" /></datalist></div>
      <div v-if="tzErr" class="err" role="alert">{{ tzErr }}</div>
      <div v-if="tzOk" class="ok" role="status">时区已保存</div>
      <div><button class="btn" type="submit" :disabled="tzBusy || !loaded || tz.trim() === savedTz">{{ tzBusy ? '保存中…' : '保存时区' }}</button></div>
    </form>

    <form class="card pw" novalidate @submit.prevent="submitPw">
      <div class="title">修改密码</div>
      <div class="field"><label for="op">旧密码</label><input id="op" v-model="oldPw" type="password" autocomplete="current-password" /></div>
      <div class="field"><label for="np">新密码（至少 8 位）</label><input id="np" v-model="newPw" type="password" autocomplete="new-password" /></div>
      <div class="field"><label for="cp">确认新密码</label><input id="cp" v-model="confirmPw" type="password" autocomplete="new-password" /></div>
      <div v-if="pwErr" class="err" role="alert">{{ pwErr }}</div>
      <div v-if="pwOk" class="ok" role="status">密码已修改</div>
      <div><button class="btn" type="submit" :disabled="pwBusy">{{ pwBusy ? '提交中…' : '修改密码' }}</button></div>
    </form>
    </div>
  </section>
</template>

<style scoped>
.panel-head { margin-bottom: 12px; }
.panel-head h2 { font-size: 16px; margin: 0; }
.card { background: var(--surface); border: 1px solid var(--line); border-radius: var(--r); padding: 16px; margin-bottom: 16px; display: grid; gap: 12px; }
.cols { display: grid; grid-template-columns: repeat(auto-fit, minmax(320px, 1fr)); gap: 16px; align-items: start; }
.cols .card { margin-bottom: 0; }
.row { display: flex; gap: 16px; align-items: center; }
.grow { flex: 1; min-width: 0; }
.title { font-weight: 600; }
.small { font-size: 12px; }
.card select { background: var(--bg); border: 1px solid var(--line); border-radius: 8px; padding: 9px 11px; }
.card select:focus { border-color: var(--c1); outline: none; }
.ok { color: var(--ok); font-size: 13px; }
.switch { flex: none; width: 32px; height: 18px; border-radius: 999px; background: var(--line); border: 0; position: relative; cursor: pointer; }
.switch::after { content: ""; position: absolute; top: 2px; left: 2px; width: 14px; height: 14px; border-radius: 50%; background: var(--fg); transition: left 0.15s; }
.switch[aria-checked="true"] { background: var(--ok); }
.switch[aria-checked="true"]::after { left: 16px; background: #fff; }
.switch:disabled { opacity: 0.6; cursor: default; }
</style>
