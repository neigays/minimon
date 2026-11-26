<script setup lang="ts">
import { computed } from 'vue'
import { level } from '../status'

const props = defineProps<{ label: string; pct: number; value: string }>()
const p = computed(() => Math.max(0, Math.min(100, props.pct)))
</script>

<template>
  <div class="meter">
    <span class="lbl">{{ label }}</span>
    <span class="bar" role="meter" :aria-label="label" :aria-valuenow="Math.round(p)" aria-valuemin="0" aria-valuemax="100">
      <i :class="level(p)" :style="{ width: p.toFixed(1) + '%' }"></i>
    </span>
    <span class="val num">{{ value }}</span>
  </div>
</template>

<style scoped>
.meter { display: grid; grid-template-columns: 34px 1fr auto; align-items: center; gap: 8px; font-size: 12px; }
.lbl { color: var(--muted); }
.bar { height: 6px; background: var(--surface-2); border-radius: 3px; overflow: hidden; border: 1px solid var(--line); }
.bar i { display: block; height: 100%; background: var(--ok); border-radius: 3px; }
.bar i.hi { background: var(--warn); }
.bar i.crit { background: var(--bad); }
.val { min-width: 96px; text-align: right; color: var(--fg-2); }
@media (max-width: 360px) { .val { min-width: 80px; } }
</style>
