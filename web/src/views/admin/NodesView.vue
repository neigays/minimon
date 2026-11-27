<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { api, ApiError } from '../../api'
import { agentCommand, httpBase, installCommand, wsBase } from '../../agentCmd'
import ConfirmButton from '../../components/ConfirmButton.vue'
import StatusPill from '../../components/StatusPill.vue'
import { useLive, type PublicNode } from '../../live'
import { statusOf } from '../../status'
import { MODE_LABELS, type TrafficMode } from '../../traffic'
import { billingPatch, blankForm, formFromNode, infoPatch } from '../../nodeForm'
import Flag from '../../components/Flag.vue'
import { menuPlacement } from '../../menu'
import TagList from '../../components/TagList.vue'
import { previewTags } from '../../tags'

type AdminNode = PublicNode & { hidden: boolean; ip: string | null; note: string; traffic_quota: number | null; traffic_reset_day: number; traffic_mode: TrafficMode; alert_offline: boolean; tags_raw: string; ipv4: string | null; ipv6: string | null }

const { nodes: liveNodes, connected } = useLive()
const list = ref<AdminNode[]>([])
const loaded = ref(false)
const pageErr = ref('')
const busy = ref(false)
let publicUrl: string | null = null

// 管理字段来自 /api/admin/nodes；在线状态与最新上报以实时推送为准。
const rows = computed(() =>
  list.value.map((n) => {
    const l = liveNodes.value.find((x) => x.id === n.id)
    return { ...n, online: l ? l.online : n.online, latest: l ? l.latest : n.latest }
  }),
)

async function reload() {
  list.value = await api<AdminNode[]>('GET', '/api/admin/nodes')
  loaded.value = true
}

/** 执行一次变更并刷新表格；失败显示在页面顶部，返回是否成功。 */
async function run<T>(fn: () => Promise<T>): Promise<T | undefined> {
  if (busy.value) return undefined
  busy.value = true
  pageErr.value = ''
  try {
    const r = await fn()
    await reload()
    return r
  } catch (e) {
    pageErr.value = e instanceof ApiError ? e.message : '操作失败，请重试'
    try {
      await reload()
    } catch {
      // 已有错误提示
    }
    return undefined
  } finally {
    busy.value = false
  }
}

onMounted(async () => {
  try {
    await reload()
    publicUrl = (await api<{ public_url: string | null }>('GET', '/api/admin/settings')).public_url
  } catch (e) {
    pageErr.value = e instanceof ApiError ? e.message : '加载失败'
  }
})

// ---------- 排序 / 公开显示 ----------
function move(i: number, d: -1 | 1) {
  const ids = list.value.map((n) => n.id)
  const j = i + d
  if (j < 0 || j >= ids.length) return
  ;[ids[i], ids[j]] = [ids[j], ids[i]]
  void run(() => api('PUT', '/api/admin/nodes/order', { ids }))
}
function toggleHidden(n: AdminNode) {
  void run(() => api('PATCH', `/api/admin/nodes/${n.id}`, { hidden: !n.hidden }))
}
function remove(n: AdminNode) {
  void run(() => api('DELETE', `/api/admin/nodes/${n.id}`))
}

// ---------- 「更多」菜单：fixed 定位，点外面 / Esc / 滚动 / 操作后关闭，同时只开一个 ----------
const menu = ref<{ id: number; style: Record<string, string> } | null>(null)
function toggleMenu(n: AdminNode, e: MouseEvent) {
  if (menu.value?.id === n.id) return void (menu.value = null)
  const r = (e.currentTarget as HTMLElement).getBoundingClientRect()
  menu.value = { id: n.id, style: menuPlacement(r, { width: window.innerWidth, height: window.innerHeight }, 130) }
}
const closeMenu = () => (menu.value = null)
function onDocClick(e: MouseEvent) {
  if (menu.value && !(e.target as HTMLElement | null)?.closest('.more')) closeMenu()
}
function onKey(e: KeyboardEvent) {
  if (e.key === 'Escape') closeMenu()
}
onMounted(() => {
  document.addEventListener('click', onDocClick)
  document.addEventListener('keydown', onKey)
  window.addEventListener('scroll', closeMenu, true)
  window.addEventListener('resize', closeMenu)
})
onBeforeUnmount(() => {
  document.removeEventListener('click', onDocClick)
  document.removeEventListener('keydown', onKey)
  window.removeEventListener('scroll', closeMenu, true)
  window.removeEventListener('resize', closeMenu)
})

function toggleAlert(n: AdminNode) {
  void run(() => api('PATCH', `/api/admin/nodes/${n.id}`, { alert_offline: !n.alert_offline }))
}

// ---------- 安装命令面板（随时可打开；token 单独请求，不进节点列表） ----------
const MIRROR_KEY = 'minimon-mirror'
function loadMirror(): string {
  try {
    return localStorage.getItem(MIRROR_KEY) ?? ''
  } catch {
    return ''
  }
}
const inst = ref<{ node: AdminNode; title: string; token: string | null; loading: boolean; err: string } | null>(null)
const instEl = ref<HTMLElement | null>(null)
const useMirror = ref(loadMirror() !== '')
const mirror = ref(loadMirror())
const copyMsg = ref('')
const instCmd = computed(() => {
  const i = inst.value
  if (!i?.token) return null
  const ws = wsBase(publicUrl, location)
  return {
    command: installCommand(httpBase(publicUrl, location), ws, i.token, useMirror.value ? mirror.value : ''),
    manual: agentCommand(ws, i.token),
  }
})
function saveMirror() {
  try {
    if (useMirror.value && mirror.value.trim()) localStorage.setItem(MIRROR_KEY, mirror.value.trim())
    else localStorage.removeItem(MIRROR_KEY)
  } catch {
    // 仅为便利，失败无妨
  }
}
async function openInstall(n: AdminNode, title: string, token?: string) {
  copyMsg.value = ''
  inst.value = { node: n, title, token: token ?? null, loading: token === undefined, err: '' }
  await nextTick()
  instEl.value?.focus()
  if (token !== undefined) return
  try {
    const r = await api<{ token: string | null }>('GET', `/api/admin/nodes/${n.id}/token`)
    if (inst.value?.node.id === n.id) Object.assign(inst.value, { token: r.token, loading: false })
  } catch (e) {
    if (inst.value?.node.id === n.id) Object.assign(inst.value, { loading: false, err: e instanceof ApiError ? e.message : '读取失败，请重试' })
  }
}
// 节点被删除（本页或其它标签页）后，关掉它的安装命令面板。
watch(list, (l) => {
  if (inst.value && !l.some((n) => n.id === inst.value?.node.id)) inst.value = null
})
function copy() {
  const c = instCmd.value?.command
  if (!c) return
  saveMirror()
  // 必须在点击处理函数内同步调用，保持用户手势。
  try {
    navigator.clipboard.writeText(c).then(
      () => (copyMsg.value = '已复制'),
      () => (copyMsg.value = '复制失败，请手动选择'),
    )
  } catch {
    copyMsg.value = '复制失败，请手动选择'
  }
}
function resetToken(n: AdminNode) {
  void run(async () => {
    const r = await api<{ token: string }>('POST', `/api/admin/nodes/${n.id}/token`)
    await openInstall(n, `已重置节点「${n.name}」的 token（旧 token 已失效）`, r.token)
  })
}

// ---------- 弹层：新增（只填名称）/ 编辑信息 / 账单（原生 dialog：焦点陷阱、Esc 关闭、关闭后还原焦点） ----------
type Mode = 'create' | 'info' | 'billing'
const dlg = ref<HTMLDialogElement | null>(null)
const mode = ref<Mode>('create')
const editing = ref<AdminNode | null>(null)
let seed = blankForm()
const form = ref(blankForm())
const tagPreview = computed(() => previewTags(form.value.tags))
const formErr = ref('')
const dlgTitle = computed(() => {
  const n = editing.value?.name ?? ''
  return mode.value === 'create' ? '新增节点' : mode.value === 'info' ? `编辑信息「${n}」` : `账单「${n}」`
})

let opener: HTMLElement | null = null
function openForm(m: Mode, n: AdminNode | null) {
  opener = document.activeElement as HTMLElement | null
  mode.value = m
  editing.value = n
  seed = n ? formFromNode(n) : blankForm()
  form.value = { ...seed }
  formErr.value = ''
  // 等本模式的字段渲染完再打开：dialog 的 autofocus 才会落在当前模式的第一个输入框上
  void nextTick(() => dlg.value?.showModal())
}
function closeForm() {
  dlg.value?.close()
}
function onDialogClose() {
  // 新增成功后焦点交给安装命令面板，其余情况还给触发按钮。
  if (mode.value !== 'create' || !inst.value) opener?.focus()
}
async function submitForm() {
  const f = form.value
  let body: Record<string, unknown>
  if (mode.value === 'billing') {
    const q = f.quota.trim()
    const qv = Number(q)
    if (q && (!Number.isFinite(qv) || qv <= 0)) return void (formErr.value = '月额度需为大于 0 的数字，留空表示不限')
    const day = Number(f.resetDay)
    if (!Number.isInteger(day) || day < 1 || day > 31) return void (formErr.value = '重置日需为 1 到 31 的整数')
    body = billingPatch(seed, f)
  } else {
    const name = f.name.trim()
    if (!name) return void (formErr.value = '请输入名称')
    body = mode.value === 'create' ? { name } : infoPatch(seed, f)
  }
  formErr.value = ''
  if (mode.value !== 'create' && Object.keys(body).length === 0) return void closeForm()
  if (busy.value) return
  busy.value = true
  try {
    if (mode.value === 'create') {
      const r = await api<{ node: AdminNode; token: string }>('POST', '/api/admin/nodes', body)
      closeForm()
      await reload()
      await openInstall(r.node, `已创建节点「${r.node.name}」`, r.token)
    } else {
      await api('PATCH', `/api/admin/nodes/${editing.value?.id}`, body)
      closeForm()
      await reload()
    }
  } catch (e) {
    formErr.value = e instanceof ApiError ? e.message : '保存失败，请重试'
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <section aria-label="节点管理">
    <div class="panel-head">
      <h2>节点管理</h2>
      <span class="muted">共 {{ list.length }} 个</span>
      <span class="spacer"></span>
      <button class="btn" type="button" @click="openForm('create', null)">新增节点</button>
    </div>
    <div v-if="pageErr" class="err page-err" role="alert">{{ pageErr }}</div>

    <div v-if="inst" ref="instEl" class="drawer" tabindex="-1" role="region" aria-label="安装命令">
      <h3>{{ inst.title || `安装命令「${inst.node.name}」` }}</h3>
      <p v-if="inst.loading" class="sub">读取中…</p>
      <div v-else-if="inst.err" class="err" role="alert">{{ inst.err }}</div>
      <template v-else-if="instCmd">
        <pre class="code" tabindex="0" aria-label="一键安装命令">{{ instCmd.command }}</pre>
        <div class="row">
          <label class="chk-inline"><input v-model="useMirror" type="checkbox" /> 使用国内镜像</label>
          <input v-if="useMirror" v-model="mirror" class="mirror" placeholder="代理前缀，如 https://ghproxy.example/" autocomplete="off" spellcheck="false" aria-label="镜像代理前缀" />
        </div>
        <p class="sub">在要监控的机器上执行；升级 agent 时重新执行同一条命令即可。已手动安装二进制时：</p>
        <pre class="code" tabindex="0" aria-label="agent 启动命令">{{ instCmd.manual }}</pre>
      </template>
      <div v-else class="hint">
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><path d="M12 9v4M12 17h.01" /><path d="M10.3 3.9L1.8 18a2 2 0 001.7 3h17a2 2 0 001.7-3L13.7 3.9a2 2 0 00-3.4 0z" /></svg>
        <span>此节点创建于旧版本，只保存了 token 的哈希。重置 token 后即可随时查看安装命令（在线的 agent 需用新命令重新安装）。</span>
      </div>
      <div class="row">
        <button v-if="instCmd" class="btn" type="button" @click="copy">复制命令</button>
        <ConfirmButton v-if="!inst.loading && !inst.err && !instCmd" label="重置 token" confirm-label="确认重置？" :disabled="busy" @confirm="resetToken(inst.node)" />
        <button class="btn ghost" type="button" @click="inst = null">关闭</button>
        <span class="muted" role="status">{{ copyMsg }}</span>
      </div>
    </div>

    <div class="table-wrap">
      <table>
        <thead><tr><th>排序</th><th>名称</th><th>分组</th><th>地区</th><th>状态</th><th>IP</th><th>公开显示</th><th>操作</th></tr></thead>
        <tbody>
          <tr v-for="(n, i) in rows" :key="n.id">
            <td class="sort">
              <button type="button" class="mv" :disabled="busy || i === 0" :aria-label="`上移 ${n.name}`" @click="move(i, -1)">↑</button>
              <button type="button" class="mv" :disabled="busy || i === rows.length - 1" :aria-label="`下移 ${n.name}`" @click="move(i, 1)">↓</button>
            </td>
            <td class="name">{{ n.name }}</td>
            <td class="grp">{{ n.group || '—' }}</td>
            <td class="reg"><template v-if="n.region"><Flag :code="n.region" /> {{ n.region }}</template><template v-else>—</template></td>
            <td class="st"><StatusPill :status="statusOf(n, connected)" /></td>
            <td class="num ip">
              <div>{{ n.ipv4 || n.ip || '—' }}</div>
              <div v-if="n.ipv6" class="v6">{{ n.ipv6 }}</div>
            </td>
            <td class="pub">
              <button type="button" class="switch" role="switch" :aria-checked="!n.hidden" :aria-label="`在公开页显示 ${n.name}`" :disabled="busy" @click="toggleHidden(n)"></button>
            </td>
            <td class="ops"><div class="ops-in">
              <button type="button" class="act" @click="openForm('info', n)">编辑信息</button>
              <button type="button" class="act" @click="openForm('billing', n)">账单</button>
              <button type="button" class="act" @click="openInstall(n, '')">安装命令</button>
              <span class="more">
                <button type="button" class="act" :aria-expanded="menu?.id === n.id" aria-haspopup="menu" :aria-label="`${n.name} 的更多操作`" @click="toggleMenu(n, $event)">更多</button>
                <div v-if="menu?.id === n.id" class="menu" role="menu" :style="menu.style">
                  <button type="button" class="act" role="menuitem" :disabled="busy" @click="closeMenu(); toggleAlert(n)">离线告警：{{ n.alert_offline ? '开' : '关' }}</button>
                  <ConfirmButton label="重置 token" confirm-label="确认重置？" :disabled="busy" @confirm="closeMenu(); resetToken(n)" />
                  <ConfirmButton label="删除节点" confirm-label="确认删除？" danger :disabled="busy" @confirm="closeMenu(); remove(n)" />
                </div>
              </span>
            </div></td>
          </tr>
          <tr v-if="loaded && rows.length === 0"><td colspan="8" class="empty">还没有节点，点击右上角「新增节点」</td></tr>
        </tbody>
      </table>
    </div>

    <dialog ref="dlg" class="dlg" aria-labelledby="dlg-title" @close="onDialogClose">
      <form novalidate @submit.prevent="submitForm">
        <h3 id="dlg-title">{{ dlgTitle }}</h3>
        <template v-if="mode !== 'billing'">
          <div class="field"><label for="f-name">名称</label><input id="f-name" v-model="form.name" maxlength="64" autofocus autocomplete="off" /></div>
          <p v-if="mode === 'create'" class="sub">创建后显示安装命令；分组、标签、账单等可稍后在节点行上编辑。地区与 IP 由 agent 自动上报。</p>
        </template>
        <template v-if="mode === 'info'">
          <div class="field"><label for="f-group">分组</label><input id="f-group" v-model="form.group" maxlength="32" autocomplete="off" /></div>
          <div class="field">
            <label for="f-tags">标签（多个标签用 ; 分隔，末尾加 &lt;颜色&gt; 指定颜色）</label>
            <input id="f-tags" v-model="form.tags" maxlength="400" placeholder="1Gbps<yellow>;CN2 GIA<red>" autocomplete="off" aria-describedby="f-tags-help" />
            <div id="f-tags-help" class="tag-help">
              <template v-if="tagPreview.length">
                <TagList :tags="tagPreview" />
                <div v-if="tagPreview.some((t) => t.invalid)" class="err-hint">标红的标签保存时会被拒绝</div>
              </template>
              <span v-else class="muted">可用颜色：gray、red、orange、yellow、green、blue、purple、pink 等 Radix 色板名称，不写为 gray</span>
            </div>
          </div>
          <div class="field"><label for="f-note">备注（仅管理员可见）</label><input id="f-note" v-model="form.note" maxlength="500" autocomplete="off" /></div>
        </template>
        <template v-if="mode === 'billing'">
          <div class="field">
            <label for="f-quota">月流量额度（留空表示不限，十进制：1 GB = 10⁹ 字节）</label>
            <div class="inline">
              <input id="f-quota" v-model="form.quota" inputmode="decimal" placeholder="不限" autocomplete="off" autofocus />
              <select v-model="form.unit" aria-label="额度单位"><option>GB</option><option>TB</option></select>
            </div>
          </div>
          <div class="field"><label for="f-day">流量重置日（1–31，当月没有该日取月末）</label><input id="f-day" v-model="form.resetDay" inputmode="numeric" maxlength="2" autocomplete="off" /></div>
          <div class="field"><label for="f-mode">计费口径</label><select id="f-mode" v-model="form.mode"><option v-for="(l, k) in MODE_LABELS" :key="k" :value="k">{{ l }}</option></select></div>
          <div class="field">
            <label for="f-exp">到期日（仅管理员可见）</label>
            <div class="inline"><input id="f-exp" v-model="form.expire" type="date" /><button class="btn ghost" type="button" :disabled="!form.expire" @click="form.expire = ''">清空</button></div>
          </div>
        </template>
        <div v-if="formErr" class="err" role="alert">{{ formErr }}</div>
        <div class="row end">
          <button class="btn ghost" type="button" @click="closeForm">取消</button>
          <button class="btn" type="submit" :disabled="busy">{{ mode === 'create' ? '创建' : '保存' }}</button>
        </div>
      </form>
    </dialog>
  </section>
</template>

<style scoped>
td.ip .v6 { font-size: 11px; color: var(--muted); }
.more { position: relative; display: inline-block; }
.more .menu { z-index: 20; background: var(--surface); border: 1px solid var(--line-strong); border-radius: 8px; padding: 6px; display: grid; gap: 2px; min-width: 140px; box-shadow: 0 6px 20px rgba(0, 0, 0, 0.25); }
.more .menu .act { text-align: left; }
.chk-inline { display: inline-flex; gap: 6px; align-items: center; font-size: 13px; cursor: pointer; }
.mirror { flex: 1; min-width: 220px; }
.tag-help { margin-top: 6px; font-size: 12px; min-height: 18px; }
.err-hint { margin-top: 4px; color: var(--bad); }
.spacer { flex: 1; }
.page-err { margin-bottom: 10px; }
.table-wrap { overflow-x: auto; background: var(--surface); border: 1px solid var(--line); box-shadow: var(--shadow); border-radius: var(--r); }
table { width: 100%; border-collapse: collapse; min-width: 760px; }
th, td { text-align: left; padding: 10px 12px; border-bottom: 1px solid var(--line); white-space: nowrap; }
th { font-size: 12px; color: var(--muted); font-weight: 500; background: var(--surface-2); }
tr:last-child td { border-bottom: 0; }
td.name { font-weight: 550; max-width: 220px; overflow: hidden; text-overflow: ellipsis; }
td.empty { text-align: center; }
.mv { background: none; border: 1px solid var(--line); border-radius: 6px; width: 26px; height: 26px; cursor: pointer; color: var(--muted); margin-right: 4px; }
.mv:hover:not(:disabled) { color: var(--fg); border-color: var(--line-strong); }
.mv:disabled { opacity: 0.35; cursor: default; }
.act { color: var(--accent-text); background: none; border: 0; cursor: pointer; padding: 2px 6px; border-radius: 6px; }
.ops-in { display: flex; gap: 2px; align-items: center; flex-wrap: wrap; }
.switch { width: 32px; height: 18px; border-radius: 999px; background: var(--line); border: 0; position: relative; cursor: pointer; vertical-align: middle; }
.switch::after { content: ""; position: absolute; top: 2px; left: 2px; width: 14px; height: 14px; border-radius: 50%; background: var(--fg); transition: left 0.15s; }
.switch[aria-checked="true"] { background: var(--ok); }
.switch[aria-checked="true"]::after { left: 16px; background: #fff; }
.switch:disabled { opacity: 0.6; cursor: default; }
.drawer { margin-bottom: 14px; background: var(--surface); border: 1px solid var(--line-strong); border-radius: var(--r); padding: 16px; display: grid; gap: 12px; }
.drawer h3 { margin: 0; font-size: 14px; }
.code { margin: 0; font-family: var(--mono); font-size: 12px; background: var(--bg); border: 1px solid var(--line); border-radius: 8px; padding: 10px 12px; overflow-x: auto; white-space: pre-wrap; word-break: break-all; color: var(--fg-2); user-select: all; }
.sub { margin: 0; font-size: 12px; color: var(--muted); }
.hint { font-size: 12px; color: var(--muted); display: flex; gap: 6px; align-items: flex-start; }
.hint svg { flex: none; margin-top: 2px; color: var(--warn); }
.row { display: flex; gap: 8px; flex-wrap: wrap; align-items: center; }
.row.end { justify-content: flex-end; }
.dlg { background: var(--surface); color: var(--fg); border: 1px solid var(--line-strong); border-radius: var(--r); padding: 20px; width: min(420px, calc(100vw - 32px)); }
.dlg::backdrop { background: rgba(0, 0, 0, 0.5); }
.dlg form { display: grid; gap: 12px; }
.dlg { max-height: calc(100vh - 32px); }
.chk label { display: inline-flex; gap: 8px; align-items: center; cursor: pointer; }
.inline { display: flex; gap: 8px; }
.inline input { flex: 1; min-width: 0; }
.dlg select, .dlg input[type=date] { background: var(--bg); border: 1px solid var(--line); border-radius: 8px; padding: 9px 11px; }
.dlg select:focus, .dlg input:focus { border-color: var(--accent-text); outline: none; }
.dlg h3 { margin: 0; font-size: 15px; }
/* 手机：表格改为每个节点一张卡片，无需横向滚动 */
@media (max-width: 640px) {
  .table-wrap { overflow: visible; background: none; border: 0; }
  table { min-width: 0; }
  thead { display: none; }
  tbody { display: grid; gap: 10px; }
  tr { display: grid; grid-template-columns: auto minmax(0, 1fr) auto; grid-template-areas: "sort name st" "reg grp grp" "ip ip ip" "pub ops ops"; gap: 8px 10px; align-items: center; background: var(--surface); border: 1px solid var(--line); box-shadow: var(--shadow); border-radius: var(--r); padding: 12px; }
  td { display: block; padding: 0; border: 0; white-space: normal; }
  td.sort { grid-area: sort; }
  td.name { grid-area: name; max-width: none; }
  td.st { grid-area: st; }
  td.reg { grid-area: reg; }
  td.grp { grid-area: grp; color: var(--muted); }
  td.ip { grid-area: ip; min-width: 0; overflow-wrap: anywhere; }
  td.pub { grid-area: pub; }
  td.ops { grid-area: ops; }
  td.ops .ops-in { justify-content: flex-end; }
  td.empty { grid-column: 1 / -1; }
}
</style>
