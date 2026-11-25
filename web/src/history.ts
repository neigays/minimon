import { api, ApiError } from './api'
import { formatRate } from './format'

export type Range = '1h' | '24h' | '7d' | '30d'
export type Granularity = 'raw' | '1m' | '15m'

export const RANGES: { key: Range; label: string }[] = [
  { key: '1h', label: '1 小时' },
  { key: '24h', label: '24 小时' },
  { key: '7d', label: '7 天' },
  { key: '30d', label: '30 天' },
]

export interface History {
  granularity: Granularity
  ts: number[]
  cpu: number[]
  mem_used: number[]
  swap_used: number[]
  disk_used: number[]
  disk_total: number[]
  net_rx_rate: number[]
  net_tx_rate: number[]
  load1: number[]
  load5: number[]
  load15: number[]
  tcp_count: number[]
  udp_count: number[]
}

export function fetchHistory(id: number | string, range: Range): Promise<History> {
  return api<History>('GET', `/api/nodes/${id}/history?range=${range}`)
}

export function granularityNote(g: Granularity): string {
  return g === 'raw' ? '原始数据（约每 3 秒一点）' : g === '1m' ? '1 分钟聚合' : '15 分钟聚合'
}

export type Unit = 'pct' | 'rate' | 'num' | 'count'

export function formatValue(unit: Unit, v: number | null | undefined): string {
  if (v === null || v === undefined || !Number.isFinite(v)) return '—'
  switch (unit) {
    case 'pct': return `${v.toFixed(1)}%`
    case 'rate': return formatRate(v)
    case 'num': return v.toFixed(2)
    case 'count': return String(Math.round(v))
  }
}

export function pctOf(used: number, total: number | null | undefined): number | null {
  if (!total || total <= 0) return null
  return Math.max(0, Math.min(100, (used / total) * 100))
}

export function medianStep(ts: number[]): number {
  const d: number[] = []
  for (let i = 1; i < ts.length; i++) d.push(ts[i] - ts[i - 1])
  if (d.length === 0) return 3000
  d.sort((a, b) => a - b)
  return d[Math.floor(d.length / 2)] || 3000
}

export function stepFor(g: Granularity, ts: number[]): number {
  return g === '1m' ? 60_000 : g === '15m' ? 900_000 : medianStep(ts)
}

/** 相邻点间隔超过 2.5 倍步长时，在中点插入一个 null 点，使曲线在缺口处断开。 */
export function withGaps(
  ts: number[],
  cols: (number | null)[][],
  stepMs: number,
): { ts: number[]; cols: (number | null)[][] } {
  const outTs: number[] = []
  const out: (number | null)[][] = cols.map(() => [])
  for (let i = 0; i < ts.length; i++) {
    if (i > 0 && ts[i] - ts[i - 1] > stepMs * 2.5) {
      outTs.push(Math.round((ts[i] + ts[i - 1]) / 2))
      out.forEach((c) => c.push(null))
    }
    outTs.push(ts[i])
    out.forEach((c, k) => c.push(cols[k][i] ?? null))
  }
  return { ts: outTs, cols: out }
}

export interface ChartSeries { name: string; values: (number | null)[]; color: string; dashed?: boolean }
export interface ChartSpec { key: string; title: string; unit: Unit; max?: number; series: ChartSeries[] }

const C1 = 'var(--c1)'
const C2 = 'var(--c2)'

export function buildCharts(h: History, memTotal: number | null | undefined): { ts: number[]; charts: ChartSpec[] } {
  const cols: (number | null)[][] = [
    h.cpu,
    h.mem_used.map((v) => pctOf(v, memTotal)),
    h.net_rx_rate,
    h.net_tx_rate,
    h.load1,
    h.disk_used.map((v, i) => pctOf(v, h.disk_total[i])),
    h.tcp_count,
    h.udp_count,
  ]
  const g = withGaps(h.ts, cols, stepFor(h.granularity, h.ts))
  const [cpu, mem, rx, tx, load, disk, tcp, udp] = g.cols
  return {
    ts: g.ts,
    charts: [
      { key: 'cpu', title: 'CPU', unit: 'pct', max: 100, series: [{ name: 'CPU', values: cpu, color: C1 }] },
      { key: 'mem', title: '内存', unit: 'pct', max: 100, series: [{ name: '内存', values: mem, color: C2 }] },
      { key: 'net', title: '网络', unit: 'rate', series: [
        { name: '↓ 下行', values: rx, color: C1 },
        { name: '↑ 上行', values: tx, color: C2 },
      ] },
      { key: 'load', title: '负载（1 分钟）', unit: 'num', series: [{ name: '1 分钟', values: load, color: C1 }] },
      { key: 'disk', title: '磁盘', unit: 'pct', max: 100, series: [{ name: '磁盘', values: disk, color: C2 }] },
      { key: 'conn', title: '连接数', unit: 'count', series: [
        { name: 'TCP', values: tcp, color: C1 },
        { name: 'UDP', values: udp, color: C2, dashed: true },
      ] },
    ],
  }
}

export interface HistoryErr { kind: 'notfound' | 'error'; text: string }

/** 把历史接口的失败映射为界面状态：404 显示固定文案，其余沿用服务端/网络层消息。 */
export function historyError(status: number, message: string): HistoryErr {
  if (status === 404) return { kind: 'notfound', text: '节点不存在或未公开' }
  return { kind: 'error', text: message }
}

export function toHistoryErr(e: unknown): HistoryErr {
  if (e instanceof ApiError) return historyError(e.status, e.message)
  return { kind: 'error', text: '加载失败' }
}
