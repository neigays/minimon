import { expect, test } from 'vitest'
import { formatBytes, formatRate, formatUptime } from './format'

test('bytes', () => {
  expect(formatBytes(0)).toBe('0 B')
  expect(formatBytes(1536)).toBe('1.5 KiB')
  expect(formatBytes(5 * 1024 ** 3)).toBe('5.0 GiB')
})
test('rate', () => {
  expect(formatRate(2048)).toBe('2.0 KiB/s')
})
test('uptime', () => {
  expect(formatUptime(59)).toBe('59秒')
  expect(formatUptime(90061)).toBe('1天 1小时')
})
