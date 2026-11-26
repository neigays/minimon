import { expect, test } from 'vitest'
import { previewTags } from './tags'

test('parses text and colors', () => {
  expect(previewTags('1Gbps<yellow>;CN2 GIA<red>')).toEqual([
    { text: '1Gbps', color: 'yellow' },
    { text: 'CN2 GIA', color: 'red' },
  ])
  expect(previewTags('BGP')).toEqual([{ text: 'BGP', color: 'gray' }])
})
test('blank segments are ignored', () => {
  expect(previewTags(' ; ')).toEqual([])
  expect(previewTags('')).toEqual([])
})
test('unknown color previews as gray; case and spaces tolerated', () => {
  expect(previewTags('x<rainbow>')).toEqual([{ text: 'x', color: 'gray', invalid: true }])
  expect(previewTags('1G< Red >')).toEqual([{ text: '1G', color: 'red' }])
})

test('invalid segments are flagged for preview', () => {
  expect(previewTags('x<rainbow>')[0].invalid).toBe(true)
  expect(previewTags('a<b<red>')[0].invalid).toBe(true)
  expect(previewTags('<red>')[0].invalid).toBe(true)
  expect(previewTags('x'.repeat(25))[0].invalid).toBe(true)
  expect(previewTags('1G<red>')[0].invalid).toBeFalsy()
  const many = previewTags(Array.from({ length: 11 }, (_, i) => `t${i}`).join(';'))
  expect(many[9].invalid).toBeFalsy()
  expect(many[10].invalid).toBe(true)
})
