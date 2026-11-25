import { onScopeDispose, ref, type Ref } from 'vue'

export interface Report {
  ts_ms: number
  cpu: number
  mem_used: number
  swap_used: number
  disk_used: number
  disk_total: number
  net_rx_bytes: number
  net_tx_bytes: number
  net_rx_rate: number
  net_tx_rate: number
  load1: number
  load5: number
  load15: number
  uptime_secs: number
  process_count: number
  tcp_count: number
  udp_count: number
}

export interface PublicNode {
  id: number
  name: string
  online: boolean
  sort: number
  os: string | null
  arch: string | null
  cpu_cores: number | null
  mem_total: number | null
  swap_total: number | null
  disk_total: number | null
  latest: Report | null
}

export type LiveMsg =
  | { type: 'snapshot'; nodes: PublicNode[] }
  | { type: 'report'; node_id: number; report: Report }
  | { type: 'online'; node_id: number }
  | { type: 'offline'; node_id: number }

/** 幂等地把一条消息应用到节点列表；未知节点的增量消息被忽略。 */
export function applyMsg(nodes: PublicNode[], msg: LiveMsg): PublicNode[] {
  if (msg.type === 'snapshot') return msg.nodes
  return nodes.map((n) => {
    if (n.id !== msg.node_id) return n
    switch (msg.type) {
      case 'report':
        return { ...n, online: true, latest: msg.report }
      case 'online':
        return { ...n, online: true }
      case 'offline':
        return { ...n, online: false }
    }
  })
}

/** 断线时无法确认节点状态，不能显示为在线。 */
export function isLive(node: PublicNode, connected: boolean): boolean {
  return connected && node.online
}

export function liveUrl(loc: Pick<Location, 'protocol' | 'host'>): string {
  const proto = loc.protocol === 'https:' ? 'wss:' : 'ws:'
  return `${proto}//${loc.host}/api/live`
}

const RECONNECT_MS = 3000

export function useLive(): { nodes: Ref<PublicNode[]>; connected: Ref<boolean> } {
  const nodes = ref<PublicNode[]>([])
  const connected = ref(false)
  let ws: WebSocket | null = null
  let timer: ReturnType<typeof setTimeout> | undefined
  let stopped = false

  const connect = () => {
    ws = new WebSocket(liveUrl(location))
    ws.onopen = () => {
      connected.value = true
    }
    ws.onmessage = (ev) => {
      try {
        nodes.value = applyMsg(nodes.value, JSON.parse(ev.data as string) as LiveMsg)
      } catch {
        // 忽略无法解析的帧
      }
    }
    ws.onclose = () => {
      connected.value = false
      ws = null
      if (!stopped) timer = setTimeout(connect, RECONNECT_MS)
    }
    ws.onerror = () => ws?.close()
  }

  connect()
  onScopeDispose(() => {
    stopped = true
    clearTimeout(timer)
    ws?.close()
  })
  return { nodes, connected }
}
