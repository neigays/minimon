<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useRoute } from 'vue-router'
import HistoryChart from '../components/HistoryChart.vue'
import StatusPill from '../components/StatusPill.vue'
import { useAuth } from '../auth'
import { formatBytes, formatUptime } from '../format'
import { buildCharts, fetchHistory, granularityNote, RANGES, toHistoryErr, type History, type HistoryErr, type Range } from '../history'
import { useLive, type PublicNode } from '../live'
import { statusOf } from '../status'

type Info = PublicNode & { hostname?: string | null; kernel?: string | null; agent_version?: string | null; ip?: string | null; note?: string }

const route = useRoute()
const { user } = useAuth()
const { nodes, connected, loaded } = useLive()

const id = computed(() => String(route.params.id))
const node = computed(() => nodes.value.find((n) => String(n.id) === id.value) as Info | undefined)
const range = ref<Range>('1h')
const hist = ref<History | null>(null)
const err = ref<HistoryErr | null>(null)
const loading = ref(true)
let seq = 0
let timer: ReturnType<typeof setInterval> | undefined

async function load(showLoading: boolean) {
  const mine = ++seq
  if (showLoading) loading.value = true
  try {
    const h = await fetchHistory(id.value, range.value)
    if (mine !== seq) return // 已有更新的请求，丢弃过期响应
    hist.value = h
    err.value = null
  } catch (e) {
    if (mine !== seq) return
    const he = toHistoryErr(e)
    if (he.kind === 'notfound' || !hist.value) {
      hist.value = null
      err.value = he
    }
  } finally {
    if (mine === seq) loading.value = false
  }
}

function selectRange(r: Range) {
  if (r === range.value) return
  range.value = r
  hist.value = null
  err.value = null
}

function tick() {
  if (range.value === '1h' && document.visibilityState === 'visible' && err.value?.kind !== 'notfound') void load(false)
}
function onVisible() {
  if (document.visibilityState === 'visible') tick()
}

watch([range, id], () => load(true))
onMounted(() => {
  void load(true)
  timer = setInterval(tick, 30_000)
  document.addEventListener('visibilitychange', onVisible)
})
onBeforeUnmount(() => {
  seq++
  clearInterval(timer)
  document.removeEventListener('visibilitychange', onVisible)
})

const built = computed(() => (hist.value ? buildCharts(hist.value, node.value?.mem_total) : null))
const status = computed(() => (node.value ? statusOf(node.value, connected.value) : 'offline'))
const r = computed(() => node.value?.latest ?? null)
const dash = (s: string | null | undefined) => (s ? s : '—')
const pub = computed(() => {
  const n = node.value
  if (!n) return []
  const disk = r.value?.disk_total || n.disk_total
  return [
    ['系统', [n.os, n.arch].filter(Boolean).join(' · ') || '—'],
    ['CPU', n.cpu_model ? `${n.cpu_model}${n.cpu_cores ? ` · ${n.cpu_cores} 核` : ''}` : dash(null)],
    ['内存', n.mem_total ? formatBytes(n.mem_total) : '—'],
    ['磁盘', disk ? formatBytes(disk) : '—'],
    ['虚拟化', dash(n.virtualization)],
    ['在线时长', r.value && status.value !== 'offline' ? formatUptime(r.value.uptime_secs) : '离线'],
  ]
})
const priv = computed(() => {
  const n = node.value
  if (!n || !user.value) return []
  return [
    ['IP', dash(n.ip)],
    ['主机名', dash(n.hostname)],
    ['内核', dash(n.kernel)],
    ['Agent 版本', dash(n.agent_version)],
    ['备注', dash(n.note)],
  ]
})
</script>

<template>
  <section aria-label="节点详情">
    <RouterLink class="back" to="/">
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><path d="M15 18l-6-6 6-6" /></svg>返回总览
    </RouterLink>

    <div v-if="err?.kind === 'notfound'" class="empty" role="alert">
      <p>{{ err.text }}</p>
      <RouterLink class="icon-btn" to="/">返回总览</RouterLink>
    </div>
    <template v-else>
      <div v-if="node" class="d-head">
        <span v-if="node.region" class="region">{{ node.region }}</span>
        <h1>{{ node.name }}</h1>
        <StatusPill :status="status" />
        <span v-if="node.group" class="gran">{{ node.group }}</span>
      </div>
      <p v-else-if="!loaded" class="muted">加载中…</p>

      <dl v-if="node" class="info">
        <div v-for="[k, v] in pub" :key="k"><dt>{{ k }}</dt><dd>{{ v }}</dd></div>
        <div v-for="[k, v] in priv" :key="k"><dt>{{ k }}<span class="lock">仅管理员</span></dt><dd class="num">{{ v }}</dd></div>
        <div v-if="!user"><dt>更多信息</dt><dd class="muted">IP、主机名等<RouterLink to="/login">登录后可见</RouterLink></dd></div>
      </dl>

      <div class="toolbar">
        <div class="seg" role="group" aria-label="时间范围">
          <button v-for="x in RANGES" :key="x.key" type="button" :aria-pressed="range === x.key" @click="selectRange(x.key)">{{ x.label }}</button>
        </div>
        <span v-if="hist" class="gran">{{ granularityNote(hist.granularity) }}</span>
      </div>

      <p v-if="err" class="err" role="alert">{{ err.text }}</p>
      <p v-else-if="loading && !hist" class="muted">加载中…</p>
      <p v-else-if="hist && hist.ts.length === 0" class="empty">该时间范围内暂无数据</p>
      <div v-else-if="built" class="charts">
        <HistoryChart v-for="c in built.charts" :key="c.key" :title="c.title" :series="c.series" :ts="built.ts" :unit="c.unit" :max="c.max" />
      </div>
    </template>
  </section>
</template>

<style scoped>
.back { display: inline-flex; align-items: center; gap: 4px; color: var(--muted); text-decoration: none; font-size: 13px; margin-bottom: 12px; }
.back:hover { color: var(--fg); }
.d-head { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; margin-bottom: 14px; }
.d-head h1 { font-size: 22px; margin: 0; font-weight: 650; overflow-wrap: anywhere; }
.region { font-size: 12px; color: var(--muted); border: 1px solid var(--line); border-radius: 4px; padding: 0 6px; }
.info { display: grid; grid-template-columns: repeat(auto-fill, minmax(190px, 1fr)); background: var(--surface); border-top: 1px solid var(--line); border-left: 1px solid var(--line); border-radius: var(--r); overflow: hidden; margin: 0 0 18px; box-shadow: 0 0 0 1px var(--line); }
.info > div { padding: 10px 14px; min-width: 0; border-right: 1px solid var(--line); border-bottom: 1px solid var(--line); }
.info dt { color: var(--muted); font-size: 12px; display: flex; align-items: center; gap: 6px; }
.info dd { margin: 2px 0 0; overflow-wrap: anywhere; }
.lock { font-size: 10px; color: var(--warn); border: 1px solid var(--warn-soft); background: var(--warn-soft); padding: 0 5px; border-radius: 4px; }
.toolbar { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; margin-bottom: 12px; }
.seg { display: inline-flex; border: 1px solid var(--line); border-radius: 8px; padding: 2px; background: var(--surface); }
.seg button { border: 0; background: transparent; padding: 4px 12px; border-radius: 6px; cursor: pointer; color: var(--muted); }
.seg button[aria-pressed="true"] { background: var(--surface-2); color: var(--fg); box-shadow: inset 0 0 0 1px var(--line); }
.gran { font-size: 12px; color: var(--muted); }
.charts { display: grid; grid-template-columns: repeat(auto-fill, minmax(min(100%, 420px), 1fr)); gap: 12px; }
</style>
