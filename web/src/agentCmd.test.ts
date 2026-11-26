import { expect, test } from 'vitest'
import { agentCommand, httpBase, installCommand, wsBase } from './agentCmd'

test('ws base', () => {
  expect(wsBase('https://m.example.com', { protocol: 'http:', host: 'x' })).toBe('wss://m.example.com')
  expect(wsBase('http://m.example.com/', { protocol: 'https:', host: 'x' })).toBe('ws://m.example.com')
  expect(wsBase(null, { protocol: 'http:', host: '127.0.0.1:25780' })).toBe('ws://127.0.0.1:25780')
  expect(wsBase(null, { protocol: 'https:', host: 'a.b' })).toBe('wss://a.b')
})
test('command', () => {
  expect(agentCommand('wss://a.b', 'ab12')).toBe('minimon-agent --server wss://a.b --token ab12')
})
test('http base', () => {
  expect(httpBase('https://m.example.com/', { protocol: 'http:', host: 'x' })).toBe('https://m.example.com')
  expect(httpBase(null, { protocol: 'http:', host: '1.2.3.4:25780' })).toBe('http://1.2.3.4:25780')
})
test('ws base trims trailing slash', () => {
  expect(wsBase('https://m.example.com/', { protocol: 'http:', host: 'x' })).toBe('wss://m.example.com')
})
test('install command', () => {
  expect(installCommand('https://m.example.com', 'wss://m.example.com', 'tok')).toBe(
    'curl -fsSL https://m.example.com/install.sh | sudo bash -s -- --server wss://m.example.com --token tok',
  )
})
