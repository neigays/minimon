<script setup lang="ts">
import { computed } from 'vue'
import { useAuth } from '../auth'
import { formatRate, formatShort, formatUptime } from '../format'
import type { PublicNode } from '../live'
import { expireLevel, expireText, formatDecBytes, quotaPct, trafficBar } from '../traffic'
import { level, offlineFor, statusOf, usagePct } from '../status'
import Meter from './Meter.vue'
import ProbeRow from './ProbeRow.vue'
import StatusPill from './StatusPill.vue'
import TagList from './TagList.vue'
import { flagEmoji } from '../flag'

const { user } = useAuth()
const props = defineProps<{ node: PublicNode; connected: boolean; now: number }>()

const status = computed(() => statusOf(props.node, props.connected))
const off = computed(() => status.value === 'offline')
const r = computed(() => props.node.latest)
const sub = computed(() => {
  const n = props.node
  return [n.os, n.arch, n.cpu_cores ? `${n.cpu_cores} 核` : null].filter(Boolean).join(' · ') || '—'
})
const memTotal = computed(() => props.node.mem_total)
const diskTotal = computed(() => r.value?.disk_total || props.node.disk_total)
const tr = computed(() => (user.value ? (props.node.traffic ?? null) : null))
const days = computed(() => (user.value ? (props.node.expire_days ?? null) : null))
const offlineText = computed(() => {
  if (!props.connected) return '与服务器的连接已中断'
  const seen = props.node.last_seen_ms
  return seen ? `已离线 ${offlineFor(seen, props.now)}` : '已离线'
})
</script>

<template>
  <RouterLink class="card" :class="{ 'is-off': off }" :to="`/node/${node.id}`">
    <div class="card-head">
      <span v-if="node.region" class="flag" :title="node.region" role="img" :aria-label="`地区 ${node.region}`">{{ flagEmoji(node.region) || node.region }}</span>
      <span class="name">{{ node.name }}</span>
      <StatusPill class="pill" :status="status" />
    </div>
    <TagList v-if="node.tags?.length" class="card-tags" :tags="node.tags" />
    <div class="sub">{{ sub }}</div>
    <template v-if="r">
      <Meter label="CPU" :pct="r.cpu" :value="r.cpu.toFixed(0) + '%'" />
      <Meter label="内存" :pct="usagePct(r.mem_used, memTotal)" :value="memTotal ? `${formatShort(r.mem_used)} / ${formatShort(memTotal)}` : formatShort(r.mem_used)" />
      <Meter label="磁盘" :pct="usagePct(r.disk_used, diskTotal)" :value="diskTotal ? `${formatShort(r.disk_used)} / ${formatShort(diskTotal)}` : formatShort(r.disk_used)" />
      <ProbeRow v-if="node.probes?.length" :probes="node.probes" />
      <div v-if="tr || days !== null" class="adm">
        <template v-if="tr">
          <span class="tl">本月 <b class="num">{{ formatDecBytes(tr.used) }}</b><template v-if="tr.quota"> / {{ formatDecBytes(tr.quota) }}</template></span>
          <template v-if="tr.quota"><span class="tbar" role="meter" aria-label="本月流量" :aria-valuenow="Math.round(quotaPct(tr.used, tr.quota))" aria-valuemin="0" aria-valuemax="100"><i :class="level(quotaPct(tr.used, tr.quota))" :style="{ width: trafficBar(tr.used, tr.quota).width }"></i></span><span class="tpct num">{{ trafficBar(tr.used, tr.quota).label }}</span></template>
        </template>
        <span v-if="days !== null" class="exp" :class="expireLevel(days)">{{ expireText(days) }}</span>
      </div>
      <div v-if="off" class="foot">
        <span>{{ offlineText }}</span>
        <span>以上为{{ connected ? '离线' : '断线' }}前最后数据</span>
      </div>
      <div v-else class="foot">
        <span>↓ <b class="num">{{ formatRate(r.net_rx_rate) }}</b></span>
        <span>↑ <b class="num">{{ formatRate(r.net_tx_rate) }}</b></span>
        <span>负载 <b class="num">{{ r.load1.toFixed(2) }}</b></span>
        <span>在线 <b>{{ formatUptime(r.uptime_secs) }}</b></span>
      </div>
    </template>
    <template v-else>
      <ProbeRow v-if="node.probes?.length" :probes="node.probes" />
      <div class="foot">
      <span>暂无数据</span>
      <span v-if="off">{{ offlineText }}</span>
      </div>
    </template>
  </RouterLink>
</template>

<style scoped>
.card { background: var(--surface); border: 1px solid var(--line); border-radius: var(--r); padding: 14px 14px 12px; display: grid; gap: 10px; text-decoration: none; cursor: pointer; transition: border-color 0.2s; min-width: 0; }
.card:hover { border-color: var(--line-strong); }
.card-head { display: flex; align-items: center; gap: 8px; min-width: 0; }
.card-tags { margin-top: -2px; }
.flag { font-size: 16px; line-height: 1; flex: none; }
.region { font-family: var(--mono); font-size: 11px; font-weight: 600; padding: 1px 5px; border-radius: 4px; border: 1px solid var(--line-strong); color: var(--fg-2); flex: none; }
.name { font-weight: 600; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; min-width: 0; }
.pill { margin-left: auto; }
.sub { color: var(--muted); font-size: 12px; margin-top: -6px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.foot { display: flex; flex-wrap: wrap; gap: 4px 14px; font-size: 12px; color: var(--muted); border-top: 1px solid var(--line); padding-top: 9px; }
.foot b { color: var(--fg-2); font-weight: 500; }
.adm { display: flex; flex-wrap: wrap; align-items: center; gap: 4px 10px; font-size: 12px; color: var(--muted); }
.adm .tl b { color: var(--fg-2); font-weight: 500; }
.tbar { flex: 1; min-width: 48px; height: 6px; background: var(--surface-2); border: 1px solid var(--line); border-radius: 3px; overflow: hidden; }
.tbar i { display: block; height: 100%; background: var(--ok); border-radius: 3px; }
.tpct { font-size: 12px; color: var(--fg-2); min-width: 32px; text-align: right; }
.tbar i.hi { background: var(--warn); }
.tbar i.crit { background: var(--bad); }
.exp { margin-left: auto; }
.exp.hi { color: var(--warn); font-weight: 600; }
.exp.crit { color: var(--bad); font-weight: 600; }
.card.is-off { opacity: 0.62; }
.card.is-off :deep(.bar i) { background: var(--line-strong); }
</style>
