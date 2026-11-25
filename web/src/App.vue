<script setup lang="ts">
import NodeCard from './components/NodeCard.vue'
import { useLive } from './live'

const { nodes, connected } = useLive()
</script>

<template>
  <header class="top">
    <h1>minimon</h1>
    <span class="conn" :class="{ ok: connected }">{{ connected ? '已连接' : '连接中断，重连中…' }}</span>
  </header>
  <main>
    <p v-if="nodes.length === 0" class="empty">暂无节点</p>
    <div v-else class="grid">
      <NodeCard v-for="n in nodes" :key="n.id" :node="n" />
    </div>
  </main>
</template>
