import type { PublicNode } from './live'
import { statusOf } from './status'

/**
 * 卡片显示模式：
 * - live：在线且有上报；
 * - stale：离线 / 与服务器断线，但有最后数据或系统信息（灰显最后数据）；
 * - never：从未上报过任何东西（刚创建、agent 还没装）。
 */
export function cardMode(n: PublicNode, connected: boolean): 'live' | 'stale' | 'never' {
  if (!n.latest && !n.os) return 'never'
  if (n.latest && statusOf(n, connected) !== 'offline') return 'live'
  return 'stale'
}
