<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { api, ApiError } from '../../api'
import ConfirmButton from '../../components/ConfirmButton.vue'
import { useLive } from '../../live'

type Kind = 'icmp' | 'tcp' | 'http'
interface Task {
  id: number
  name: string
  kind: Kind
  target: string
  interval_secs: number
  samples: number
  timeout_ms: number
  all_nodes: boolean
  node_ids: number[]
  show_on_card: boolean
  sort: number
}

const KINDS: Record<Kind, string> = { icmp: 'ICMP', tcp: 'TCP', http: 'HTTP' }
const PLACEHOLDER: Record<Kind, string> = { icmp: '如 1.1.1.1 或 example.com', tcp: '如 example.com:443', http: '如 https://example.com/' }

const { nodes } = useLive()
const tasks = ref<Task[]>([])
// 三网模板：还没有任务时默认展开，已有任务时折叠成一行（只在首次加载时决定，之后随用户开合）。
const tplOpen = ref(false)
let tplDecided = false
const loaded = ref(false)
const pageErr = ref('')
const busy = ref(false)

const nodeName = (id: number) => nodes.value.find((n) => n.id === id)?.name ?? `#${id}`
const scope = (t: Task) => (t.all_nodes ? '全部节点' : t.node_ids.length ? t.node_ids.map(nodeName).join('、') : '未选择节点')

async function reload() {
  tasks.value = await api<Task[]>('GET', '/api/admin/probes')
  if (!tplDecided) {
    tplOpen.value = tasks.value.length === 0
    tplDecided = true
  }
  loaded.value = true
}
onMounted(async () => {
  try {
    await reload()
  } catch (e) {
    pageErr.value = e instanceof ApiError ? e.message : '加载失败'
  }
})

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
    await reload()
  } catch {
    // 已有错误提示
  }
  busy.value = false
}
const toggleCard = (t: Task) => run(() => api('PATCH', `/api/admin/probes/${t.id}`, { show_on_card: !t.show_on_card }))
const remove = (t: Task) => run(() => api('DELETE', `/api/admin/probes/${t.id}`))

// ---------- 三网模板 ----------
const isp = ref({ telecom: '', unicom: '', mobile: '' })
const ispErr = ref('')
const ispOk = ref('')
const ispBusy = ref(false)
async function createIsp() {
  if (ispBusy.value) return
  ispOk.value = ''
  const v = isp.value
  if (!v.telecom.trim() && !v.unicom.trim() && !v.mobile.trim()) return void (ispErr.value = '请至少填写一个运营商的目标')
  ispErr.value = ''
  ispBusy.value = true
  try {
    const body: Record<string, string> = {}
    if (v.telecom.trim()) body.telecom = v.telecom.trim()
    if (v.unicom.trim()) body.unicom = v.unicom.trim()
    if (v.mobile.trim()) body.mobile = v.mobile.trim()
    const r = await api<Task[]>('POST', '/api/admin/probes/isp-template', body)
    ispOk.value = `已创建 ${r.length} 个探测任务`
    isp.value = { telecom: '', unicom: '', mobile: '' }
    await reload()
  } catch (e) {
    ispErr.value = e instanceof ApiError ? e.message : '创建失败，请重试'
  } finally {
    ispBusy.value = false
  }
}

// ---------- 新建 / 编辑 ----------
const dlg = ref<HTMLDialogElement | null>(null)
const editing = ref<Task | null>(null)
const blank = () => ({ name: '', kind: 'icmp' as Kind, target: '', interval: '60', samples: '5', timeout: '5000', all: true, ids: [] as number[], card: true, sort: '0' })
const form = ref(blank())
const formErr = ref('')
let opener: HTMLElement | null = null

// 新建时切换类型联动默认样本数（icmp 5，其它 1）。
watch(() => form.value.kind, (k) => {
  if (!editing.value) form.value.samples = k === 'icmp' ? '5' : '1'
})

function openForm(t: Task | null) {
  opener = document.activeElement as HTMLElement | null
  editing.value = t
  form.value = t
    ? { name: t.name, kind: t.kind, target: t.target, interval: String(t.interval_secs), samples: String(t.samples), timeout: String(t.timeout_ms), all: t.all_nodes, ids: [...t.node_ids], card: t.show_on_card, sort: String(t.sort) }
    : blank()
  formErr.value = ''
  dlg.value?.showModal()
}
const closeForm = () => dlg.value?.close()
const onClose = () => opener?.focus()

function toggleNode(id: number) {
  const ids = form.value.ids
  form.value.ids = ids.includes(id) ? ids.filter((x) => x !== id) : [...ids, id]
}

async function submit() {
  const f = form.value
  const name = f.name.trim()
  const target = f.target.trim()
  if (!name) return void (formErr.value = '请输入名称')
  if (!target) return void (formErr.value = '请输入探测目标')
  const num = (s: string) => (s.trim() === '' ? NaN : Number(s))
  const interval = num(f.interval), samples = num(f.samples), timeout = num(f.timeout), sort = num(f.sort)
  if (![interval, samples, timeout].every(Number.isInteger)) return void (formErr.value = '间隔、样本数、超时需为整数')
  if (!f.all && f.ids.length === 0) return void (formErr.value = '请至少选择一个节点，或选择「全部节点」')
  formErr.value = ''
  const body: Record<string, unknown> = {
    name, kind: f.kind, target, interval_secs: interval, samples, timeout_ms: timeout,
    all_nodes: f.all, node_ids: f.all ? [] : f.ids, show_on_card: f.card,
  }
  if (editing.value) {
    if (!Number.isInteger(sort)) return void (formErr.value = '排序需为整数')
    body.sort = sort
  }
  if (busy.value) return
  busy.value = true
  try {
    if (editing.value) await api('PATCH', `/api/admin/probes/${editing.value.id}`, body)
    else await api('POST', '/api/admin/probes', body)
    closeForm()
    await reload()
  } catch (e) {
    formErr.value = e instanceof ApiError ? e.message : '保存失败，请重试'
  } finally {
    busy.value = false
  }
}
const sortedNodes = computed(() => [...nodes.value].sort((a, b) => a.sort - b.sort))
</script>

<template>
  <section aria-label="探测任务">
    <div class="panel-head">
      <h2>探测任务</h2>
      <span class="muted">共 {{ tasks.length }} 个</span>
      <span class="spacer"></span>
      <button class="btn" type="button" @click="openForm(null)">新建任务</button>
    </div>
    <div v-if="pageErr" class="err page-err" role="alert">{{ pageErr }}</div>

    <details class="tpl" :open="tplOpen" @toggle="tplOpen = ($event.target as HTMLDetailsElement).open">
      <summary class="tpl-sum"><span class="caret" aria-hidden="true">▸</span>三网延迟模板<span v-if="!tplOpen" class="muted small">一键创建电信 / 联通 / 移动三条 ICMP 探测</span></summary>
    <form class="tpl-body" novalidate @submit.prevent="createIsp">
      <div class="tpl-head">
        <span class="muted small">填写各运营商的测速 IP 或域名，例如当地电信/联通/移动的 DNS 或测速节点。留空的运营商不创建；任务为 ICMP，适用全部节点，显示在卡片上。</span>
      </div>
      <div class="tpl-grid">
        <div class="field"><label for="i-t">电信</label><input id="i-t" v-model="isp.telecom" placeholder="IP 或域名" autocomplete="off" spellcheck="false" /></div>
        <div class="field"><label for="i-u">联通</label><input id="i-u" v-model="isp.unicom" placeholder="IP 或域名" autocomplete="off" spellcheck="false" /></div>
        <div class="field"><label for="i-m">移动</label><input id="i-m" v-model="isp.mobile" placeholder="IP 或域名" autocomplete="off" spellcheck="false" /></div>
        <button class="btn" type="submit" :disabled="ispBusy">{{ ispBusy ? '创建中…' : '一键创建' }}</button>
      </div>
      <div v-if="ispErr" class="err" role="alert">{{ ispErr }}</div>
      <div v-if="ispOk" class="ok" role="status">{{ ispOk }}</div>
    </form>
    </details>

    <div class="table-wrap">
      <table>
        <thead><tr><th>名称</th><th>类型</th><th>目标</th><th>间隔</th><th>适用节点</th><th>卡片显示</th><th>操作</th></tr></thead>
        <tbody>
          <tr v-for="t in tasks" :key="t.id">
            <td class="name">{{ t.name }}</td>
            <td>{{ KINDS[t.kind] }}</td>
            <td class="num target">{{ t.target }}</td>
            <td class="num">{{ t.interval_secs }} 秒</td>
            <td class="scope">{{ scope(t) }}</td>
            <td><button type="button" class="switch" role="switch" :aria-checked="t.show_on_card" :aria-label="`在卡片显示 ${t.name}`" :disabled="busy" @click="toggleCard(t)"></button></td>
            <td class="ops"><div class="ops-in">
              <button type="button" class="act" @click="openForm(t)">编辑</button>
              <ConfirmButton label="删除" confirm-label="确认删除？" danger :disabled="busy" @confirm="remove(t)" />
            </div></td>
          </tr>
          <tr v-if="loaded && tasks.length === 0"><td colspan="7" class="empty">还没有探测任务，可使用上方「三网延迟模板」或点击「新建任务」</td></tr>
        </tbody>
      </table>
    </div>

    <dialog ref="dlg" class="dlg" aria-labelledby="pd-title" @close="onClose">
      <form novalidate @submit.prevent="submit">
        <h3 id="pd-title">{{ editing ? `编辑任务「${editing.name}」` : '新建探测任务' }}</h3>
        <div class="field"><label for="p-name">名称（1–32 字符）</label><input id="p-name" v-model="form.name" maxlength="32" autocomplete="off" autofocus /></div>
        <div class="field"><label for="p-kind">类型</label><select id="p-kind" v-model="form.kind"><option v-for="(l, k) in KINDS" :key="k" :value="k">{{ l }}</option></select></div>
        <div class="field"><label for="p-target">目标</label><input id="p-target" v-model="form.target" :placeholder="PLACEHOLDER[form.kind]" autocomplete="off" spellcheck="false" /></div>
        <div class="trio">
          <div class="field"><label for="p-int">间隔（10–3600 秒）</label><input id="p-int" v-model="form.interval" inputmode="numeric" autocomplete="off" /></div>
          <div class="field"><label for="p-smp">样本数（1–20）</label><input id="p-smp" v-model="form.samples" inputmode="numeric" autocomplete="off" /></div>
          <div class="field"><label for="p-to">超时（500–10000 毫秒）</label><input id="p-to" v-model="form.timeout" inputmode="numeric" autocomplete="off" /></div>
        </div>
        <div class="chk"><label><input v-model="form.all" type="checkbox" /> 适用全部节点</label></div>
        <fieldset v-if="!form.all" class="nodes">
          <legend>选择节点</legend>
          <label v-for="n in sortedNodes" :key="n.id"><input type="checkbox" :checked="form.ids.includes(n.id)" @change="toggleNode(n.id)" /> {{ n.name }}</label>
          <span v-if="sortedNodes.length === 0" class="muted">暂无节点</span>
        </fieldset>
        <div class="chk"><label><input v-model="form.card" type="checkbox" /> 在卡片上显示</label></div>
        <div v-if="editing" class="field"><label for="p-sort">排序（数字越小越靠前）</label><input id="p-sort" v-model="form.sort" inputmode="numeric" autocomplete="off" /></div>
        <div v-if="formErr" class="err" role="alert">{{ formErr }}</div>
        <div class="row end">
          <button class="btn ghost" type="button" @click="closeForm">取消</button>
          <button class="btn" type="submit" :disabled="busy">{{ editing ? '保存' : '创建' }}</button>
        </div>
      </form>
    </dialog>
  </section>
</template>

<style scoped>
.spacer { flex: 1; }
.small { font-size: 12px; }
.page-err { margin-bottom: 10px; }
.ok { color: var(--ok); font-size: 13px; }
.tpl { background: var(--surface); border: 1px solid var(--line); box-shadow: var(--shadow); border-radius: var(--r); padding: 12px 16px; margin-bottom: 14px; }
.tpl-sum { cursor: pointer; list-style: none; display: flex; align-items: center; gap: 8px; font-weight: 600; font-size: 14px; }
.tpl-sum::-webkit-details-marker { display: none; }
.caret { display: inline-block; transition: transform 0.15s; color: var(--muted); }
.tpl[open] .caret { transform: rotate(90deg); }
.tpl-body { display: grid; gap: 10px; margin-top: 10px; }
.tpl-head { display: grid; gap: 2px; }
.tpl-grid { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)) auto; gap: 10px; align-items: end; }
.table-wrap { overflow-x: auto; background: var(--surface); border: 1px solid var(--line); box-shadow: var(--shadow); border-radius: var(--r); }
table { width: 100%; border-collapse: collapse; min-width: 720px; }
th, td { text-align: left; padding: 10px 12px; border-bottom: 1px solid var(--line); white-space: nowrap; }
th { font-size: 12px; color: var(--muted); font-weight: 500; background: var(--surface-2); }
tr:last-child td { border-bottom: 0; }
td.name { font-weight: 550; max-width: 160px; overflow: hidden; text-overflow: ellipsis; }
td.target { max-width: 220px; overflow: hidden; text-overflow: ellipsis; }
td.scope { max-width: 200px; overflow: hidden; text-overflow: ellipsis; }
td.empty { text-align: center; white-space: normal; }
.act { color: var(--accent-text); background: none; border: 0; cursor: pointer; padding: 2px 6px; border-radius: 6px; }
.ops-in { display: flex; gap: 2px; align-items: center; }
.switch { width: 32px; height: 18px; border-radius: 999px; background: var(--line); border: 0; position: relative; cursor: pointer; vertical-align: middle; }
.switch::after { content: ""; position: absolute; top: 2px; left: 2px; width: 14px; height: 14px; border-radius: 50%; background: var(--fg); transition: left 0.15s; }
.switch[aria-checked="true"] { background: var(--ok); }
.switch[aria-checked="true"]::after { left: 16px; background: #fff; }
.switch:disabled { opacity: 0.6; cursor: default; }
.row { display: flex; gap: 8px; flex-wrap: wrap; align-items: center; }
.row.end { justify-content: flex-end; }
.dlg { background: var(--surface); color: var(--fg); border: 1px solid var(--line-strong); border-radius: var(--r); padding: 20px; width: min(460px, calc(100vw - 32px)); max-height: calc(100vh - 32px); }
.dlg::backdrop { background: rgba(0, 0, 0, 0.5); }
.dlg form { display: grid; gap: 12px; }
.dlg h3 { margin: 0; font-size: 15px; }
.dlg select { background: var(--bg); border: 1px solid var(--line); border-radius: 8px; padding: 9px 11px; }
.dlg select:focus, .dlg input:focus { border-color: var(--accent-text); outline: none; }
.trio { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 10px; }
.trio label { font-size: 12px; }
.chk label { display: inline-flex; gap: 8px; align-items: center; cursor: pointer; }
.nodes { border: 1px solid var(--line); border-radius: 8px; padding: 8px 12px; display: grid; gap: 6px; max-height: 160px; overflow: auto; margin: 0; }
.nodes legend { font-size: 12px; color: var(--muted); padding: 0 4px; }
.nodes label { display: flex; gap: 8px; align-items: center; cursor: pointer; }
@media (max-width: 640px) {
  .tpl-grid { grid-template-columns: minmax(0, 1fr); }
  .trio { grid-template-columns: minmax(0, 1fr); }
}
</style>
