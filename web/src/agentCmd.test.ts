import { expect, test } from 'vitest'
import { agentCommand, wsBase } from './agentCmd'

test('ws base', () => {
  expect(wsBase('https://m.example.com', { protocol: 'http:', host: 'x' })).toBe('wss://m.example.com')
  expect(wsBase('http://m.example.com/', { protocol: 'https:', host: 'x' })).toBe('ws://m.example.com')
  expect(wsBase(null, { protocol: 'http:', host: '127.0.0.1:25780' })).toBe('ws://127.0.0.1:25780')
  expect(wsBase(null, { protocol: 'https:', host: 'a.b' })).toBe('wss://a.b')
})
test('command', () => {
  expect(agentCommand('wss://a.b', 'ab12')).toBe('minimon-agent --server wss://a.b --token ab12')
})
