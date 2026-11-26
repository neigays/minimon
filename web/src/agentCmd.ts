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

/** 面板 HTTP 基址：优先 public_url（去尾 `/`），否则按当前页面地址。 */
export function httpBase(publicUrl: string | null, location: { protocol: string; host: string }): string {
  if (publicUrl) return publicUrl.replace(/\/+$/, '')
  return `${location.protocol}//${location.host}`
}

/** 一键安装命令：从面板下载 install.sh 并以 root 执行。 */
export function installCommand(httpBase: string, wsBase: string, token: string): string {
  return `curl -fsSL ${httpBase}/install.sh | sudo bash -s -- --server ${wsBase} --token ${token}`
}
