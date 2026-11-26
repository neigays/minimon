<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { flagUrl } from '../flag'

// 内置 SVG 国旗（各系统显示一致；Windows 不渲染国旗 emoji）。没有对应图片时显示字母代码。
const props = defineProps<{ code: string; size?: number }>()
const failed = ref(false)
watch(() => props.code, () => (failed.value = false))
const url = computed(() => flagUrl(props.code))
const h = computed(() => props.size ?? 12)
</script>

<template>
  <img v-if="url && !failed" class="flag-img" :src="url" :height="h" :width="Math.round((h * 4) / 3)" :alt="`地区 ${code}`" :title="code" loading="lazy" @error="failed = true" />
  <span v-else class="flag-code" :title="code">{{ code }}</span>
</template>

<style scoped>
.flag-img { display: inline-block; vertical-align: middle; border-radius: 2px; box-shadow: 0 0 0 1px var(--line); flex: none; }
.flag-code { font-family: var(--mono); font-size: 11px; font-weight: 600; padding: 0 4px; border-radius: 4px; border: 1px solid var(--line-strong); color: var(--fg-2); flex: none; }
</style>
