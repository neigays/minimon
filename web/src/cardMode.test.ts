import { expect, test } from 'vitest'
import { cardMode } from './cardMode'
import type { PublicNode } from './live'

const report = { ts_ms: 1, cpu: 5, mem_used: 1, swap_used: 0, disk_used: 1, disk_total: 10, net_rx_bytes: 0, net_tx_bytes: 0, net_rx_rate: 0, net_tx_rate: 0, load1: 0, load5: 0, load15: 0, uptime_secs: 1, process_count: 1, tcp_count: 0, udp_count: 0 }
const node = (p: Partial<PublicNode>): PublicNode => ({
  id: 1, name: 'n', tags: [], online: true, sort: 0, group: '', region: '', os: 'linux', arch: null, cpu_model: null, cpu_cores: null,
  virtualization: null, mem_total: 100, swap_total: null, disk_total: null, last_seen_ms: null, latest: report, probes: [], ...p,
})

test('online with a report is live', () => {
  expect(cardMode(node({}), true)).toBe('live')
})
test('offline or disconnected with data is stale', () => {
  expect(cardMode(node({ online: false }), true)).toBe('stale')
  expect(cardMode(node({}), false)).toBe('stale')
  expect(cardMode(node({ online: false, latest: null }), true)).toBe('stale')
})
test('never reported anything is never', () => {
  expect(cardMode(node({ latest: null, os: null }), true)).toBe('never')
  expect(cardMode(node({ latest: null, os: null, online: false }), false)).toBe('never')
})
test('online with system info but no report yet is waiting, not offline', () => {
  expect(cardMode(node({ latest: null }), true)).toBe('waiting')
  expect(cardMode(node({ latest: null }), false)).toBe('stale')
})
