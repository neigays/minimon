/** agent 连接地址：优先用配置的 public_url（http→ws，https→wss），否则按当前页面地址推断。 */
export function wsBase(publicUrl: string | null, location: { protocol: string; host: string }): string {
  if (publicUrl) {
    const u = publicUrl.replace(/\/+$/, '')
    if (u.startsWith('https://')) return 'wss://' + u.slice('https://'.length)
    if (u.startsWith('http://')) return 'ws://' + u.slice('http://'.length)
    return u
  }
  return `${location.protocol === 'https:' ? 'wss:' : 'ws:'}//${location.host}`
}

export function agentCommand(base: string, token: string): string {
  return `minimon-agent --server ${base} --token ${token}`
}
