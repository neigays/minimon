<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { api, ApiError } from '../../api'
import { formatEventTime, ruleSummary, type ActiveAlert, type AlertRule, type AlertSettings } from '../../alert'
import AlertHistory from '../../components/AlertHistory.vue'
import AlertRuleDialog from '../../components/AlertRuleDialog.vue'
import ConfirmButton from '../../components/ConfirmButton.vue'
import { useLive } from '../../live'

const { nodes } = useLive()
const rules = ref<AlertRule[]>([])
const tasks = ref<{ id: number; name: string }[]>([])
const active = ref<ActiveAlert[]>([])
const settings = ref<AlertSettings | null>(null)
const tz = ref<string | null>(null)
const loaded = ref(false)
const pageErr = ref('')
const busy = ref(false)
const dlg = ref<InstanceType<typeof AlertRuleDialog> | null>(null)
const history = ref<InstanceType<typeof AlertHistory> | null>(null)

const sortedNodes = computed(() => [...nodes.value].sort((a, b) => a.sort - b.sort))
const taskName = (id: number) => tasks.value.find((t) => t.id === id)?.name
const scope = (r: AlertRule) => (r.all_nodes ? '全部节点' : `${r.node_ids.length} 个节点`)
const since = (a: ActiveAlert) => formatEventTime(a.since_ms, tz.value)

async function loadRules() {
  rules.value = await api<AlertRule[]>('GET', '/api/admin/alert-rules')
}
async function loadActive() {
  active.value = await api<ActiveAlert[]>('GET', '/api/admin/alerts/active')
}
async function loadAll() {
  const [, t, s, st] = await Promise.all([
    loadRules(),
    api<{ id: number; name: string }[]>('GET', '/api/admin/probes'),
    api<AlertSettings>('GET', '/api/admin/alert-settings'),
    api<{ timezone: string }>('GET', '/api/admin/settings').catch(() => null),
    loadActive(),
  ])
  tasks.value = t
  settings.value = s
  tz.value = st?.timezone ?? null
  loaded.value = true
}
let timer: ReturnType<typeof setInterval> | undefined
onMounted(async () => {
  try {
    await loadAll()
  } catch (e) {
    pageErr.value = e instanceof ApiError ? e.message : '加载失败'
  }
  timer = setInterval(() => void loadActive().catch(() => undefined), 15000)
})
onBeforeUnmount(() => clearInterval(timer))

async function run(fn: () => Promise<unknown>) {
  if (busy.value) return
  busy.value = true
  pageErr.value = ''
  try {
    await fn()
  } catch (e) {
    pageErr.value = e instanceof ApiError ? e.message : '操作失败，请重试'
  }
  try {
    await Promise.all([loadRules(), loadActive()])
  } catch {
    // 已有错误提示
  }
  busy.value = false
}
const toggleRule = (r: AlertRule) => run(() => api('PATCH', `/api/admin/alert-rules/${r.id}`, { enabled: !r.enabled }))
const removeRule = (r: AlertRule) => run(() => api('DELETE', `/api/admin/alert-rules/${r.id}`))
async function onSaved() {
  try {
    await Promise.all([loadRules(), loadActive()])
  } catch (e) {
    pageErr.value = e instanceof ApiError ? e.message : '刷新失败'
  }
}

// ---------- 内置规则 ----------
async function putSettings(body: Record<string, unknown>) {
  settings.value = await api<AlertSettings>('PUT', '/api/admin/alert-settings', body)
}
type Flag = 'offline_enabled' | 'traffic_enabled' | 'expire_enabled'
const BUILTIN: { key: Flag; label: string; hint: string }[] = [
  { key: 'offline_enabled', label: '节点离线', hint: '节点断线超过宽限期后告警' },
  { key: 'traffic_enabled', label: '流量用量', hint: '本月流量达到额度的 80%、100% 时提醒' },
  { key: 'expire_enabled', label: '到期提醒', hint: '到期前 7、3、1 天提醒' },
]
const toggleFlag = (k: Flag) => {
  const s = settings.value
  if (s) void run(() => putSettings({ [k]: !s[k] }))
}
const grace = ref('')
const graceErr = ref('')
const graceShown = computed(() => (grace.value !== '' ? grace.value : String(settings.value?.offline_grace_secs ?? '')))
async function saveGrace() {
  const v = Number(graceShown.value)
  if (!Number.isInteger(v) || v < 30 || v > 3600) return void (graceErr.value = '宽限期需为 30–3600 的整数（秒）')
  graceErr.value = ''
  if (v === settings.value?.offline_grace_secs) return void (grace.value = '')
  await run(() => putSettings({ offline_grace_secs: v }))
  grace.value = ''
}

// ---------- 通知 ----------
const key = ref('')
const keyMsg = ref('')
const keyErr = ref('')
const testMsg = ref('')
const testErr = ref('')
const testing = ref(false)
async function saveKey(clear: boolean) {
  const v = clear ? '' : key.value.trim()
  if (!clear && !v) return void (keyErr.value = '请输入 SendKey；留空表示不修改')
  keyErr.value = ''
  keyMsg.value = ''
  if (busy.value) return
  busy.value = true
  try {
    await putSettings({ sendkey: v })
    key.value = ''
    keyMsg.value = clear ? '已清除 SendKey' : '已保存'
  } catch (e) {
    keyErr.value = e instanceof ApiError ? e.message : '保存失败，请重试'
  } finally {
    busy.value = false
  }
}
const tags = ref<string | null>(null)
const tagsShown = computed(() => tags.value ?? settings.value?.push_tags ?? '')
const tagsMsg = ref('')
const tagsErr = ref('')
async function saveTags() {
  tagsErr.value = ''
  tagsMsg.value = ''
  if (busy.value) return
  busy.value = true
  try {
    await putSettings({ push_tags: tagsShown.value })
    tags.value = null
    tagsMsg.value = '已保存'
  } catch (e) {
    tagsErr.value = e instanceof ApiError ? e.message : '保存失败，请重试'
  } finally {
    busy.value = false
  }
}
async function sendTest() {
  if (testing.value) return
  testing.value = true
  testMsg.value = ''
  testErr.value = ''
  try {
    await api('POST', '/api/admin/alert-settings/test')
    testMsg.value = '测试消息已发送，请在 Server酱³ 中查收'
  } catch (e) {
    testErr.value = e instanceof ApiError ? e.message : '发送失败，请重试'
  } finally {
    testing.value = false
    void history.value?.reload()
  }
}
</script>

<template>
  <section aria-label="告警">
    <div class="panel-head"><h2>告警</h2></div>
    <div v-if="pageErr" class="err page-err" role="alert">{{ pageErr }}</div>

    <h3>当前告警</h3>
    <div class="box">
      <p v-if="loaded && active.length === 0" class="ok-line">一切正常</p>
      <ul v-else class="active">
        <li v-for="(a, i) in active" :key="i">
          <span class="dot" aria-hidden="true"></span>
          <strong>{{ a.rule_name ?? '离线' }}</strong>
          <span>{{ a.node_name ?? `节点 #${a.node_id}` }}<template v-if="a.task_name">（{{ a.task_name }}）</template></span>
          <span v-if="a.value_text" class="num">{{ a.value_text }}</span>
          <span class="muted small">已触发 · 始于 {{ since(a) }}</span>
        </li>
      </ul>
    </div>

    <h3>规则</h3>
    <div class="box builtin">
      <div v-for="b in BUILTIN" :key="b.key" class="b-row">
        <button type="button" class="switch" role="switch" :aria-checked="settings?.[b.key] ?? false" :aria-label="`启用${b.label}告警`" :disabled="busy || !settings" @click="toggleFlag(b.key)"></button>
        <span class="b-name">{{ b.label }}</span>
        <span class="muted small">{{ b.hint }}</span>
        <form v-if="b.key === 'offline_enabled' && settings" class="grace" novalidate @submit.prevent="saveGrace">
          <label for="grace">宽限期（秒）</label>
          <input id="grace" class="gin" :value="graceShown" inputmode="numeric" autocomplete="off" @input="grace = ($event.target as HTMLInputElement).value" @blur="saveGrace" />
        </form>
      </div>
      <div v-if="graceErr" class="err" role="alert">{{ graceErr }}</div>
    </div>

    <div class="panel-head sub">
      <h4>自定义规则</h4>
      <span class="spacer"></span>
      <button class="btn" type="button" @click="dlg?.open(null)">新增规则</button>
    </div>
    <div class="table-wrap">
      <table>
        <thead><tr><th>名称</th><th>条件</th><th>适用节点</th><th>启用</th><th>操作</th></tr></thead>
        <tbody>
          <tr v-for="r in rules" :key="r.id">
            <td class="name">{{ r.name }}</td>
            <td class="cond">{{ ruleSummary(r, taskName) }}</td>
            <td>{{ scope(r) }}</td>
            <td><button type="button" class="switch" role="switch" :aria-checked="r.enabled" :aria-label="`启用规则 ${r.name}`" :disabled="busy" @click="toggleRule(r)"></button></td>
            <td class="ops"><div class="ops-in">
              <button type="button" class="act" @click="dlg?.open(r)">编辑</button>
              <ConfirmButton label="删除" confirm-label="确认删除？" danger :disabled="busy" @confirm="removeRule(r)" />
            </div></td>
          </tr>
          <tr v-if="loaded && rules.length === 0"><td colspan="5" class="empty">还没有自定义规则，点击「新增规则」创建</td></tr>
        </tbody>
      </table>
    </div>

    <h3>通知</h3>
    <div class="box notify">
      <form class="key" novalidate @submit.prevent="saveKey(false)">
        <div class="field">
          <label for="sk">Server酱³ SendKey</label>
          <input id="sk" v-model="key" type="password" :placeholder="settings?.sendkey_set ? `••••${settings.sendkey_tail ?? ''}` : '如 sctp123t…'" autocomplete="off" spellcheck="false" />
        </div>
        <button class="btn" type="submit" :disabled="busy">保存</button>
        <button class="btn ghost" type="button" :disabled="busy || !settings?.sendkey_set" @click="saveKey(true)">清除</button>
        <button class="btn ghost" type="button" :disabled="testing" @click="sendTest">{{ testing ? '发送中…' : '发送测试' }}</button>
      </form>
      <p class="muted small">已设置时留空保存不会修改 SendKey；SendKey 仅用于推送，不会回显。</p>
      <div v-if="keyErr" class="err" role="alert">{{ keyErr }}</div>
      <div v-if="keyMsg" class="ok" role="status">{{ keyMsg }}</div>
      <div v-if="testErr" class="err" role="alert">{{ testErr }}</div>
      <div v-if="testMsg" class="ok" role="status">{{ testMsg }}</div>
      <form class="key" novalidate @submit.prevent="saveTags">
        <div class="field">
          <label for="ptags">推送标签（可留空）</label>
          <input id="ptags" :value="tagsShown" placeholder="minimon|服务器" maxlength="120" autocomplete="off" spellcheck="false" @input="tags = ($event.target as HTMLInputElement).value" />
        </div>
        <button class="btn" type="submit" :disabled="busy">保存</button>
      </form>
      <p class="muted small">多个用 | 分隔，最多 5 个。推送时会自动附加类型标签：离线 / 上线 / 告警 / 恢复 / 流量 / 到期 / 测试，可在 Server酱³ App 中按标签筛选。</p>
      <div v-if="tagsErr" class="err" role="alert">{{ tagsErr }}</div>
      <div v-if="tagsMsg" class="ok" role="status">{{ tagsMsg }}</div>
    </div>

    <h3>告警历史</h3>
    <AlertHistory ref="history" :tz="tz" />

    <AlertRuleDialog ref="dlg" :nodes="sortedNodes" :tasks="tasks" @saved="onSaved" />
  </section>
</template>

<style scoped>
.panel-head { display: flex; align-items: center; gap: 12px; margin-bottom: 12px; flex-wrap: wrap; }
.panel-head h2 { font-size: 18px; font-weight: 650; margin: 0; }
.panel-head.sub { margin: 14px 0 8px; }
.panel-head h4 { margin: 0; font-size: 13px; color: var(--fg-2); }
h3 { font-size: 13px; font-weight: 600; color: var(--muted); letter-spacing: 0.04em; margin: 24px 0 8px; }
.spacer { flex: 1; }
.small { font-size: 12px; }
.page-err { margin-bottom: 10px; }
.ok { color: var(--ok); font-size: 13px; }
.box { background: var(--surface); border: 1px solid var(--line); box-shadow: var(--shadow); border-radius: var(--r); padding: 12px 16px; display: grid; gap: 8px; }
.ok-line { margin: 0; color: var(--ok); }
.active { list-style: none; margin: 0; padding: 0; display: grid; gap: 8px; }
.active li { display: flex; gap: 10px; align-items: baseline; flex-wrap: wrap; }
.dot { width: 8px; height: 8px; border-radius: 50%; background: var(--bad); align-self: center; }
.b-row { display: flex; gap: 10px; align-items: center; flex-wrap: wrap; }
.b-name { font-weight: 550; min-width: 64px; }
.grace { display: flex; gap: 8px; align-items: center; margin-left: auto; font-size: 13px; color: var(--fg-2); }
.gin { width: 80px; background: var(--bg); border: 1px solid var(--line); border-radius: 8px; padding: 5px 8px; }
.gin:focus { border-color: var(--accent); outline: none; }
.key { display: flex; gap: 8px; align-items: end; flex-wrap: wrap; }
.key .field { flex: 1; min-width: 220px; }
.notify p { margin: 0; }
.table-wrap { overflow-x: auto; background: var(--surface); border: 1px solid var(--line); box-shadow: var(--shadow); border-radius: var(--r); }
table { width: 100%; border-collapse: collapse; min-width: 640px; }
th, td { text-align: left; padding: 10px 12px; border-bottom: 1px solid var(--line); white-space: nowrap; }
th { font-size: 12px; color: var(--muted); font-weight: 500; background: var(--surface-2); }
tr:last-child td { border-bottom: 0; }
td.name { font-weight: 550; max-width: 160px; overflow: hidden; text-overflow: ellipsis; }
td.cond { max-width: 300px; overflow: hidden; text-overflow: ellipsis; }
td.empty { text-align: center; white-space: normal; }
.act { color: var(--accent); background: none; border: 0; cursor: pointer; padding: 2px 6px; border-radius: 6px; }
.ops-in { display: flex; gap: 2px; align-items: center; }
.switch { width: 32px; height: 18px; border-radius: 999px; background: var(--line); border: 0; position: relative; cursor: pointer; vertical-align: middle; flex: none; }
.switch::after { content: ""; position: absolute; top: 2px; left: 2px; width: 14px; height: 14px; border-radius: 50%; background: var(--fg); transition: left 0.15s; }
.switch[aria-checked="true"] { background: var(--ok); }
.switch[aria-checked="true"]::after { left: 16px; background: #fff; }
.switch:disabled { opacity: 0.6; cursor: default; }
</style>
