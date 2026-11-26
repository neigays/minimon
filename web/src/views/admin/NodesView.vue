<script setup lang="ts">
import { computed, nextTick, onMounted, ref } from 'vue'
import { api, ApiError } from '../../api'
import { agentCommand, httpBase, installCommand, wsBase } from '../../agentCmd'
import ConfirmButton from '../../components/ConfirmButton.vue'
import StatusPill from '../../components/StatusPill.vue'
import { useLive, type PublicNode } from '../../live'
import { statusOf } from '../../status'
import { MODE_LABELS, type TrafficMode } from '../../traffic'
import { blankForm, diffPatch, formFromNode } from '../../nodeForm'
import TagList from '../../components/TagList.vue'
import { previewTags } from '../../tags'

type AdminNode = PublicNode & { hidden: boolean; ip: string | null; note: string; traffic_quota: number | null; traffic_reset_day: number; traffic_mode: TrafficMode; alert_offline: boolean; tags_raw: string }

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

// ---------- 新增 / 重置 token 抽屉 ----------
const drawer = ref<{ title: string; command: string; manual: string } | null>(null)
const drawerEl = ref<HTMLElement | null>(null)
const copyMsg = ref('')

async function showDrawer(title: string, token: string) {
  const ws = wsBase(publicUrl, location)
  drawer.value = {
    title,
    command: installCommand(httpBase(publicUrl, location), ws, token),
    manual: agentCommand(ws, token),
  }
  copyMsg.value = ''
  await nextTick()
  drawerEl.value?.focus()
}
function copy() {
  if (!drawer.value) return
  // 必须在点击处理函数内同步调用，保持用户手势。
  try {
    navigator.clipboard.writeText(drawer.value.command).then(
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
    await showDrawer(`已重置节点「${n.name}」的 token`, r.token)
  })
}

// ---------- 新增 / 编辑弹层（原生 dialog：自带焦点陷阱、Esc 关闭、关闭后还原焦点） ----------
const dlg = ref<HTMLDialogElement | null>(null)
const editing = ref<AdminNode | null>(null) // null 表示新增
let seed = blankForm()
const form = ref(blankForm())
const tagPreview = computed(() => previewTags(form.value.tags))
const formErr = ref('')

let opener: HTMLElement | null = null
function openForm(n: AdminNode | null) {
  opener = document.activeElement as HTMLElement | null
  editing.value = n
  seed = n ? formFromNode(n) : blankForm()
  form.value = { ...seed }
  formErr.value = ''
  dlg.value?.showModal()
}
function closeForm() {
  dlg.value?.close()
}
function onDialogClose() {
  // 新增成功后焦点交给接入面板，其余情况还给触发按钮。
  if (!drawer.value || editing.value) opener?.focus()
}
function onRegion(e: Event) {
  const el = e.target as HTMLInputElement
  el.value = el.value.replace(/[^a-zA-Z]/g, '').toUpperCase().slice(0, 2)
  form.value.region = el.value
}
async function submitForm() {
  const f = form.value
  const name = f.name.trim()
  if (!name) return void (formErr.value = '请输入名称')
  if (f.region && f.region.length !== 2) return void (formErr.value = '地区需为空或 2 位字母代码')
  formErr.value = ''
  const body: Record<string, unknown> = { name, group: f.group.trim(), region: f.region, note: f.note }
  if (editing.value) {
    const q = f.quota.trim()
    const qv = Number(q)
    if (q && (!Number.isFinite(qv) || qv <= 0)) return void (formErr.value = '月额度需为大于 0 的数字，留空表示不限')
    const day = Number(f.resetDay)
    if (!Number.isInteger(day) || day < 1 || day > 31) return void (formErr.value = '重置日需为 1 到 31 的整数')
    // 只提交相对打开时有变化的字段，避免误改未动过的计费配置。
    for (const k of Object.keys(body)) delete body[k]
    Object.assign(body, diffPatch(seed, f))
    if (Object.keys(body).length === 0) return void closeForm()
  }
  if (busy.value) return
  busy.value = true
  try {
    if (editing.value) {
      await api('PATCH', `/api/admin/nodes/${editing.value.id}`, body)
      closeForm()
    } else {
      const r = await api<{ node: AdminNode; token: string }>('POST', '/api/admin/nodes', body)
      closeForm()
      await showDrawer(`已创建节点「${r.node.name}」`, r.token)
    }
    await reload()
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
      <button class="btn" type="button" @click="openForm(null)">新增节点</button>
    </div>
    <div v-if="pageErr" class="err page-err" role="alert">{{ pageErr }}</div>

    <div v-if="drawer" ref="drawerEl" class="drawer" tabindex="-1" role="region" aria-label="新节点接入">
      <h3>{{ drawer.title }}</h3>
      <div class="hint">
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><path d="M12 9v4M12 17h.01" /><path d="M10.3 3.9L1.8 18a2 2 0 001.7 3h17a2 2 0 001.7-3L13.7 3.9a2 2 0 00-3.4 0z" /></svg>
        <span>token 只显示这一次，关闭后无法再查看，只能重置。</span>
      </div>
      <pre class="code" tabindex="0" aria-label="一键安装命令">{{ drawer.command }}</pre>
      <p class="sub">国内 VPS 下载慢可在末尾加 <code>--mirror &lt;代理前缀&gt;</code>，如 <code>https://ghproxy.example/</code></p>
      <p class="sub">已手动安装二进制：</p>
      <pre class="code" tabindex="0" aria-label="agent 启动命令">{{ drawer.manual }}</pre>
      <div class="row">
        <button class="btn" type="button" @click="copy">复制命令</button>
        <button class="btn ghost" type="button" @click="drawer = null">我已保存，关闭</button>
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
            <td>{{ n.group || '—' }}</td>
            <td>{{ n.region || '—' }}</td>
            <td><StatusPill :status="statusOf(n, connected)" /></td>
            <td class="num">{{ n.ip || '—' }}</td>
            <td>
              <button type="button" class="switch" role="switch" :aria-checked="!n.hidden" :aria-label="`在公开页显示 ${n.name}`" :disabled="busy" @click="toggleHidden(n)"></button>
            </td>
            <td class="ops">
              <button type="button" class="act" @click="openForm(n)">编辑</button>
              <ConfirmButton label="重置 token" confirm-label="确认重置？" :disabled="busy" @confirm="resetToken(n)" />
              <ConfirmButton label="删除" confirm-label="确认删除？" danger :disabled="busy" @confirm="remove(n)" />
            </td>
          </tr>
          <tr v-if="loaded && rows.length === 0"><td colspan="8" class="empty">还没有节点，点击右上角「新增节点」</td></tr>
        </tbody>
      </table>
    </div>

    <dialog ref="dlg" class="dlg" aria-labelledby="dlg-title" @close="onDialogClose">
      <form novalidate @submit.prevent="submitForm">
        <h3 id="dlg-title">{{ editing ? `编辑节点「${editing.name}」` : '新增节点' }}</h3>
        <div class="field"><label for="f-name">名称</label><input id="f-name" v-model="form.name" maxlength="64" autofocus autocomplete="off" /></div>
        <div class="field"><label for="f-group">分组</label><input id="f-group" v-model="form.group" maxlength="32" autocomplete="off" /></div>
        <div class="field"><label for="f-region">地区（2 位字母代码，可留空）</label><input id="f-region" :value="form.region" maxlength="2" placeholder="如 HK" autocomplete="off" @input="onRegion" /></div>
        <div class="field"><label for="f-note">备注（仅管理员可见）</label><input id="f-note" v-model="form.note" maxlength="500" autocomplete="off" /></div>
        <template v-if="editing">
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
          <div class="field">
            <label for="f-quota">月流量额度（留空表示不限，十进制：1 GB = 10⁹ 字节）</label>
            <div class="inline">
              <input id="f-quota" v-model="form.quota" inputmode="decimal" placeholder="不限" autocomplete="off" />
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
        <div v-if="editing" class="chk"><label><input v-model="form.alertOffline" type="checkbox" /> 离线告警（节点断线时推送通知）</label></div>
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
.tag-help { margin-top: 6px; font-size: 12px; min-height: 18px; }
.err-hint { margin-top: 4px; color: var(--bad); }
.panel-head { display: flex; align-items: center; gap: 12px; margin-bottom: 12px; flex-wrap: wrap; }
.panel-head h2 { font-size: 16px; margin: 0; }
.spacer { flex: 1; }
.page-err { margin-bottom: 10px; }
.table-wrap { overflow-x: auto; background: var(--surface); border: 1px solid var(--line); border-radius: var(--r); }
table { width: 100%; border-collapse: collapse; min-width: 760px; }
th, td { text-align: left; padding: 10px 12px; border-bottom: 1px solid var(--line); white-space: nowrap; }
th { font-size: 12px; color: var(--muted); font-weight: 500; background: var(--surface-2); }
tr:last-child td { border-bottom: 0; }
td.name { font-weight: 550; max-width: 220px; overflow: hidden; text-overflow: ellipsis; }
td.empty { text-align: center; }
.mv { background: none; border: 1px solid var(--line); border-radius: 6px; width: 26px; height: 26px; cursor: pointer; color: var(--muted); margin-right: 4px; }
.mv:hover:not(:disabled) { color: var(--fg); border-color: var(--line-strong); }
.mv:disabled { opacity: 0.35; cursor: default; }
.act { color: var(--c1); background: none; border: 0; cursor: pointer; padding: 2px 6px; border-radius: 6px; }
.ops { display: flex; gap: 2px; align-items: center; }
.switch { width: 32px; height: 18px; border-radius: 999px; background: var(--line); border: 0; position: relative; cursor: pointer; vertical-align: middle; }
.switch::after { content: ""; position: absolute; top: 2px; left: 2px; width: 14px; height: 14px; border-radius: 50%; background: var(--fg); transition: left 0.15s; }
.switch[aria-checked="true"] { background: var(--ok); }
.switch[aria-checked="true"]::after { left: 16px; background: #fff; }
.switch:disabled { opacity: 0.6; cursor: default; }
.drawer { margin-bottom: 14px; background: var(--surface); border: 1px solid var(--line-strong); border-radius: var(--r); padding: 16px; display: grid; gap: 12px; }
.drawer h3 { margin: 0; font-size: 14px; }
.code { margin: 0; font-family: var(--mono); font-size: 12px; background: var(--bg); border: 1px solid var(--line); border-radius: 8px; padding: 10px 12px; overflow-x: auto; white-space: pre; color: var(--fg-2); user-select: all; }
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
.dlg select:focus, .dlg input:focus { border-color: var(--c1); outline: none; }
.dlg h3 { margin: 0; font-size: 15px; }
</style>
