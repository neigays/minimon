import { expect, test } from 'vitest'
import { flagEmoji } from './flag'

test('two letters become a regional-indicator flag', () => {
  expect(flagEmoji('jp')).toBe('🇯🇵')
  expect(flagEmoji('JP')).toBe('🇯🇵')
  expect(flagEmoji('us')).toBe('🇺🇸')
})
test('anything else yields empty', () => {
  for (const s of ['', 'J', 'J1', 'JPN', '中国']) expect(flagEmoji(s)).toBe('')
})
