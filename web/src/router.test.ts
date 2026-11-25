import { expect, test } from 'vitest'
import { guard } from './router'

test('guard', () => {
  expect(guard('/admin', false, false)).toBe('/login')
  expect(guard('/admin/settings', false, false)).toBe('/login')
  expect(guard('/', false, true)).toBe('/login')
  expect(guard('/node/3', false, true)).toBe('/login')
  expect(guard('/', false, false)).toBeNull()
  expect(guard('/node/3', false, false)).toBeNull()
  expect(guard('/login', false, true)).toBeNull()
  expect(guard('/admin/settings', true, false)).toBeNull()
  expect(guard('/login', true, false)).toBe('/')
})
