import { expect, test } from 'vitest'
import { applyMsg, isLive, liveUrl, type PublicNode, type Report } from './live'

const node = (id: number, online = false): PublicNode => ({
  id, name: `n${id}`, online, sort: 0, os: null, arch: null,
  cpu_cores: null, mem_total: null, swap_total: null, disk_total: null, latest: null,
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
