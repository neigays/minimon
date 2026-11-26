import { expect, test } from 'vitest'
import { ipRows } from './ip'

test('public addresses reported by the agent win over the connection address', () => {
  expect(ipRows({ ipv4: '203.0.113.5', ipv6: '2001:db8::5', ip: '192.168.158.1' })).toEqual([
    ['IPv4', '203.0.113.5'],
    ['IPv6', '2001:db8::5'],
  ])
  expect(ipRows({ ipv4: '203.0.113.5', ipv6: null, ip: '192.168.158.1' })).toEqual([['IPv4', '203.0.113.5']])
})
test('old agents fall back to the connection address', () => {
  expect(ipRows({ ipv4: null, ipv6: null, ip: '198.51.100.7' })).toEqual([['IP', '198.51.100.7']])
  expect(ipRows({})).toEqual([['IP', '—']])
})
