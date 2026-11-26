import { expect, test } from 'vitest'
import { bytesToQuotaInput, expireLevel, expireText, formatDecBytes, formatPeriodDay, gbToBytes, quotaPct, trafficBar } from './traffic'

test('decimal traffic', () => {
  expect(formatDecBytes(126_400_000_000)).toBe('126.4G')
  expect(formatDecBytes(1e12)).toBe('1T')
  expect(formatDecBytes(0)).toBe('0')
  expect(formatDecBytes(512)).toBe('512B')
  expect(formatDecBytes(1.5e6)).toBe('1.5M')
  expect(formatDecBytes(999_960_000_000)).toBe('1T')
  expect(gbToBytes(1, 'TB')).toBe(1e12)
  expect(gbToBytes(500, 'GB')).toBe(500e9)
  expect(bytesToQuotaInput(500e9)).toEqual({ value: 500, unit: 'GB' })
  expect(bytesToQuotaInput(2e12)).toEqual({ value: 2, unit: 'TB' })
  expect(bytesToQuotaInput(1.5e12)).toEqual({ value: 1500, unit: 'GB' })
})

test('quotaPct', () => {
  expect(quotaPct(50, 100)).toBe(50)
  expect(quotaPct(150, 100)).toBe(100)
  expect(quotaPct(1, null)).toBe(0)
})

test('expire', () => {
  expect(expireText(96)).toBe('剩 96 天')
  expect(expireText(0)).toBe('今天到期')
  expect(expireText(-3)).toBe('已过期 3 天')
  expect(expireText(null)).toBe('')
  expect(expireLevel(8)).toBe('')
  expect(expireLevel(7)).toBe('hi')
  expect(expireLevel(0)).toBe('hi')
  expect(expireLevel(-1)).toBe('crit')
  expect(expireLevel(null)).toBe('')
})

test('formatPeriodDay uses the given timezone', () => {
  // 2026-02-28T16:00:00Z = 上海 3/1 00:00 = 纽约 2/28 11:00
  const ms = Date.UTC(2026, 1, 28, 16, 0)
  expect(formatPeriodDay(ms, 'Asia/Shanghai')).toBe('3/1 00:00')
  expect(formatPeriodDay(ms, 'America/New_York')).toBe('2/28 11:00')
  expect(formatPeriodDay(ms, 'UTC')).toBe('2/28 16:00')
  expect(formatPeriodDay(ms, 'Mars/X')).toMatch(/^\d+\/\d+ \d\d:\d\d$/)
})

test('trafficBar: visible minimum and <1% label', () => {
  expect(trafficBar(0, 1e12)).toEqual({ width: '0%', label: '0%' })
  const tiny = trafficBar(66e6, 1e12)
  expect(tiny.label).toBe('<1%')
  expect(tiny.width).toContain('2px')
  expect(trafficBar(5e11, 1e12)).toEqual({ width: '50.0%', label: '50%' })
  expect(trafficBar(2e12, 1e12).label).toBe('100%')
})
