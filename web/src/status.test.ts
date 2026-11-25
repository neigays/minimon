import { expect, test } from 'vitest'
import { level, offlineFor, statusOf, usagePct } from './status'
import type { Report } from './live'

const rep = (o: Partial<Report>) => ({ cpu: 0, mem_used: 0, disk_used: 0, disk_total: 100, ...o }) as Report
const node = (o: object) => ({ mem_total: 100, disk_total: 100, ...o }) as any

test('levels', () => {
  expect(level(74.9)).toBe('')
  expect(level(75)).toBe('hi')
  expect(level(90)).toBe('crit')
})
test('status', () => {
  expect(statusOf(node({ online: true, latest: rep({ cpu: 95 }) }), true)).toBe('busy')
  expect(statusOf(node({ online: true, latest: rep({ cpu: 10 }) }), true)).toBe('online')
  expect(statusOf(node({ online: true, latest: rep({ cpu: 10 }) }), false)).toBe('offline')
  expect(statusOf(node({ online: false, latest: rep({ cpu: 99 }) }), true)).toBe('offline')
  expect(statusOf(node({ online: true, latest: rep({ mem_used: 90 }) }), true)).toBe('busy')
  expect(statusOf(node({ online: true, latest: rep({ disk_used: 90 }) }), true)).toBe('busy')
  expect(statusOf(node({ online: true, latest: null }), true)).toBe('online')
})
test('usagePct', () => {
  expect(usagePct(50, 200)).toBe(25)
  expect(usagePct(5, 0)).toBe(0)
  expect(usagePct(300, 200)).toBe(100)
})
test('offlineFor', () => {
  expect(offlineFor(0, 12 * 60e3)).toBe('12 分钟')
  expect(offlineFor(0, 3 * 3600e3)).toBe('3 小时')
  expect(offlineFor(0, 2 * 86400e3)).toBe('2 天')
  expect(offlineFor(0, 20e3)).toBe('不到 1 分钟')
})
