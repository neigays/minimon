import { expect, test } from 'vitest'
import { formatDuration, ruleBody, ruleFormFrom, rulePatch, ruleSummary, toDisplay, toStored, type AlertRule } from './alert'

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

test('rulePatch is empty when nothing changed', () => {
  const r = rule({ all_nodes: false, node_ids: [3, 1] })
  expect(rulePatch(ruleFormFrom(r), ruleFormFrom(r))).toEqual({})
})
test('rulePatch leaves an untouched rounded threshold alone', () => {
  const r = rule({ metric: 'net_rx_rate', threshold: 15_625 }) // 0.125 Mbps，显示为 0.13
  const seed = ruleFormFrom(r)
  expect(seed.threshold).toBe('0.13')
  const f = { ...ruleFormFrom(r), name: '新名字' }
  expect(rulePatch(seed, f)).toEqual({ name: '新名字' })
  expect(rulePatch(seed, { ...f, threshold: '0.5' })).toEqual({ name: '新名字', threshold: 62_500 })
})
test('rulePatch does not resend a deleted bound task', () => {
  const r = rule({ metric: 'probe_latency', threshold: 200, probe_task_id: 9 })
  const seed = ruleFormFrom(r)
  expect(rulePatch(seed, { ...ruleFormFrom(r), duration: '600' })).toEqual({ duration_secs: 600 })
  expect(rulePatch(seed, { ...ruleFormFrom(r), task: '' })).toEqual({ probe_task_id: null })
  expect(rulePatch(seed, { ...ruleFormFrom(r), task: '4' })).toEqual({ probe_task_id: 4 })
})
test('rulePatch handles metric and node scope changes', () => {
  const seed = ruleFormFrom(rule())
  expect(rulePatch(seed, { ...seed, metric: 'net_tx_rate', threshold: '90' })).toEqual({ metric: 'net_tx_rate', threshold: 11_250_000 })
  expect(rulePatch(seed, { ...seed, all: false, ids: [2] })).toEqual({ all_nodes: false, node_ids: [2] })
  const scoped = ruleFormFrom(rule({ all_nodes: false, node_ids: [1, 2] }))
  expect(rulePatch(scoped, { ...scoped, ids: [2, 1] })).toEqual({})
  expect(rulePatch(scoped, { ...scoped, ids: [2] })).toEqual({ node_ids: [2] })
  expect(rulePatch(scoped, { ...scoped, all: true })).toEqual({ all_nodes: true })
})
test('ruleBody sends the full body for create', () => {
  const f = { ...ruleFormFrom(rule({ metric: 'probe_loss', threshold: 5, probe_task_id: 3 })), name: ' x ' }
  expect(ruleBody(f)).toEqual({
    name: 'x', metric: 'probe_loss', op: 'gt', threshold: 5, duration_secs: 300, cooldown_secs: 1800,
    all_nodes: true, node_ids: [], probe_task_id: 3,
  })
  expect(ruleBody({ ...f, metric: 'cpu' }).probe_task_id).toBeNull()
})
