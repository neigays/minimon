import { expect, test } from 'vitest'
import { formatDuration, ruleSummary, toDisplay, toStored, type AlertRule } from './alert'

const rule = (o: Partial<AlertRule> = {}): AlertRule => ({
  id: 1, name: 'r', enabled: true, metric: 'cpu', op: 'gt', threshold: 90, duration_secs: 300,
  cooldown_secs: 1800, all_nodes: true, node_ids: [], probe_task_id: null, ...o,
})

test('Mbps converts to bytes per second and back', () => {
  expect(toStored('net_rx_rate', 100)).toBe(12_500_000)
  expect(toDisplay('net_rx_rate', 12_500_000)).toBe(100)
  expect(toStored('cpu', 90)).toBe(90)
  expect(toDisplay('probe_latency', 200)).toBe(200)
})
test('ruleSummary formats metric, threshold and duration', () => {
  expect(ruleSummary(rule())).toBe('CPU > 90% 持续 5 分钟')
  expect(ruleSummary(rule({ metric: 'probe_latency', threshold: 200, duration_secs: 0, probe_task_id: 7 }), () => '电信')).toBe('探测延迟 > 200 ms（电信）')
})
test('ruleSummary flags a deleted bound task', () => {
  expect(ruleSummary(rule({ metric: 'probe_loss', op: 'lt', threshold: 5, probe_task_id: 9 }), () => undefined).endsWith('（绑定的任务已删除）')).toBe(true)
})
test('formatDuration', () => {
  expect(formatDuration(30)).toBe('30 秒')
  expect(formatDuration(300)).toBe('5 分钟')
  expect(formatDuration(5400)).toBe('1 小时 30 分钟')
})
