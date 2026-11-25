/** 流量按十进制单位（1 GB = 1e9 B）；网速、内存、磁盘仍用 format.ts 的 1024 进制。 */
const DEC = ['B', 'K', 'M', 'G', 'T', 'P']

export function formatDecBytes(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return '0'
  let i = 0
  let v = n
  while (v >= 1000 && i < DEC.length - 1) {
    v /= 1000
    i++
  }
  if (i === 0) return `${Math.round(v)}B`
  let t = Math.round(v * 10) / 10
  if (t >= 1000 && i < DEC.length - 1) {
    t = 1
    i++
  }
  return `${t}${DEC[i]}`
}

export function gbToBytes(v: number, unit: 'GB' | 'TB'): number {
  return Math.round(v * (unit === 'TB' ? 1e12 : 1e9))
}

export function bytesToQuotaInput(n: number): { value: number; unit: 'GB' | 'TB' } {
  if (n >= 1e12 && n % 1e12 === 0) return { value: n / 1e12, unit: 'TB' }
  return { value: Math.round((n / 1e9) * 100) / 100, unit: 'GB' }
}

export function quotaPct(used: number, quota: number | null): number {
  if (!quota || quota <= 0) return 0
  return Math.max(0, Math.min(100, (used / quota) * 100))
}

export function expireText(days: number | null | undefined): string {
  if (days === null || days === undefined) return ''
  if (days === 0) return '今天到期'
  return days > 0 ? `剩 ${days} 天` : `已过期 ${-days} 天`
}

export function expireLevel(days: number | null | undefined): '' | 'hi' | 'crit' {
  if (days === null || days === undefined) return ''
  if (days < 0) return 'crit'
  return days <= 7 ? 'hi' : ''
}

export type TrafficMode = 'rx' | 'tx' | 'sum' | 'max'
export const MODE_LABELS: Record<TrafficMode, string> = {
  rx: '只算下行',
  tx: '只算上行',
  sum: '上下行相加',
  max: '取较大值',
}

export interface Traffic {
  quota: number | null
  reset_day: number
  mode: TrafficMode
  used: number
  rx: number
  tx: number
  period_start_ms: number
  period_end_ms: number
}

/** 账期边界按服务器时区显示为 `M/D HH:mm`；时区缺省或无效时退回浏览器本地时间。 */
export function formatPeriodDay(ms: number, timeZone: string | null): string {
  const opts: Intl.DateTimeFormatOptions = { month: 'numeric', day: 'numeric', hour: '2-digit', minute: '2-digit', hourCycle: 'h23' }
  let f: Intl.DateTimeFormat
  try {
    f = new Intl.DateTimeFormat('en-US', timeZone ? { ...opts, timeZone } : opts)
  } catch {
    f = new Intl.DateTimeFormat('en-US', opts)
  }
  const m: Record<string, string> = {}
  for (const p of f.formatToParts(new Date(ms))) m[p.type] = p.value
  return `${m.month}/${m.day} ${m.hour}:${m.minute}`
}
