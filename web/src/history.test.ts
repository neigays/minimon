import { describe, expect, test } from 'vitest'
import { buildCharts, chartShape, formatValue, historyError, medianStep, pctOf, stepFor, withGaps, type History } from './history'

describe('withGaps', () => {
  test('inserts null on gap', () => {
    const r = withGaps([0, 3000, 6000, 30000, 33000], [[1, 2, 3, 4, 5]], 3000)
    expect(r.ts).toEqual([0, 3000, 6000, 18000, 30000, 33000])
    expect(r.cols[0]).toEqual([1, 2, 3, null, 4, 5])
  })
  test('no gap no change', () => {
    const r = withGaps([0, 3000, 6000, 9000], [[1, 2, 3, 4], [4, 3, 2, 1]], 3000)
    expect(r.ts).toEqual([0, 3000, 6000, 9000])
    expect(r.cols).toEqual([[1, 2, 3, 4], [4, 3, 2, 1]])
  })
  test('exactly 2.5x step is not a gap, just over is', () => {
    expect(withGaps([0, 7500], [[1, 2]], 3000).ts).toEqual([0, 7500])
    expect(withGaps([0, 7501], [[1, 2]], 3000).ts.length).toBe(3)
  })
  test('empty and single input', () => {
    expect(withGaps([], [[]], 3000)).toEqual({ ts: [], cols: [[]] })
    expect(withGaps([5], [[1]], 3000)).toEqual({ ts: [5], cols: [[1]] })
  })
})

describe('step', () => {
  test('medianStep uses median of diffs, ignoring outlier gaps', () => {
    expect(medianStep([0, 3000, 6000, 9000, 60000, 63000])).toBe(3000)
    expect(medianStep([1])).toBe(3000)
  })
  test('stepFor by granularity', () => {
    expect(stepFor('1m', [])).toBe(60_000)
    expect(stepFor('15m', [])).toBe(900_000)
    expect(stepFor('raw', [0, 5000, 10000])).toBe(5000)
  })
})

describe('pctOf', () => {
  test('clamps and nulls', () => {
    expect(pctOf(50, 200)).toBe(25)
    expect(pctOf(300, 200)).toBe(100)
    expect(pctOf(1, 0)).toBeNull()
    expect(pctOf(1, null)).toBeNull()
  })
})

describe('historyError', () => {
  test('404 maps to not found', () => {
    expect(historyError(404, '节点不存在或未公开')).toEqual({ kind: 'notfound', text: '节点不存在或未公开' })
    expect(historyError(404, 'whatever').text).toBe('节点不存在或未公开')
  })
  test('other errors keep server message', () => {
    expect(historyError(500, '请求失败（500）')).toEqual({ kind: 'error', text: '请求失败（500）' })
    expect(historyError(0, '无法连接服务器').kind).toBe('error')
    expect(historyError(401, '需要登录').kind).toBe('error')
  })
})

describe('formatValue', () => {
  test('units', () => {
    expect(formatValue('pct', 12.34)).toBe('12.3%')
    expect(formatValue('rate', 2048)).toBe('2.0 KiB/s')
    expect(formatValue('num', 0.456)).toBe('0.46')
    expect(formatValue('count', 12.4)).toBe('12')
    expect(formatValue('pct', null)).toBe('—')
  })
})

const H: History = {
  granularity: 'raw',
  ts: [0, 3000, 6000],
  cpu: [10, 20, 30],
  mem_used: [50, 100, 150],
  swap_used: [0, 0, 0],
  disk_used: [10, 20, 30],
  disk_total: [100, 100, 0],
  net_rx_rate: [1, 2, 3],
  net_tx_rate: [4, 5, 6],
  load1: [0.1, 0.2, 0.3],
  load5: [0, 0, 0],
  load15: [0, 0, 0],
  tcp_count: [7, 8, 9],
  udp_count: [1, 2, 3],
}

describe('buildCharts', () => {
  test('six charts with derived percentages', () => {
    const { ts, charts } = buildCharts(H, 200)
    expect(ts).toEqual([0, 3000, 6000])
    expect(charts.map((c) => c.title)).toEqual(['CPU', '内存', '网络', '负载（1 分钟）', '磁盘', '连接数'])
    expect(charts[1].series[0].values).toEqual([25, 50, 75])
    expect(charts[4].series[0].values).toEqual([10, 20, null])
    expect(charts[2].series.map((s) => s.name)).toEqual(['↓ 下行', '↑ 上行'])
    expect(charts[5].series[1].dashed).toBe(true)
  })
  test('gaps are applied to every column', () => {
    const { ts, charts } = buildCharts({ ...H, ts: [0, 3000, 6000, 9000, 60000], cpu: [1, 2, 3, 4, 5], mem_used: [1, 1, 1, 1, 1], disk_used: [1, 1, 1, 1, 1], disk_total: [9, 9, 9, 9, 9], net_rx_rate: [1, 1, 1, 1, 1], net_tx_rate: [1, 1, 1, 1, 1], load1: [1, 1, 1, 1, 1], tcp_count: [1, 1, 1, 1, 1], udp_count: [1, 1, 1, 1, 1] }, 200)
    expect(ts.length).toBe(6)
    expect(charts[0].series[0].values[4]).toBeNull()
    expect(charts[5].series[1].values[4]).toBeNull()
  })
})

describe('chartShape', () => {
  const a = [{ name: 'CPU', values: [1, 2], color: 'var(--c1)' }]
  test('same shape when only values change', () => {
    expect(chartShape('pct', 100, a)).toBe(chartShape('pct', 100, [{ ...a[0], values: [9, 9, 9] }]))
  })
  test('differs on unit, max, color, dashed or names', () => {
    const base = chartShape('pct', 100, a)
    expect(chartShape('num', 100, a)).not.toBe(base)
    expect(chartShape('pct', undefined, a)).not.toBe(base)
    expect(chartShape('pct', 100, [{ ...a[0], color: 'var(--c2)' }])).not.toBe(base)
    expect(chartShape('pct', 100, [{ ...a[0], dashed: true }])).not.toBe(base)
    expect(chartShape('pct', 100, [{ ...a[0], name: 'x' }])).not.toBe(base)
  })
})
