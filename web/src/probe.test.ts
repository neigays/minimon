import { expect, test } from 'vitest'
import { applyProbeMsg, buildProbeCharts, latencyLevel, type ProbeHistory } from './probe'
import type { PublicNode } from './live'

test('latencyLevel', () => {
  expect(latencyLevel(99, 0)).toBe('ok')
  expect(latencyLevel(100, 0)).toBe('hi')
  expect(latencyLevel(200, 0)).toBe('hi')
  expect(latencyLevel(201, 0)).toBe('crit')
  expect(latencyLevel(30, 0.25)).toBe('crit')
  expect(latencyLevel(30, 0.2)).toBe('ok')
  expect(latencyLevel(null, null)).toBe('none')
  expect(latencyLevel(null, 1)).toBe('crit')
})

test('buildProbeCharts merges tasks on union timeline, loss in percent, gaps', () => {
  const h: ProbeHistory = {
    granularity: '5m',
    tasks: [
      { task_id: 1, name: '电信', ts: [0, 300_000, 1_500_000], avg_ms: [10, null, 30], loss: [0, 1, 0.5] },
      { task_id: 2, name: '联通', ts: [300_000, 600_000], avg_ms: [20, 25], loss: [0, 0] },
    ],
  }
  const b = buildProbeCharts(h)
  // 每个任务各自断开（任务自己的缺口插入 null），其它任务的时间点对它是 undefined（连线跨过，不断开）
  expect(b.ts).toEqual([0, 300_000, 600_000, 900_000, 1_500_000])
  const lat = b.charts[0]
  expect(lat.unit).toBe('ms')
  expect(lat.series.map((s) => s.name)).toEqual(['电信', '联通'])
  expect(lat.series[0].values).toEqual([10, null, undefined, null, 30])
  expect(lat.series[1].values).toEqual([undefined, 20, 25, undefined, undefined])
  const loss = b.charts[1]
  expect(loss.unit).toBe('pct')
  expect(loss.series[0].values).toEqual([0, 100, undefined, null, 50])
})

const node = (id: number): PublicNode =>
  ({ id, name: 'n', online: true, probes: [{ task_id: 1, name: '电信', avg_ms: 10, loss: 0, error: null }] }) as unknown as PublicNode

test('applyProbeMsg updates or appends', () => {
  const m = { type: 'probe' as const, node_id: 1, task_id: 1, name: '电信', avg_ms: 50, loss: 0.1, error: null }
  const r = applyProbeMsg([node(1), node(2)], m)
  expect(r[0].probes[0].avg_ms).toBe(50)
  expect(r[1].probes[0].avg_ms).toBe(10)
  const r2 = applyProbeMsg([node(1)], { ...m, task_id: 2, name: '联通' })
  expect(r2[0].probes.map((p) => p.task_id)).toEqual([1, 2])
  expect(applyProbeMsg([node(1)], { ...m, node_id: 9 })[0].probes[0].avg_ms).toBe(10)
})
