<script setup lang="ts">
import { RouterLink, RouterView, useRoute } from 'vue-router'

const route = useRoute()
const items: { to?: string; label: string; hint?: string }[] = [
  { to: '/admin', label: '节点管理' },
  { label: '探测任务', hint: 'M3' },
  { label: '告警规则', hint: 'M4' },
  { label: '通知设置', hint: 'M4' },
  { to: '/admin/settings', label: '系统设置' },
]
</script>

<template>
  <div class="admin-grid">
    <nav class="side" aria-label="后台菜单">
      <template v-for="i in items" :key="i.label">
        <RouterLink v-if="i.to" :to="i.to" :aria-current="route.path === i.to ? 'page' : undefined">{{ i.label }}</RouterLink>
        <button v-else type="button" disabled :title="`${i.hint} 里程碑提供`">{{ i.label }}（{{ i.hint }}）</button>
      </template>
    </nav>
    <div class="content"><RouterView /></div>
  </div>
</template>

<style scoped>
.admin-grid { display: grid; grid-template-columns: 180px minmax(0, 1fr); gap: 20px; }
.side { display: grid; gap: 2px; align-content: start; }
.side a, .side button { text-align: left; background: none; border: 0; padding: 7px 10px; border-radius: 6px; color: var(--muted); text-decoration: none; cursor: pointer; white-space: nowrap; }
.side button:disabled { cursor: not-allowed; opacity: 0.6; }
.side a:hover { color: var(--fg); }
.side a[aria-current="page"] { background: var(--surface); color: var(--fg); box-shadow: inset 0 0 0 1px var(--line); }
.content { min-width: 0; }
@media (max-width: 720px) {
  .admin-grid { grid-template-columns: minmax(0, 1fr); }
  .side { grid-auto-flow: column; overflow-x: auto; justify-content: start; }
}
</style>
