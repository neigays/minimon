<script setup lang="ts">
import uPlot from 'uplot'
import 'uplot/dist/uPlot.min.css'
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { chartShape, formatValue, type ChartSeries, type Unit } from '../history'

const props = defineProps<{ title: string; series: ChartSeries[]; ts: number[]; unit: Unit; max?: number }>()

const PLOT_H = 160
const root = ref<HTMLDivElement>()
const tip = ref<HTMLDivElement>()
const tipText = ref('')
let plot: uPlot | null = null
let shape = ''
let ro: ResizeObserver | null = null
let mo: MutationObserver | null = null
let mq: MediaQueryList | null = null

/** 把 `var(--c1)` 解析为当前主题下的实际颜色（canvas 不认 CSS 变量）。 */
function cssColor(v: string): string {
  const m = /^var\((--[\w-]+)\)$/.exec(v)
  if (!m) return v
  return getComputedStyle(document.documentElement).getPropertyValue(m[1]).trim() || v
}

const latest = computed(() =>
  props.series.map((s) => {
    for (let i = s.values.length - 1; i >= 0; i--) if (s.values[i] != null) return s.values[i]
    return null
  }),
)
const nowText = computed(() =>
  props.series.map((s, i) => (props.series.length > 1 ? `${s.name} ` : '当前 ') + formatValue(props.unit, latest.value[i])).join(' · '),
)

function tickLabel(sec: number, longSpan: boolean): string {
  const d = new Date(sec * 1000)
  const p = (n: number) => String(n).padStart(2, '0')
  return longSpan ? `${d.getMonth() + 1}/${d.getDate()}` : `${p(d.getHours())}:${p(d.getMinutes())}`
}

/** 任意 CSS 颜色（hex、rgb、命名色）转为带透明度的 rgba，借 canvas 解析。 */
function withAlpha(color: string, a: number): string {
  const ctx = document.createElement('canvas').getContext('2d')
  if (!ctx) return color
  ctx.fillStyle = '#000'
  ctx.fillStyle = color
  const v = ctx.fillStyle // 不透明为 #rrggbb，带透明度为 rgba(...)
  if (/^#[0-9a-f]{6}$/i.test(v)) {
    const n = parseInt(v.slice(1), 16)
    return `rgba(${n >> 16}, ${(n >> 8) & 255}, ${n & 255}, ${a})`
  }
  return v
}

function timeLabel(sec: number): string {
  const d = new Date(sec * 1000)
  const p = (n: number) => String(n).padStart(2, '0')
  return `${d.getMonth() + 1}/${d.getDate()} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`
}

function create() {
  const el = root.value
  if (!el) return
  plot?.destroy()
  plot = null
  const muted = cssColor('var(--muted)')
  const grid = cssColor('var(--grid)')
  const axis = {
    stroke: muted,
    grid: { stroke: grid, width: 1 },
    ticks: { show: false },
    font: '10px ui-monospace, Menlo, Consolas, monospace',
  }
  const unit = props.unit
  const longSpan = props.ts.length > 1 && props.ts[props.ts.length - 1] - props.ts[0] > 2 * 86400_000
  const max = props.max
  const opts: uPlot.Options = {
    width: Math.max(120, el.clientWidth),
    height: PLOT_H,
    legend: { show: false },
    padding: [8, 14, 0, 0],
    cursor: { points: { size: 6 }, drag: { x: false, y: false } },
    scales: {
      x: { time: true },
      y: {
        range: (_u, _min, dmax) => [0, max ?? Math.max(dmax * 1.15, unit === 'count' ? 4 : 1)],
      },
    },
    axes: [
      { ...axis, size: 24, space: 80, values: (_u, vals) => vals.map((v) => tickLabel(v, longSpan)) },
      { ...axis, size: unit === 'rate' ? 84 : 58, values: (_u, vals) => vals.map((v) => formatValue(unit, v)) },
    ],
    series: [
      {},
      ...props.series.map((s) => {
        const color = cssColor(s.color)
        return {
          label: s.name,
          stroke: color,
          width: 1.6,
          dash: s.dashed ? [4, 3] : undefined,
          fill: props.series.length === 1 ? withAlpha(color, 0.12) : undefined,
          points: { show: false },
          spanGaps: false,
        } as uPlot.Series
      }),
    ],
    hooks: {
      setCursor: [
        (u) => {
          const i = u.cursor.idx
          const t = tip.value
          if (!t) return
          if (i === null || i === undefined || u.cursor.left === undefined || u.cursor.left < 0) {
            t.hidden = true
            return
          }
          const parts = props.series.map((s) => (props.series.length > 1 ? `${s.name} ` : '') + formatValue(unit, s.values[i]))
          tipText.value = `${timeLabel(u.data[0][i])}  ${parts.join('  ')}`
          t.hidden = false
          const x = u.cursor.left + u.over.offsetLeft + (root.value?.offsetLeft ?? 0)
          const w = t.offsetWidth
          const box = root.value?.clientWidth ?? 0
          t.style.left = `${Math.max(w / 2, Math.min(box - w / 2, x))}px`
          t.style.top = `${u.over.offsetTop + (root.value?.offsetTop ?? 0) + 4}px`
        },
      ],
    },
  }
  const data = [props.ts.map((t) => t / 1000), ...props.series.map((s) => s.values)] as uPlot.AlignedData
  plot = new uPlot(opts, data, el)
  shape = chartShape(props.unit, props.max, props.series)
}

function dataArr(): uPlot.AlignedData {
  return [props.ts.map((t) => t / 1000), ...props.series.map((s) => s.values)] as uPlot.AlignedData
}

/** 只有数据变化时 setData，形状（单位/上限/颜色/序列）变化才重建。 */
function refresh() {
  if (plot && shape === chartShape(props.unit, props.max, props.series)) plot.setData(dataArr())
  else create()
}

function onTheme() {
  create()
}

onMounted(() => {
  create()
  ro = new ResizeObserver(() => {
    const el = root.value
    if (plot && el && el.clientWidth > 0) plot.setSize({ width: el.clientWidth, height: PLOT_H })
  })
  if (root.value) ro.observe(root.value)
  mo = new MutationObserver(onTheme)
  mo.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] })
  mq = window.matchMedia('(prefers-color-scheme: light)')
  mq.addEventListener('change', onTheme)
})
onBeforeUnmount(() => {
  ro?.disconnect()
  mo?.disconnect()
  mq?.removeEventListener('change', onTheme)
  plot?.destroy()
  plot = null
})
watch(() => [props.ts, props.series, props.unit, props.max], refresh)
</script>

<template>
  <div class="chart">
    <h3>
      {{ title }}
      <span v-if="series.length > 1" class="legend">
        <span v-for="s in series" :key="s.name"><i :style="{ background: s.color, opacity: s.dashed ? 0.7 : 1 }"></i>{{ s.name }}</span>
      </span>
      <span class="now num">{{ nowText }}</span>
    </h3>
    <div ref="root" class="plot" role="img" :aria-label="`${title}历史曲线，${nowText}`"></div>
    <div ref="tip" class="tip num" hidden>{{ tipText }}</div>
  </div>
</template>

<style scoped>
.chart { background: var(--surface); border: 1px solid var(--line); box-shadow: var(--shadow); border-radius: var(--r); padding: 12px 12px 6px; min-width: 0; position: relative; }
h3 { margin: 0 0 2px; font-size: 13px; font-weight: 600; display: flex; align-items: baseline; gap: 10px; flex-wrap: wrap; }
.now { margin-left: auto; font-weight: 500; color: var(--fg-2); font-size: 12px; }
.legend { display: inline-flex; gap: 10px; font-size: 12px; color: var(--muted); font-weight: 400; }
.legend i { display: inline-block; width: 10px; height: 2px; vertical-align: middle; margin-right: 4px; }
.plot { width: 100%; min-height: 160px; }
.tip { position: absolute; pointer-events: none; background: var(--fg); color: var(--bg); font-size: 12px; padding: 4px 8px; border-radius: 6px; white-space: nowrap; transform: translate(-50%, 0); z-index: 2; }
.tip[hidden] { display: none; }
</style>
