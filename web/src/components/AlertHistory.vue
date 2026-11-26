<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { api, ApiError } from '../api'
import { formatEventTime, SENT_LABELS, type AlertEvent } from '../alert'

const props = defineProps<{ tz: string | null }>()
const PAGE = 50
const events = ref<AlertEvent[]>([])
const more = ref(false)
const loaded = ref(false)
const loading = ref(false)
const err = ref('')

async function load(reset: boolean) {
  if (loading.value) return
  loading.value = true
  err.value = ''
  try {
    const before = !reset && events.value.length ? `&before=${events.value[events.value.length - 1].id}` : ''
    const page = await api<AlertEvent[]>('GET', `/api/admin/alert-events?limit=${PAGE}${before}`)
    events.value = reset ? page : [...events.value, ...page]
    more.value = page.length === PAGE
    loaded.value = true
  } catch (e) {
    err.value = e instanceof ApiError ? e.message : '加载失败'
  } finally {
    loading.value = false
  }
}
onMounted(() => load(true))
defineExpose({ reload: () => load(true) })
</script>

<template>
  <div>
    <div v-if="err" class="err" role="alert">{{ err }}</div>
    <ul class="events">
      <li v-for="e in events" :key="e.id">
        <span class="num time">{{ formatEventTime(e.ts_ms, props.tz) }}</span>
        <span class="title">{{ e.title }}</span>
        <span class="badge" :class="e.sent">{{ SENT_LABELS[e.sent] }}</span>
        <span v-if="e.body" class="body muted">{{ e.body }}</span>
      </li>
      <li v-if="loaded && events.length === 0" class="empty">暂无告警记录</li>
    </ul>
    <div v-if="more" class="more"><button class="btn ghost" type="button" :disabled="loading" @click="load(false)">{{ loading ? '加载中…' : '加载更多' }}</button></div>
  </div>
</template>

<style scoped>
.events { list-style: none; margin: 0; padding: 0; background: var(--surface); border: 1px solid var(--line); border-radius: var(--r); }
.events li { display: flex; gap: 10px; align-items: baseline; flex-wrap: wrap; padding: 9px 12px; border-bottom: 1px solid var(--line); }
.events li:last-child { border-bottom: 0; }
.time { color: var(--muted); font-size: 12px; }
.title { font-weight: 550; }
.body { flex-basis: 100%; font-size: 12px; white-space: pre-wrap; overflow-wrap: anywhere; }
.badge { font-size: 12px; padding: 1px 8px; border-radius: 999px; background: var(--surface-2); color: var(--muted); }
.badge.ok { color: var(--ok); }
.badge.failed { color: var(--bad); background: var(--bad-soft); }
.empty { justify-content: center; padding: 24px 0 !important; }
.more { margin-top: 10px; text-align: center; }
</style>
