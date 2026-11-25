import { ref } from 'vue'
import { api } from './api'
import { useAuth } from './auth'

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
  group: string
  region: string
  os: string | null
  arch: string | null
  cpu_model: string | null
  cpu_cores: number | null
  virtualization: string | null
  mem_total: number | null
  swap_total: number | null
  disk_total: number | null
  last_seen_ms: number | null
  /** 最后已知上报：离线时保留，从未上报过为 null。 */
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

// 全局单例：顶栏与各页面共用同一条实时连接。
const nodes = ref<PublicNode[]>([])
const connected = ref(false)
const loaded = ref(false)
const lastMsgMs = ref(0)
const now = ref(Date.now())
let ws: WebSocket | null = null
let timer: ReturnType<typeof setTimeout> | undefined
let started = false

function connect() {
  const { user, loginRequired } = useAuth()
  const sock = new WebSocket(liveUrl(location))
  ws = sock
  sock.onopen = () => {
    connected.value = true
  }
  sock.onmessage = (ev) => {
    try {
      nodes.value = applyMsg(nodes.value, JSON.parse(ev.data as string) as LiveMsg)
      loaded.value = true
      lastMsgMs.value = Date.now()
    } catch {
      // 忽略无法解析的帧
    }
  }
  sock.onclose = async () => {
    if (ws !== sock) return
    connected.value = false
    ws = null
    // 关闭可能是 server 要求登录：探测一次，401 会触发全局事件并置 loginRequired。
    try {
      await api('GET', '/api/nodes')
    } catch {
      // 失败原因由 api 层处理
    }
    if (loginRequired.value && !user.value) {
      nodes.value = []
      loaded.value = false
      return // 登录成功后由 restartLive 重连
    }
    timer = setTimeout(connect, RECONNECT_MS)
  }
  sock.onerror = () => sock.close()
}

/** 登录/退出后重建连接，使服务端按新身份推送。 */
export function restartLive() {
  clearTimeout(timer)
  const old = ws
  ws = null
  old?.close()
  connected.value = false
  connect()
}

export function useLive() {
  if (!started) {
    started = true
    setInterval(() => (now.value = Date.now()), 1000)
    connect()
  }
  return { nodes, connected, loaded, lastMsgMs, now }
}
