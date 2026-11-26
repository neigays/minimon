import { expect, test } from 'vitest'
import { diffPatch, formFromNode } from './nodeForm'

const node = { name: 'a', group: '', region: 'HK', note: '', traffic_quota: 500e9, traffic_reset_day: 9, traffic_mode: 'max' as const, expire_date: '2027-01-01' }

test('unchanged form produces empty patch', () => {
  const seed = formFromNode(node)
  expect(diffPatch(seed, { ...seed })).toEqual({})
})
test('rename only does not touch billing', () => {
  const seed = formFromNode(node)
  expect(diffPatch(seed, { ...seed, name: ' b ' })).toEqual({ name: 'b' })
})
test('clearing quota and expire sends null', () => {
  const seed = formFromNode(node)
  expect(diffPatch(seed, { ...seed, quota: '', expire: '' })).toEqual({ traffic_quota: null, expire_date: null })
})
test('equivalent unit is not a change; real change is', () => {
  const seed = formFromNode(node)
  expect(diffPatch(seed, { ...seed, quota: '0.5', unit: 'TB' })).toEqual({})
  expect(diffPatch(seed, { ...seed, quota: '1', unit: 'TB', resetDay: '31', mode: 'rx' })).toEqual({ traffic_quota: 1e12, traffic_reset_day: 31, traffic_mode: 'rx' })
})
test('node without config uses defaults', () => {
  const seed = formFromNode({ name: 'a', group: '', region: '', note: '' })
  expect(seed).toMatchObject({ quota: '', resetDay: '1', mode: 'sum', expire: '' })
})
test('alert_offline defaults to true and only that change is patched', () => {
  const seed = formFromNode(node)
  expect(seed.alertOffline).toBe(true)
  expect(formFromNode({ ...node, alert_offline: false }).alertOffline).toBe(false)
  expect(diffPatch(seed, { ...seed, alertOffline: false })).toEqual({ alert_offline: false })
})
test('tags come from tags_raw and only changed tags are patched', () => {
  const seed = formFromNode({ ...node, tags_raw: 'BGP' })
  expect(seed.tags).toBe('BGP')
  expect(formFromNode(node).tags).toBe('')
  expect(diffPatch(seed, { ...seed })).toEqual({})
  expect(diffPatch(seed, { ...seed, tags: 'BGP;1G<red>' })).toEqual({ tags: 'BGP;1G<red>' })
})
