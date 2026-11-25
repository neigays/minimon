import { api } from './api'
import { medianStep, withGaps, type ChartSpec, type Range } from './history'
import type { PublicNode } from './live'

export type Level = 'ok' | 'hi' | 'crit' | 'none'

/** avg < 100ms 绿；100–200ms 黄；> 200ms 或丢包 > 20% 红。 */
export function latencyLevel(avg: number | null, loss: number | null): Level {
  if (loss !== null && loss > 0.2) return 'crit'
  if (avg === null) return 'none'
  if (avg > 200) return 'crit'
  return avg >= 100 ? 'hi' : 'ok'
}

export interface ProbeSummary {
  task_id: number
  name: string
  avg_ms: number | null
  loss: number | null
  error: string | null
}

export type ProbeMsg = { type: 'probe'; node_id: number } & ProbeSummary

export function applyProbeMsg(nodes: PublicNode[], m: ProbeMsg): PublicNode[] {
  return nodes.map((n) => {
    if (n.id !== m.node_id) return n
    const next: ProbeSummary = { task_id: m.task_id, name: m.name, avg_ms: m.avg_ms, loss: m.loss, error: m.error }
    const has = n.probes.some((p) => p.task_id === m.task_id)
    return { ...n, probes: has ? n.probes.map((p) => (p.task_id === m.task_id ? next : p)) : [...n.probes, next] }
  })
}

export interface ProbeHistory {
  granularity: 'raw' | '5m'
  tasks: { task_id: number; name: string; ts: number[]; avg_ms: (number | null)[]; loss: (number | null)[] }[]
}

export function fetchProbeHistory(id: number | string, range: Range): Promise<ProbeHistory> {
  return api<ProbeHistory>('GET', `/api/nodes/${id}/probes?range=${range}`)
}

const COLORS = ['var(--c1)', 'var(--c2)', 'var(--c3)', 'var(--c4)']

/**
 * 把各任务（时间轴互不相同）合并到同一条时间轴。
 * 每个任务自己的缺口用 null 断开；其它任务独有的时间点对它是 undefined（uPlot 连线跨过）。
 */
export function buildProbeCharts(h: ProbeHistory): { ts: number[]; charts: ChartSpec[] } {
  const step = h.granularity === '5m' ? 300_000 : Math.max(0, ...h.tasks.map((t) => medianStep(t.ts)))
  const per = h.tasks.map((t) => {
    const loss = t.loss.map((v) => (v === null ? null : v * 100))
    return withGaps(t.ts, [t.avg_ms, loss], step || 3000)
  })
  const ts = [...new Set(per.flatMap((p) => p.ts))].sort((a, b) => a - b)
  const col = (k: number, c: 0 | 1) =>
    per[k].ts.length === 0
      ? ts.map(() => undefined)
      : (() => {
          const m = new Map(per[k].ts.map((t, i) => [t, per[k].cols[c][i]]))
          return ts.map((t) => (m.has(t) ? (m.get(t) as number | null) : undefined))
        })()
  const series = (c: 0 | 1) =>
    h.tasks.map((t, k) => ({ name: t.name, values: col(k, c) as (number | null)[], color: COLORS[k % COLORS.length] }))
  return {
    ts,
    charts: [
      { key: 'latency', title: '三网延迟', unit: 'ms', series: series(0) },
      { key: 'loss', title: '丢包率', unit: 'pct', max: 100, series: series(1) },
    ],
  }
}
