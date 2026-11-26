import { expect, test } from 'vitest'
import { flagUrl } from './flag'

test('two letters map to a bundled svg', () => {
  expect(flagUrl('US')).toBe('/flags/us.svg')
  expect(flagUrl('jp')).toBe('/flags/jp.svg')
})
test('anything else yields empty (no path tricks)', () => {
  for (const s of ['', 'J', 'J1', 'JPN', '中国', '../', 'u/']) expect(flagUrl(s)).toBe('')
})
