<script setup lang="ts">
import { latencyLevel, type ProbeSummary } from '../probe'

defineProps<{ probes: ProbeSummary[] }>()

function text(p: ProbeSummary): string {
  if (p.error) return '—'
  if (p.avg_ms === null) return p.loss !== null && p.loss >= 1 ? '丢包' : '—'
  const v = p.avg_ms < 10 ? p.avg_ms.toFixed(1) : String(Math.round(p.avg_ms))
  return `${v}ms`
}
function title(p: ProbeSummary): string {
  if (p.error) return `${p.name}：${p.error}`
  return p.loss !== null && p.loss > 0 ? `${p.name}：丢包 ${(p.loss * 100).toFixed(0)}%` : p.name
}
</script>

<template>
  <div class="probes" aria-label="延迟探测">
    <span v-for="p in probes" :key="p.task_id" class="p" :title="title(p)">
      <span class="pn">{{ p.name }}</span>
      <b class="num" :class="p.error ? 'none' : latencyLevel(p.avg_ms, p.loss)">{{ text(p) }}</b>
    </span>
  </div>
</template>

<style scoped>
.probes { display: flex; flex-wrap: wrap; gap: 2px 12px; font-size: 12px; min-width: 0; }
.p { display: inline-flex; gap: 5px; align-items: baseline; min-width: 0; }
.pn { color: var(--muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 6em; }
b { font-weight: 600; }
b.ok { color: var(--ok); }
b.hi { color: var(--warn); }
b.crit { color: var(--bad); }
b.none { color: var(--muted); font-weight: 500; }
</style>
