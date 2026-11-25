<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import NodeCard from '../components/NodeCard.vue'
import { formatRate } from '../format'
import { useLive } from '../live'
import { statusOf } from '../status'

const { nodes, connected, loaded, now } = useLive()
const ALL = '全部'
const group = ref(ALL)

const sorted = computed(() => [...nodes.value].sort((a, b) => a.sort - b.sort || a.id - b.id))
const groupName = (g: string) => g || '未分组'
const groups = computed(() => {
  const counts = new Map<string, number>()
  for (const n of sorted.value) counts.set(groupName(n.group), (counts.get(groupName(n.group)) ?? 0) + 1)
  return [...counts]
})
// 选中的分组被删光后回到「全部」。
watch(groups, (g) => {
  if (group.value !== ALL && !g.some(([name]) => name === group.value)) group.value = ALL
})
const visible = computed(() => sorted.value.filter((n) => group.value === ALL || groupName(n.group) === group.value))

const summary = computed(() => {
  const st = sorted.value.map((n) => ({ n, s: statusOf(n, connected.value) }))
  const up = st.filter((x) => x.s !== 'offline')
  return {
    total: st.length,
    online: up.length,
    offline: st.length - up.length,
    busy: st.filter((x) => x.s === 'busy').length,
    rx: up.reduce((a, x) => a + (x.n.latest?.net_rx_rate ?? 0), 0),
    tx: up.reduce((a, x) => a + (x.n.latest?.net_tx_rate ?? 0), 0),
  }
})
</script>

<template>
  <section aria-label="节点总览">
    <p v-if="!loaded" class="empty">{{ connected ? '加载中…' : '正在连接服务器…' }}</p>
    <p v-else-if="nodes.length === 0" class="empty">暂无节点</p>
    <template v-else>
      <div class="summary">
        <span><span class="k">在线</span><span class="v num">{{ summary.online }}</span><span class="k tail">/ {{ summary.total }}</span></span>
        <span><span class="k">离线</span><span class="v num" :class="{ bad: summary.offline }">{{ summary.offline }}</span></span>
        <span><span class="k">负载高</span><span class="v num" :class="{ warn: summary.busy }">{{ summary.busy }}</span></span>
        <span><span class="k">总下行</span><span class="v num">{{ formatRate(summary.rx) }}</span></span>
        <span><span class="k">总上行</span><span class="v num">{{ formatRate(summary.tx) }}</span></span>
      </div>
      <div class="chips" role="group" aria-label="按分组筛选">
        <button type="button" class="chip" :aria-pressed="group === ALL" @click="group = ALL">{{ ALL }}<span class="n num">{{ sorted.length }}</span></button>
        <button v-for="[g, c] in groups" :key="g" type="button" class="chip" :aria-pressed="group === g" @click="group = g">{{ g }}<span class="n num">{{ c }}</span></button>
      </div>
      <div class="grid">
        <NodeCard v-for="n in visible" :key="n.id" :node="n" :connected="connected" :now="now" />
      </div>
    </template>
  </section>
</template>

<style scoped>
.summary { display: flex; flex-wrap: wrap; gap: 8px 28px; align-items: baseline; padding: 12px 16px; background: var(--surface); border: 1px solid var(--line); border-radius: var(--r); margin-bottom: 16px; }
.k { color: var(--muted); font-size: 12px; margin-right: 6px; }
.k.tail { margin: 0 0 0 2px; }
.v { font-size: 16px; font-weight: 600; }
.v.bad { color: var(--bad); }
.v.warn { color: var(--warn); }
.chips { display: flex; flex-wrap: wrap; gap: 6px; margin-bottom: 16px; }
.chip { border: 1px solid var(--line); background: transparent; border-radius: 999px; padding: 3px 12px; cursor: pointer; color: var(--fg-2); font-size: 13px; transition: background 0.15s, border-color 0.15s; }
.chip:hover { border-color: var(--line-strong); }
.chip[aria-pressed="true"] { background: var(--fg); color: var(--bg); border-color: var(--fg); }
.n { color: var(--muted); margin-left: 4px; }
.chip[aria-pressed="true"] .n { color: inherit; opacity: 0.7; }
.grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(min(100%, 270px), 1fr)); gap: 12px; }
</style>
