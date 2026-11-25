import { expect, test } from 'vitest'
import { guard, loginLocation, safeRedirect } from './router'

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

test('safeRedirect only allows same-origin paths', () => {
  expect(safeRedirect('/admin/settings')).toBe('/admin/settings')
  expect(safeRedirect('/node/3?x=1')).toBe('/node/3?x=1')
  expect(safeRedirect('//evil.com')).toBe('/')
  expect(safeRedirect('/\\evil.com')).toBe('/')
  expect(safeRedirect('https://evil.com')).toBe('/')
  expect(safeRedirect('javascript:alert(1)')).toBe('/')
  expect(safeRedirect(undefined)).toBe('/')
  expect(safeRedirect(['/admin'])).toBe('/admin')
  expect(safeRedirect('/login')).toBe('/')
})
test('loginLocation', () => {
  expect(loginLocation('/')).toEqual({ path: '/login' })
  expect(loginLocation('/admin')).toEqual({ path: '/login', query: { redirect: '/admin' } })
})
