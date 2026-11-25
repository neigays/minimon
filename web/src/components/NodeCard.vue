<script setup lang="ts">
import { computed } from 'vue'
import { formatBytes, formatRate, formatUptime } from '../format'
import type { PublicNode } from '../live'

const props = defineProps<{ node: PublicNode; live: boolean }>()

const r = computed(() => props.node.latest)
const memPct = computed(() => {
  const total = props.node.mem_total
  return r.value && total ? Math.min(100, (r.value.mem_used / total) * 100) : 0
})
const dash = '—'
</script>

<template>
  <section class="card" :class="{ off: !live }">
    <div class="head">
      <span class="dot" :class="{ on: live }"></span>
      <strong class="name">{{ node.name }}</strong>
      <span class="sub">{{ node.os ?? dash }} / {{ node.arch ?? dash }}</span>
    </div>
    <dl>
      <dt>CPU</dt>
      <dd>{{ r ? r.cpu.toFixed(1) + '%' : dash }}</dd>
      <dt>内存</dt>
      <dd>
        {{ r && node.mem_total ? `${formatBytes(r.mem_used)} / ${formatBytes(node.mem_total)}` : dash }}
        <div class="bar"><div class="fill" :style="{ width: memPct + '%' }"></div></div>
      </dd>
      <dt>磁盘</dt>
      <dd>{{ r && r.disk_total ? `${formatBytes(r.disk_used)} / ${formatBytes(r.disk_total)}` : dash }}</dd>
      <dt>网络</dt>
      <dd>{{ r ? `↓ ${formatRate(r.net_rx_rate)}  ↑ ${formatRate(r.net_tx_rate)}` : dash }}</dd>
      <dt>在线时长</dt>
      <dd>{{ r ? formatUptime(r.uptime_secs) : dash }}</dd>
    </dl>
  </section>
</template>
