import { bytesToQuotaInput, gbToBytes, type TrafficMode } from './traffic'

export interface NodeForm {
  name: string
  group: string
  region: string
  note: string
  quota: string
  unit: 'GB' | 'TB'
  resetDay: string
  mode: TrafficMode
  expire: string
  alertOffline: boolean
  tags: string
}

export const blankForm = (): NodeForm => ({ name: '', group: '', region: '', note: '', quota: '', unit: 'GB', resetDay: '1', mode: 'sum', expire: '', alertOffline: true, tags: '' })

/** 管理员节点里已存的配置（不是用量缓存 traffic）。 */
export interface NodeConfig {
  name: string
  group: string
  region: string
  note: string
  traffic_quota?: number | null
  traffic_reset_day?: number
  traffic_mode?: TrafficMode
  expire_date?: string | null
  alert_offline?: boolean
  tags_raw?: string
}

export function formFromNode(n: NodeConfig): NodeForm {
  const f = blankForm()
  Object.assign(f, { name: n.name, group: n.group, region: n.region, note: n.note })
  if (n.traffic_quota) {
    const q = bytesToQuotaInput(n.traffic_quota)
    f.quota = String(q.value)
    f.unit = q.unit
  }
  if (n.traffic_reset_day) f.resetDay = String(n.traffic_reset_day)
  if (n.traffic_mode) f.mode = n.traffic_mode
  f.expire = n.expire_date ?? ''
  f.alertOffline = n.alert_offline ?? true
  f.tags = n.tags_raw ?? ''
  return f
}

/** 额度字节数；留空为 null（不限）。调用方先校验输入合法。 */
export function quotaBytes(f: NodeForm): number | null {
  const q = f.quota.trim()
  return q ? gbToBytes(Number(q), f.unit) : null
}

/** 只包含相对 seed 有变化的字段；清空额度/到期日明确发送 null。 */
export function diffPatch(seed: NodeForm, form: NodeForm): Record<string, unknown> {
  const out: Record<string, unknown> = {}
  const name = form.name.trim()
  const group = form.group.trim()
  if (name !== seed.name) out.name = name
  if (group !== seed.group) out.group = group
  if (form.region !== seed.region) out.region = form.region
  if (form.note !== seed.note) out.note = form.note
  const q = quotaBytes(form)
  if (q !== quotaBytes(seed)) out.traffic_quota = q
  if (Number(form.resetDay) !== Number(seed.resetDay)) out.traffic_reset_day = Number(form.resetDay)
  if (form.mode !== seed.mode) out.traffic_mode = form.mode
  if (form.expire !== seed.expire) out.expire_date = form.expire || null
  if (form.alertOffline !== seed.alertOffline) out.alert_offline = form.alertOffline
  if (form.tags.trim() !== seed.tags.trim()) out.tags = form.tags.trim()
  return out
}
