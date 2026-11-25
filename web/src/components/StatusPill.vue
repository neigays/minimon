<script setup lang="ts">
import { computed } from 'vue'
import type { Status } from '../status'

const props = defineProps<{ status: Status }>()
const text = computed(() => ({ online: '在线', busy: '负载高', offline: '离线' })[props.status])
const cls = computed(() => ({ online: 'on', busy: 'warn', offline: 'off' })[props.status])
</script>

<template>
  <span class="status" :class="cls">
    <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" aria-hidden="true">
      <path v-if="status === 'online'" d="M5 12l5 5L20 7" />
      <path v-else-if="status === 'offline'" d="M6 6l12 12M18 6L6 18" />
      <path v-else d="M12 7v6M12 17h.01" />
    </svg>
    {{ text }}
  </span>
</template>

<style scoped>
.status { display: inline-flex; align-items: center; gap: 4px; font-size: 12px; padding: 1px 8px 1px 6px; border-radius: 999px; flex: none; }
.on { color: var(--ok); background: var(--ok-soft); }
.off { color: var(--bad); background: var(--bad-soft); }
.warn { color: var(--warn); background: var(--warn-soft); }
</style>
