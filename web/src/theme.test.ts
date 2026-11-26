import { expect, test } from 'vitest'
import { themeColors } from './theme'

test('without a manual choice the status bar follows the system theme', () => {
  expect(themeColors(null)).toEqual({ light: '#ffffff', dark: '#1b2336' })
})
test('a manual choice wins regardless of the system theme', () => {
  expect(themeColors('dark')).toEqual({ light: '#1b2336', dark: '#1b2336' })
  expect(themeColors('light')).toEqual({ light: '#ffffff', dark: '#ffffff' })
})
