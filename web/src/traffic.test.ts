import { expect, test } from 'vitest'
import { bytesToQuotaInput, expireLevel, expireText, formatDecBytes, gbToBytes, quotaPct } from './traffic'

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
