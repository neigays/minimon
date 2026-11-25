export class ApiError extends Error {
  status: number
  constructor(status: number, message: string) {
    super(message)
    this.name = 'ApiError'
    this.status = status
  }
}

type UnauthorizedHandler = (path: string) => void
const handlers = new Set<UnauthorizedHandler>()

/** 订阅全局「需要登录」事件（任一接口返回 401 时触发）。返回取消订阅函数。 */
export function onUnauthorized(h: UnauthorizedHandler): () => void {
  handlers.add(h)
  return () => handlers.delete(h)
}

export async function api<T>(method: string, path: string, body?: unknown): Promise<T> {
  const init: RequestInit = { method, credentials: 'same-origin', headers: {} }
  // 所有修改类请求必须带 JSON Content-Type，否则 server 返回 415。
  if (method !== 'GET') {
    ;(init.headers as Record<string, string>)['Content-Type'] = 'application/json'
    init.body = JSON.stringify(body ?? {})
  }
  let res: Response
  try {
    res = await fetch(path, init)
  } catch {
    throw new ApiError(0, '无法连接服务器')
  }
  if (!res.ok) {
    let message = `请求失败（${res.status}）`
    try {
      const j = (await res.json()) as { error?: string }
      if (j.error) message = j.error
    } catch {
      // 响应不是 JSON，沿用默认文案
    }
    if (res.status === 401) handlers.forEach((h) => h(path))
    throw new ApiError(res.status, message)
  }
  if (res.status === 204) return undefined as T
  const text = await res.text()
  return (text ? JSON.parse(text) : undefined) as T
}
