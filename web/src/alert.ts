export type Metric = 'cpu' | 'mem' | 'swap' | 'disk' | 'load1' | 'net_rx_rate' | 'net_tx_rate' | 'probe_latency' | 'probe_loss'
export type Op = 'gt' | 'lt'

export interface AlertRule {
  id: number
  name: string
  enabled: boolean
  metric: Metric
  op: Op
  /** 规范单位：百分数 0–100、负载原值、网速为字节/秒、延迟为毫秒。 */
  threshold: number
  duration_secs: number
  cooldown_secs: number
  all_nodes: boolean
  node_ids: number[]
  probe_task_id: number | null
}

export interface AlertSettings {
  offline_enabled: boolean
  offline_grace_secs: number
  traffic_enabled: boolean
  expire_enabled: boolean
  sendkey_set: boolean
  sendkey_tail: string | null
  push_tags: string
}

export type EventKind = 'firing' | 'resolved' | 'traffic' | 'expire' | 'test'
export type SentState = 'pending' | 'ok' | 'failed' | 'skipped'

export interface AlertEvent {
  id: number
  ts_ms: number
  node_id: number | null
  node_name: string | null
  kind: EventKind
  title: string
  body: string
  sent: SentState
}

export interface ActiveAlert {
  kind: string
  rule_id: number
  rule_name: string | null
  node_id: number
  node_name: string | null
  task_id: number | null
  task_name: string | null
  since_ms: number
  fired_ms: number | null
  value: number | null
  value_text: string | null
}

export type Unit = '%' | 'Mbps' | 'ms' | ''

export const METRICS: { value: Metric; label: string; unit: Unit; probe: boolean }[] = [
  { value: 'cpu', label: 'CPU', unit: '%', probe: false },
  { value: 'mem', label: '内存', unit: '%', probe: false },
  { value: 'swap', label: 'Swap', unit: '%', probe: false },
  { value: 'disk', label: '磁盘', unit: '%', probe: false },
  { value: 'load1', label: '负载', unit: '', probe: false },
  { value: 'net_rx_rate', label: '下行速率', unit: 'Mbps', probe: false },
  { value: 'net_tx_rate', label: '上行速率', unit: 'Mbps', probe: false },
  { value: 'probe_latency', label: '探测延迟', unit: 'ms', probe: true },
  { value: 'probe_loss', label: '探测丢包', unit: '%', probe: true },
]

const BYTES_PER_MBPS = 125_000
const isNet = (m: Metric) => m === 'net_rx_rate' || m === 'net_tx_rate'

export const toStored = (m: Metric, display: number): number => (isNet(m) ? display * BYTES_PER_MBPS : display)
export const toDisplay = (m: Metric, stored: number): number => (isNet(m) ? stored / BYTES_PER_MBPS : stored)

export function formatDuration(secs: number): string {
  const s = Math.max(0, Math.round(secs))
  if (s < 60) return `${s} 秒`
  const h = Math.floor(s / 3600)
  const m = Math.floor((s % 3600) / 60)
  const rest = s % 60
  const parts: string[] = []
  if (h) parts.push(`${h} 小时`)
  if (m) parts.push(`${m} 分钟`)
  if (rest) parts.push(`${rest} 秒`)
  return parts.join(' ')
}

export function ruleSummary(r: AlertRule, taskName?: (id: number) => string | undefined): string {
  const meta = METRICS.find((m) => m.value === r.metric)
  const unit = meta?.unit ?? ''
  const num = Number(toDisplay(r.metric, r.threshold).toFixed(2))
  const u = unit === '' || unit === '%' ? unit : ` ${unit}`
  let s = `${meta?.label ?? r.metric} ${r.op === 'gt' ? '>' : '<'} ${num}${u}`
  if (r.duration_secs > 0) s += ` 持续 ${formatDuration(r.duration_secs)}`
  if (r.probe_task_id != null) {
    const n = taskName?.(r.probe_task_id)
    s += n === undefined ? '（绑定的任务已删除）' : `（${n}）`
  }
  return s
}

/** 事件时间：按全局时区显示；时区无效时退回浏览器本地时区。 */
export function formatEventTime(ms: number, timeZone: string | null): string {
  const opts: Intl.DateTimeFormatOptions = { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', second: '2-digit', hourCycle: 'h23' }
  let f: Intl.DateTimeFormat
  try {
    f = new Intl.DateTimeFormat('zh-CN', timeZone ? { ...opts, timeZone } : opts)
  } catch {
    f = new Intl.DateTimeFormat('zh-CN', opts)
  }
  return f.format(new Date(ms))
}

export const SENT_LABELS: Record<SentState, string> = { ok: '已发送', failed: '失败', skipped: '未配置', pending: '发送中' }

/** 规则对话框的表单状态（数值字段保留用户输入的原始字符串）。 */
export interface RuleForm {
  name: string
  metric: Metric
  op: Op
  threshold: string
  duration: string
  cooldown: string
  all: boolean
  ids: number[]
  task: string
}

export const blankRuleForm = (): RuleForm => ({ name: '', metric: 'cpu', op: 'gt', threshold: '90', duration: '300', cooldown: '1800', all: true, ids: [], task: '' })

export function ruleFormFrom(r: AlertRule): RuleForm {
  return {
    name: r.name, metric: r.metric, op: r.op, threshold: String(Number(toDisplay(r.metric, r.threshold).toFixed(2))),
    duration: String(r.duration_secs), cooldown: String(r.cooldown_secs), all: r.all_nodes, ids: [...r.node_ids],
    task: r.probe_task_id == null ? '' : String(r.probe_task_id),
  }
}

const isProbe = (m: Metric) => METRICS.find((x) => x.value === m)?.probe ?? false
const boundTask = (f: RuleForm): number | null => (isProbe(f.metric) && f.task ? Number(f.task) : null)
const sameIds = (a: number[], b: number[]) => {
  const x = [...a].sort((p, q) => p - q)
  const y = [...b].sort((p, q) => p - q)
  return x.length === y.length && x.every((v, i) => v === y[i])
}

/** 新建规则的完整请求体。调用方先校验输入合法。 */
export function ruleBody(f: RuleForm): Record<string, unknown> {
  return {
    name: f.name.trim(), metric: f.metric, op: f.op, threshold: toStored(f.metric, Number(f.threshold)),
    duration_secs: Number(f.duration), cooldown_secs: Number(f.cooldown), all_nodes: f.all, node_ids: f.all ? [] : f.ids,
    probe_task_id: boundTask(f),
  }
}

/**
 * 编辑规则时只发送相对 seed 有变化的字段。
 * 阈值按输入框原文比较：未改动时不回写，避免显示取整（如 0.125 → 0.13 Mbps）改变已存值；
 * 绑定的任务未改动时也不发送，已删除任务的规则仍可编辑其它字段。
 */
export function rulePatch(seed: RuleForm, f: RuleForm): Record<string, unknown> {
  const out: Record<string, unknown> = {}
  const name = f.name.trim()
  if (name !== seed.name) out.name = name
  if (f.metric !== seed.metric) out.metric = f.metric
  if (f.op !== seed.op) out.op = f.op
  // 指标变化时单位可能不同，阈值需按新指标重新换算。
  if (f.threshold.trim() !== seed.threshold.trim() || f.metric !== seed.metric) out.threshold = toStored(f.metric, Number(f.threshold))
  if (Number(f.duration) !== Number(seed.duration)) out.duration_secs = Number(f.duration)
  if (Number(f.cooldown) !== Number(seed.cooldown)) out.cooldown_secs = Number(f.cooldown)
  if (f.all !== seed.all) out.all_nodes = f.all
  if (!f.all && (f.all !== seed.all || !sameIds(f.ids, seed.ids))) out.node_ids = f.ids
  const task = boundTask(f)
  if (task !== boundTask(seed)) out.probe_task_id = task
  return out
}
