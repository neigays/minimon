<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useRoute } from 'vue-router'
import HistoryChart from '../components/HistoryChart.vue'
import StatusPill from '../components/StatusPill.vue'
import { buildCharts, fetchHistory, granularityNote, RANGES, toHistoryErr, type History, type HistoryErr, type Range } from '../history'
import { useLive, type PublicNode } from '../live'
import { statusOf } from '../status'
import { buildProbeCharts, fetchProbeHistory, type ProbeHistory } from '../probe'
import TagList from '../components/TagList.vue'
import Flag from '../components/Flag.vue'

const route = useRoute()
const { nodes, connected, loaded } = useLive()

const id = computed(() => String(route.params.id))
const node = computed(() => nodes.value.find((n) => String(n.id) === id.value) as PublicNode | undefined)
const range = ref<Range>('1h')
const hist = ref<History | null>(null)
const probeHist = ref<ProbeHistory | null>(null)
const err = ref<HistoryErr | null>(null)
const loading = ref(true)
let seq = 0
let timer: ReturnType<typeof setInterval> | undefined

async function load(showLoading: boolean) {
  const mine = ++seq
  if (showLoading) loading.value = true
  // 探测历史失败不影响主图表：静默置空。
  const pp = fetchProbeHistory(id.value, range.value).catch(() => null)
  try {
    const h = await fetchHistory(id.value, range.value)
    const ph = await pp
    if (mine !== seq) return // 已有更新的请求，丢弃过期响应
    probeHist.value = ph
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
  probeHist.value = null
  err.value = null
}

function tick() {
  if (range.value === '1h' && document.visibilityState === 'visible' && err.value?.kind !== 'notfound') void load(false)
}
function onVisible() {
  if (document.visibilityState === 'visible') tick()
}

watch([range, id], (_n, [, oldId]) => {
  // 切换节点：立刻清掉上一个节点的数据与错误（含 notfound），避免短暂显示旧节点
  if (oldId !== id.value) {
    hist.value = null
    probeHist.value = null
    err.value = null
  }
  void load(true)
})
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

// memTotal 是原始值：live 推送替换 node 对象但数值不变时，built 不会重算，图表不会被重建。
const memTotal = computed(() => node.value?.mem_total ?? null)
const built = computed(() => (hist.value ? buildCharts(hist.value, memTotal.value) : null))
const probeBuilt = computed(() => {
  const h = probeHist.value
  if (!h || !h.tasks.some((t) => t.ts.length > 0)) return null
  return buildProbeCharts(h)
})
const status = computed(() => (node.value ? statusOf(node.value, connected.value) : 'offline'))
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
        <Flag v-if="node.region" :code="node.region" :size="18" />
        <h1>{{ node.name }}</h1>
        <StatusPill :status="status" />
        <span v-if="node.group" class="gran">{{ node.group }}</span>
        <TagList v-if="node.tags?.length" :tags="node.tags" />
      </div>
      <p v-else-if="!loaded" class="muted">加载中…</p>

      <div class="toolbar">
        <div class="seg" role="group" aria-label="时间范围">
          <button v-for="x in RANGES" :key="x.key" type="button" :aria-pressed="range === x.key" @click="selectRange(x.key)">{{ x.label }}</button>
        </div>
        <span v-if="hist" class="gran">{{ granularityNote(hist.granularity) }}</span>
      </div>

      <p v-if="err" class="err" role="alert">{{ err.text }}</p>
      <p v-else-if="loading && !hist" class="muted">加载中…</p>
      <p v-else-if="hist && hist.ts.length === 0 && !probeBuilt" class="empty">该时间范围内暂无数据</p>
      <div v-else-if="(built && hist && hist.ts.length > 0) || probeBuilt" class="charts">
        <template v-if="probeBuilt">
          <HistoryChart v-for="c in probeBuilt.charts" :key="c.key" :title="c.title" :series="c.series" :ts="probeBuilt.ts" :unit="c.unit" :max="c.max" />
        </template>
        <template v-if="built && hist && hist.ts.length > 0">
          <HistoryChart v-for="c in built.charts" :key="c.key" :title="c.title" :series="c.series" :ts="built.ts" :unit="c.unit" :max="c.max" />
        </template>
      </div>
    </template>
  </section>
</template>

<style scoped>
.back { display: inline-flex; align-items: center; gap: 4px; color: var(--muted); text-decoration: none; font-size: 13px; margin-bottom: 12px; }
.back:hover { color: var(--fg); }
.d-head { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; margin-bottom: 14px; }
.d-head h1 { font-size: 22px; margin: 0; font-weight: 650; overflow-wrap: anywhere; }
.toolbar { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; margin-bottom: 12px; }
.seg { display: inline-flex; border: 1px solid var(--line); border-radius: 8px; padding: 2px; background: var(--surface); }
.seg button { border: 0; background: transparent; padding: 4px 12px; border-radius: 6px; cursor: pointer; color: var(--muted); }
.seg button[aria-pressed="true"] { background: var(--surface-2); color: var(--fg); box-shadow: inset 0 0 0 1px var(--line); }
.gran { font-size: 12px; color: var(--muted); }
.charts { display: grid; grid-template-columns: repeat(auto-fill, minmax(min(100%, 420px), 1fr)); gap: 12px; }
</style>
