/** 管理员看到的 IP 行：agent 上报的公网地址优先；旧 agent 没上报时回落到连接来源地址（Docker / 反代后可能是内网地址）。 */
export function ipRows(n: { ipv4?: string | null; ipv6?: string | null; ip?: string | null }): [string, string][] {
  const rows: [string, string][] = []
  if (n.ipv4) rows.push(['IPv4', n.ipv4])
  if (n.ipv6) rows.push(['IPv6', n.ipv6])
  if (rows.length === 0) rows.push(['IP', n.ip || '—'])
  return rows
}
