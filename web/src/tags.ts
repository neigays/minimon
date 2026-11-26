/** Radix 彩色色板，与服务端 `tags::COLORS` 一致；无后缀为 gray。 */
export const TAG_COLORS = [
  'gray', 'gold', 'bronze', 'brown', 'yellow', 'amber', 'orange', 'tomato', 'red', 'ruby',
  'crimson', 'pink', 'plum', 'purple', 'violet', 'iris', 'indigo', 'blue', 'cyan', 'teal',
  'jade', 'green', 'grass', 'lime', 'mint', 'sky',
] as const

export interface Tag {
  text: string
  color: string
}

/** 编辑时的实时预览：宽松解析，未知颜色显示为 gray；最终以服务端校验为准。 */
export function previewTags(raw: string): Tag[] {
  const out: Tag[] = []
  for (const seg of raw.split(';')) {
    const s = seg.trim()
    if (!s) continue
    const m = /^(.*)<([^<>]*)>$/.exec(s)
    const text = (m ? m[1] : s).trim()
    if (!text) continue
    const c = m ? m[2].trim().toLowerCase() : 'gray'
    out.push({ text, color: (TAG_COLORS as readonly string[]).includes(c) ? c : 'gray' })
  }
  return out
}
