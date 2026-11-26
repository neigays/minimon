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
  expect(previewTags('x<rainbow>')).toEqual([{ text: 'x', color: 'gray' }])
  expect(previewTags('1G< Red >')).toEqual([{ text: '1G', color: 'red' }])
})
