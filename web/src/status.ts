import type { PublicNode } from './live'

export type Status = 'online' | 'busy' | 'offline'

export function usagePct(used: number, total: number | null | undefined): number {
  if (!total || total <= 0) return 0
  return Math.max(0, Math.min(100, (used / total) * 100))
}

export function level(p: number): '' | 'hi' | 'crit' {
  return p >= 90 ? 'crit' : p >= 75 ? 'hi' : ''
}

/** 与 server 断线时无法确认节点状态，一律按离线样式显示（沿用 M1 isLive 语义）。 */
export function statusOf(n: PublicNode, connected: boolean): Status {
  if (!connected || !n.online) return 'offline'
  const r = n.latest
  if (r) {
    const worst = Math.max(r.cpu, usagePct(r.mem_used, n.mem_total), usagePct(r.disk_used, r.disk_total))
    if (worst >= 90) return 'busy'
  }
  return 'online'
}

export function offlineFor(lastSeenMs: number, nowMs: number): string {
  const mins = Math.floor(Math.max(0, nowMs - lastSeenMs) / 60000)
  if (mins < 1) return '不到 1 分钟'
  if (mins < 60) return `${mins} 分钟`
  const hours = Math.floor(mins / 60)
  if (hours < 24) return `${hours} 小时`
  return `${Math.floor(hours / 24)} 天`
}
