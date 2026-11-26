import { expect, test } from 'vitest'
import { menuPlacement } from './menu'

const vp = { width: 1000, height: 800 }

test('opens below the trigger when there is room, right-aligned', () => {
  expect(menuPlacement({ top: 100, bottom: 124, right: 900 }, vp, 120)).toEqual({ position: 'fixed', right: '100px', top: '128px' })
})
test('opens upward near the bottom of the viewport', () => {
  expect(menuPlacement({ top: 740, bottom: 764, right: 900 }, vp, 120)).toEqual({ position: 'fixed', right: '100px', bottom: '64px' })
})
