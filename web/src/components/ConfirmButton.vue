<script setup lang="ts">
import { onBeforeUnmount, ref } from 'vue'

// 页面内二次确认：第一次点击进入「确认」状态（约 4 秒后自动还原），第二次点击才执行。
const props = defineProps<{ label: string; confirmLabel: string; danger?: boolean; disabled?: boolean }>()
const emit = defineEmits<{ confirm: [] }>()
const armed = ref(false)
let timer: ReturnType<typeof setTimeout> | undefined

function click() {
  if (props.disabled) return
  if (armed.value) {
    clearTimeout(timer)
    armed.value = false
    emit('confirm')
    return
  }
  armed.value = true
  timer = setTimeout(() => (armed.value = false), 4000)
}
onBeforeUnmount(() => clearTimeout(timer))
</script>

<template>
  <button type="button" class="act" :class="{ danger: danger || armed, armed }" :disabled="disabled" @click="click" @blur="armed = false">
    {{ armed ? confirmLabel : label }}
  </button>
</template>

<style scoped>
.act { color: var(--accent); background: none; border: 0; cursor: pointer; padding: 2px 6px; border-radius: 6px; }
.act.danger { color: var(--bad); }
.act.armed { background: var(--bad-soft); font-weight: 600; }
.act:disabled { opacity: 0.5; cursor: default; }
</style>
