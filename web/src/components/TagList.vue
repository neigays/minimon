<script setup lang="ts">
import type { Tag } from '../tags'

defineProps<{ tags: Tag[] }>()
</script>

<template>
  <span v-if="tags.length" class="tags">
    <span v-for="(t, i) in tags" :key="i" class="tag" :data-color="t.color">{{ t.text }}</span>
  </span>
</template>

<style scoped>
.tags { display: inline-flex; flex-wrap: wrap; gap: 4px; min-width: 0; }
.tag { font-size: 11px; font-weight: 600; line-height: 18px; padding: 0 6px; border-radius: 4px; white-space: nowrap; }
</style>
