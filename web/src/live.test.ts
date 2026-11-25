import { expect, test } from 'vitest'
import { applyMsg, isLive, liveUrl, loginRequiredAfterGuestSuccess, viewerChanged, type PublicNode, type Report } from './live'

const node = (id: number, online = false): PublicNode => ({
  id, name: `n${id}`, online, sort: 0, group: '', region: '', os: null, arch: null,
  cpu_model: null, cpu_cores: null, virtualization: null, mem_total: null, swap_total: null,
  disk_total: null, last_seen_ms: null, latest: null, probes: [],
})
const report = { cpu: 5 } as Report

test('snapshot replaces', () => {
  expect(applyMsg([node(9)], { type: 'snapshot', nodes: [node(1), node(2)] }).map((n) => n.id)).toEqual([1, 2])
})
test('report marks online and stores latest', () => {
  const r = applyMsg([node(1)], { type: 'report', node_id: 1, report })
  expect(r[0].online).toBe(true)
  expect(r[0].latest?.cpu).toBe(5)
})
test('online/offline idempotent, unknown ignored', () => {
  let s = [node(1)]
  s = applyMsg(applyMsg(s, { type: 'online', node_id: 1 }), { type: 'online', node_id: 1 })
  expect(s[0].online).toBe(true)
  s = applyMsg(applyMsg(s, { type: 'offline', node_id: 1 }), { type: 'offline', node_id: 1 })
  expect(s[0].online).toBe(false)
  expect(applyMsg(s, { type: 'online', node_id: 42 })).toEqual(s)
})
test('ws url', () => {
  expect(liveUrl({ protocol: 'https:', host: 'a.b:1' })).toBe('wss://a.b:1/api/live')
  expect(liveUrl({ protocol: 'http:', host: 'x' })).toBe('ws://x/api/live')
})
test('disconnected hides liveness', () => {
  expect(isLive(node(1, true), true)).toBe(true)
  expect(isLive(node(1, true), false)).toBe(false)
  expect(isLive(node(1, false), true)).toBe(false)
})

test('viewer change detection', () => {
  expect(viewerChanged(null, 'admin')).toBe(true)
  expect(viewerChanged('admin', null)).toBe(true)
  expect(viewerChanged('admin', 'admin')).toBe(false)
  expect(viewerChanged(null, null)).toBe(false)
})
test('guest success clears loginRequired', () => {
  expect(loginRequiredAfterGuestSuccess()).toBe(false)
})

test('probe and traffic messages', () => {
  const n = { ...node(1), traffic: { quota: null, reset_day: 1, mode: 'sum', used: 1, rx: 1, tx: 0, period_start_ms: 0, period_end_ms: 9 } } as PublicNode
  const p = applyMsg([n], { type: 'probe', node_id: 1, task_id: 3, name: '移动', avg_ms: 5, loss: 0, error: null })
  expect(p[0].probes[0].name).toBe('移动')
  const t = applyMsg([n], { type: 'traffic', node_id: 1, used: 50, rx: 30, tx: 20, quota: 100, period_start_ms: 1, period_end_ms: 2 })
  expect(t[0].traffic).toMatchObject({ used: 50, quota: 100, reset_day: 1, mode: 'sum', period_end_ms: 2 })
  const built = applyMsg([{ ...node(1), traffic_reset_day: 9, traffic_mode: 'max' } as PublicNode], { type: 'traffic', node_id: 1, used: 7, rx: 4, tx: 3, quota: 100, period_start_ms: 0, period_end_ms: 1 })[0].traffic
  expect(built).toEqual({ quota: 100, reset_day: 9, mode: 'max', used: 7, rx: 4, tx: 3, period_start_ms: 0, period_end_ms: 1 })
  expect(applyMsg([node(1)], { type: 'traffic', node_id: 1, used: 1, rx: 1, tx: 0, quota: null, period_start_ms: 0, period_end_ms: 1 })[0].traffic).toMatchObject({ reset_day: 1, mode: 'sum', used: 1 })
})
