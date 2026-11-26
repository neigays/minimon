/** Radix 彩色色板，与服务端 `tags::COLORS` 一致；无后缀为 gray。 */
export const TAG_COLORS = [
  'gray', 'gold', 'bronze', 'brown', 'yellow', 'amber', 'orange', 'tomato', 'red', 'ruby',
  'crimson', 'pink', 'plum', 'purple', 'violet', 'iris', 'indigo', 'blue', 'cyan', 'teal',
  'jade', 'green', 'grass', 'lime', 'mint', 'sky',
] as const

export interface Tag {
  text: string
  color: string
  /** 仅预览：保存时会被服务端拒绝。 */
  invalid?: boolean
}

const MAX_TAGS = 10
const MAX_TEXT_CHARS = 24

/** 编辑时的实时预览：规则与服务端一致，非法的段标记 `invalid`（显示为 gray）。 */
export function previewTags(raw: string): Tag[] {
  const out: Tag[] = []
  for (const seg of raw.split(';')) {
    const s = seg.trim()
    if (!s) continue
    let text = s
    let color = 'gray'
    let bad = false
    const lt = s.lastIndexOf('<')
    if (s.endsWith('>') && lt >= 0) {
      text = s.slice(0, lt).trim()
      const c = s.slice(lt + 1, -1).trim().toLowerCase()
      if (!c || /[<>]/.test(c) || !(TAG_COLORS as readonly string[]).includes(c)) bad = true
      else color = c
    }
    if (!text || /[<>;]/.test(text) || [...text].length > MAX_TEXT_CHARS) bad = true
    if (out.length >= MAX_TAGS) bad = true
    out.push(bad ? { text: text || s, color: 'gray', invalid: true } : { text, color })
  }
  return out
}
